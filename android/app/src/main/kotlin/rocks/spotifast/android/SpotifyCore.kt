package rocks.spotifast.android

import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONArray
import rocks.spotifast.android.service.PlaybackService
import rocks.spotifast.android.util.vibrateTick
import uniffi.spotifast.CoreHandle
import uniffi.spotifast.PlaybackStateListener

/** The sign-in state shown by the UI, mirrored from the core's auth events. */
sealed interface AuthState {
    data object SignedOut : AuthState
    data class WaitingForBrowser(val url: String) : AuthState
    data object Connecting : AuthState
    data class Connected(val username: String) : AuthState
    data class Failed(val message: String) : AuthState
}

/** One playlist in the library list. */
data class LibraryItem(val id: String, val name: String, val artUrl: String?)

/** One row of the account's playlist tree — a playlist, or a folder
 *  holding more rows (playlists and, in principle, nested folders, though
 *  Spotify's own apps only ever create one level deep). Built from the
 *  session's rootlist, not the flat Web API playlist list, since folders
 *  have no public REST representation. */
sealed interface LibraryNode {
    data class PlaylistNode(val item: LibraryItem) : LibraryNode
    data class FolderNode(val id: String, val name: String, val children: List<LibraryNode>) : LibraryNode
}

/** One track in a playlist. [addedAt] is Spotify's own per-track timestamp
 *  (ISO-8601, UTC) for a real playlist or Liked Songs row; null for search,
 *  top-tracks, and queue rows, which carry no such timestamp. [artistId]/
 *  [albumId] back the track context menu's "View artist"/"View album"
 *  entries; either is null when the track is an episode, or — for
 *  [albumId] on an album's own track list — points back at the album
 *  already on screen. */
data class TrackItem(
    val uri: String,
    val name: String,
    val artist: String,
    val durationMs: Long,
    val artUrl: String?,
    val addedAt: String? = null,
    val artistId: String? = null,
    val albumId: String? = null,
)

/** One artist, from a search or the account's top artists. */
data class ArtistItem(val id: String, val name: String, val artUrl: String?)

/** The rest of an artist's Spotify profile — `on_artist_info`'s companion,
 *  for the info view opened by tapping the artist's picture. [images] is
 *  widest first; Spotify's artist images are usually just a few
 *  resolutions of the one photo, not distinct pictures. */
data class ArtistDetails(
    val id: String,
    val followers: Long,
    val genres: List<String>,
    val popularity: Int?,
    val images: List<String>,
)

/** One album, from a search. */
data class AlbumItem(val id: String, val name: String, val artist: String, val artUrl: String?)

/** What kind of library entity a [RecentEntry] card represents. */
enum class RecentKind { Playlist, Artist, Album }

/** One card in Home's recently-played feed — a playlist, artist, or album a
 *  track was actually played from (or, for playlists, that Spotify's own
 *  play history reports from any device). Newest [timestampMs] first. */
data class RecentEntry(
    val kind: RecentKind,
    val id: String,
    val name: String,
    val subtitle: String,
    val artUrl: String?,
    val timestampMs: Long,
)

/** Where a [SpotifyCore.playTracks] call originated, so Home's recently-
 *  played feed can record it. Omitting it (the default) means there's
 *  nothing worth remembering — a search result or the queue, not a
 *  playlist/artist/album view. */
sealed interface PlayContext {
    data class Playlist(val id: String, val name: String, val artUrl: String?) : PlayContext
    data class Artist(val id: String, val name: String, val artUrl: String?) : PlayContext
    data class Album(val id: String, val name: String, val artist: String, val artUrl: String?) : PlayContext
}

/** What's currently playing from, for the "go to source" button on
 *  [rocks.spotifast.android.ui.NowPlayingScreen] — just enough to navigate
 *  there, unlike [PlayContext] (which also needs a name/art for the
 *  recently-played feed, not always known for playback another device
 *  started — see [SpotifyCore.currentSource]). */
sealed interface CurrentSource {
    data class Playlist(val id: String) : CurrentSource
    data class Artist(val id: String) : CurrentSource
    data class Album(val id: String) : CurrentSource
}

private fun PlayContext.toCurrentSource(): CurrentSource = when (this) {
    is PlayContext.Playlist -> CurrentSource.Playlist(id)
    is PlayContext.Artist -> CurrentSource.Artist(id)
    is PlayContext.Album -> CurrentSource.Album(id)
}

/** One Spotify Connect device this account can play on. */
data class DeviceItem(
    val id: String,
    val name: String,
    val kind: String,
    val isActive: Boolean,
    val volumePercent: Int? = null,
    val supportsVolume: Boolean = false,
)

/** Streaming quality presets; librespot bakes the bitrate into the session
 *  at startup, so a change here takes effect the next time the app opens. */
enum class AudioQuality(val kbps: Int, val label: String) {
    Low(96, "Low"),
    Normal(160, "Normal"),
    High(320, "High"),
}

/** The app's selectable color themes; maps to [rocks.spotifast.android.ui.ThemeChoice]
 *  at the point [SpotifastTheme] is applied. Unlike [AudioQuality], this
 *  takes effect immediately — it's UI-only, nothing to restart. */
enum class AppTheme(val label: String) {
    Spotify("Spotify"),
    Noir("Comic Noir"),
}

/** How Library and Home order their playlists. [Custom] is Spotify's own
 *  order (the rootlist's order on Library, the Web API's on Home). There's
 *  no "recently added" (when you followed/created a playlist) — Spotify's
 *  API has no such timestamp anywhere. [RecentlyPlayed] and [RecentlyUpdated]
 *  use real but imperfect proxies — see [SpotifyCore.recentlyPlayedRanks]
 *  and [sortLibraryItems]. */
enum class LibrarySort(val label: String) {
    Custom("Custom order"),
    Alphabetical("Alphabetical"),
    RecentlyPlayed("Recently played"),
    RecentlyUpdated("Recently updated"),
}

/** The engine's playback phase — mirrors `core::player::Playback`. */
enum class PlaybackPhase { Stopped, Loading, Playing, Paused }

/** The currently (or last) loaded track, as the engine reports it.
 *  [artistId] comes straight from the engine's local playback metadata;
 *  [albumId] doesn't — that metadata only carries the album's name, not
 *  its id — so it arrives separately via `onTrackAlbum`, once resolved,
 *  and stays null until then (or forever, for a track with no album). */
data class PlayingTrack(
    val uri: String,
    val title: String,
    val artist: String,
    val artistId: String?,
    val album: String,
    val albumId: String? = null,
    val artUrl: String?,
    val durationMs: Long,
)

/** One lyrics line. [atMs] is null for a track with unsynced lyrics (see
 *  [LyricsResult.synced]). */
data class LyricsLine(val atMs: Int?, val text: String)

/** A [SpotifyCore.fetchLyrics] answer for the track at [uri] — check that
 *  against whatever's currently playing before showing it, since the
 *  answer can arrive after the track's moved on. [found] false means
 *  nobody's transcribed this track (not a fault); [error], when set, means
 *  the lookup itself failed (network, decode) rather than coming back empty. */
data class LyricsResult(
    val uri: String,
    val found: Boolean,
    val synced: Boolean,
    val instrumental: Boolean,
    val lines: List<LyricsLine>,
    val error: String?,
)

/**
 * The engine's live transport state: phase, loaded track, position, and
 * shuffle/repeat. `error` carries a load failure (e.g. a timed-out audio
 * key) while `track`/`phase` keep reporting whatever was playing before —
 * librespot's own "stays on the old track until the new one is ready"
 * behavior, not a bug in this mirror of it.
 */
data class LocalPlayback(
    val phase: PlaybackPhase,
    val track: PlayingTrack?,
    val positionMs: Long,
    val shuffle: Boolean,
    val repeat: String,
    val error: String?,
)

/** The account's playback as `GET /me/player` last reported it — see
 *  [SpotifyCore]'s `remoteSnapshot`/`publishEffectivePlayback`. */
private data class RemoteSnapshot(
    val deviceId: String?,
    val isPlaying: Boolean,
    val positionMs: Long,
    val shuffle: Boolean,
    val repeat: String,
    val track: PlayingTrack?,
    val source: CurrentSource?,
)

/**
 * Process-wide holder for the single `CoreHandle`. The Activity and
 * `PlaybackService` both read from this; Compose collects the state flows.
 */
object SpotifyCore {

    /**
     * The Spotify Web API client id whose `spotifast://callback` redirect is
     * registered, so the browser can return to the app.
     */
    private const val CLIENT_ID = "34f75901adbf40d9ad1b5b25c2d666ca"

    /** The sentinel playlist id Liked Songs is addressed by in [tracks] —
     *  it isn't a real playlist, but reuses the same storage/UI as one. */
    const val LIKED_SONGS_ID = "__liked__"

    /** The synthetic library entry Home/Library prepend to show Liked Songs
     *  alongside real playlists. */
    val LIKED_SONGS_ITEM = LibraryItem(LIKED_SONGS_ID, "Liked Songs", null)

    private var appContext: Context? = null
    /** The local playback phase as of the last [publishEffectivePlayback], so
     *  [ensurePlaybackServiceStarted] fires on an idle-to-playing edge rather
     *  than every update while already playing. */
    private var lastLocalPhase: PlaybackPhase? = null

    private val _authState = MutableStateFlow<AuthState>(AuthState.SignedOut)
    val authState: StateFlow<AuthState> = _authState.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private val _library = MutableStateFlow<List<LibraryItem>>(emptyList())
    val library: StateFlow<List<LibraryItem>> = _library.asStateFlow()

    /** The library as Spotify's own rootlist orders it, folders and all.
     *  Rebuilt whenever [library] or the raw rootlist rows change — it
     *  needs both: the rootlist has the tree shape but only bare ids, the
     *  flat library has the names/art. */
    private val _libraryTree = MutableStateFlow<List<LibraryNode>>(emptyList())
    val libraryTree: StateFlow<List<LibraryNode>> = _libraryTree.asStateFlow()

    private var rawRootlistRows: List<RawRootlistRow> = emptyList()

    /** Whether the flat `/me/playlists` sweep has completed at least once.
     *  The rootlist (session-protocol, fast) routinely arrives before that
     *  sweep (Web API, paginated) finishes — resolving "missing from
     *  library" rootlist rows individually before the sweep lands treats
     *  every playlist as missing, firing dozens of concurrent
     *  `playlist_info` lookups that trip Spotify's rate limiter and stall
     *  for up to 30s apiece. Waiting for the sweep first keeps that lookup
     *  to the handful of rows actually absent from it (Spotify's own
     *  algorithmic playlists, pinned but never followed). */
    private var libraryLoaded = false

    /** Ids already asked for via `playlist_info`, so a playlist `/me/playlists`
     *  doesn't know about (typically one of Spotify's own algorithmic
     *  playlists — Discover Weekly, a Daily Mix — pinned in the sidebar but
     *  not "followed") is only requested once, not every rebuild. */
    private val requestedPlaylistInfoIds = mutableSetOf<String>()

    /** Ids `playlist_info` came back 404 for — permanently unresolvable
     *  (Spotify's own algorithmic playlists, blocked from the public Web
     *  API for every account), so dropped from the tree instead of
     *  lingering as a dead "id as name" row. */
    private val unavailablePlaylistIds = mutableSetOf<String>()

    private val _tracks = MutableStateFlow<Map<String, List<TrackItem>>>(emptyMap())
    val tracks: StateFlow<Map<String, List<TrackItem>>> = _tracks.asStateFlow()

    /** Whether a playlist (or [LIKED_SONGS_ID]) has more tracks beyond what
     *  [tracks] currently holds — [loadMoreTracks]/[loadAllTracks] fetch
     *  them. Absent (rather than false) until the first page has arrived. */
    private val _tracksHasMore = MutableStateFlow<Map<String, Boolean>>(emptyMap())
    val tracksHasMore: StateFlow<Map<String, Boolean>> = _tracksHasMore.asStateFlow()

    /** Ids with a [loadMoreTracks]/[loadAllTracks] request in flight — lets
     *  a scroll-triggered caller skip re-requesting the same page before
     *  the last one has answered. */
    private val _tracksLoadingMore = MutableStateFlow<Set<String>>(emptySet())
    val tracksLoadingMore: StateFlow<Set<String>> = _tracksLoadingMore.asStateFlow()

    private val _selectedPlaylist = MutableStateFlow<String?>(null)
    val selectedPlaylist: StateFlow<String?> = _selectedPlaylist.asStateFlow()

    private val _selectedArtist = MutableStateFlow<String?>(null)
    val selectedArtist: StateFlow<String?> = _selectedArtist.asStateFlow()

    private val _selectedAlbum = MutableStateFlow<String?>(null)
    val selectedAlbum: StateFlow<String?> = _selectedAlbum.asStateFlow()

    /** Resolved artist profiles, by id — filled in as [openArtist] asks for
     *  them, same read-as-needed shape as [library]'s playlist lookups. */
    private val _artists = MutableStateFlow<Map<String, ArtistItem>>(emptyMap())
    val artists: StateFlow<Map<String, ArtistItem>> = _artists.asStateFlow()

    /** The rest of each resolved artist's profile, by id — arrives
     *  alongside [artists] from the same `artist_info` fetch, so opening
     *  the info view never needs a fetch of its own. */
    private val _artistDetails = MutableStateFlow<Map<String, ArtistDetails>>(emptyMap())
    val artistDetails: StateFlow<Map<String, ArtistDetails>> = _artistDetails.asStateFlow()

    /** An artist's most popular tracks, by artist id. */
    private val _artistTracks = MutableStateFlow<Map<String, List<TrackItem>>>(emptyMap())
    val artistTracks: StateFlow<Map<String, List<TrackItem>>> = _artistTracks.asStateFlow()

    /** An artist's albums and singles, by artist id. */
    private val _artistAlbums = MutableStateFlow<Map<String, List<AlbumItem>>>(emptyMap())
    val artistAlbums: StateFlow<Map<String, List<AlbumItem>>> = _artistAlbums.asStateFlow()

    /** The account's saved albums, for Library's Albums selector. */
    private val _savedAlbumItems = MutableStateFlow<List<AlbumItem>>(emptyList())
    val savedAlbumItems: StateFlow<List<AlbumItem>> = _savedAlbumItems.asStateFlow()

    /** The account's followed artists, for Library's Artists selector. */
    private val _followedArtistItems = MutableStateFlow<List<ArtistItem>>(emptyList())
    val followedArtistItems: StateFlow<List<ArtistItem>> = _followedArtistItems.asStateFlow()

    /** This week's new releases. */
    private val _newReleases = MutableStateFlow<List<AlbumItem>>(emptyList())
    val newReleases: StateFlow<List<AlbumItem>> = _newReleases.asStateFlow()

    /** Resolved album metadata, by id. */
    private val _albums = MutableStateFlow<Map<String, AlbumItem>>(emptyMap())
    val albums: StateFlow<Map<String, AlbumItem>> = _albums.asStateFlow()

    /** An album's tracks, by album id. */
    private val _albumTracks = MutableStateFlow<Map<String, List<TrackItem>>>(emptyMap())
    val albumTracks: StateFlow<Map<String, List<TrackItem>>> = _albumTracks.asStateFlow()

    private val _searchTracks = MutableStateFlow<List<TrackItem>>(emptyList())
    val searchTracks: StateFlow<List<TrackItem>> = _searchTracks.asStateFlow()

    private val _searching = MutableStateFlow(false)
    val searching: StateFlow<Boolean> = _searching.asStateFlow()

    /** Recent search queries, newest first, capped and persisted locally —
     *  Spotify's Web API has nothing like `/me/search-history`, so this is
     *  purely a client-side convenience, same as every other Android-only
     *  preference in this file. */
    private val _searchHistory = MutableStateFlow<List<String>>(emptyList())
    val searchHistory: StateFlow<List<String>> = _searchHistory.asStateFlow()

    /** The query [rememberSearch] last added on its own (not a user's
     *  explicit pick from [searchHistory] or long-press removal) — while
     *  it's still the history's first entry, a query that extends or
     *  shortens it takes its place instead of piling up one entry per
     *  keystroke the debounce happened to fire on. */
    private var lastAutoRememberedQuery: String? = null

    private val _playbackStatus = MutableStateFlow("Unavailable" to "")
    val playbackStatus: StateFlow<Pair<String, String>> = _playbackStatus.asStateFlow()

    /** The engine's own last-reported state — local playback only. Never
     *  read by the UI directly; [localPlayback] is the merged value the UI
     *  actually sees, derived from this plus [remoteSnapshot] by
     *  [publishEffectivePlayback]. Kept separate so a track change on
     *  *this* device can still be told apart from one on another device. */
    private var engineState: LocalPlayback? = null

    /** The account's playback as `GET /me/player` last reported it —
     *  whichever Connect device is active, polled periodically since
     *  nothing pushes this the way the local engine pushes [engineState].
     *  Null until the first poll lands. */
    private var remoteSnapshot: RemoteSnapshot? = null

    /** This phone's own Connect device id, from `onPlaybackStatus`'s
     *  `Ready` detail — what [remoteSnapshot]'s device id is compared
     *  against to tell "another device took over" from "that's just me". */
    private var localDeviceId: String? = null

    /** The uri [playTracks] last asked to load, while still waiting for
     *  `onNowPlaying` to actually confirm it — see that method's doc for
     *  why a stale intermediate report needs to be ignored rather than
     *  trusted at face value during this window. */
    private var pendingTrackUri: String? = null

    /** What [playTracks] last started *locally* playing from, if a
     *  [PlayContext] was given — persists across a natural track advance
     *  within that same context, only changing on the next [playTracks]
     *  (or [goToRadio], which clears it: a radio queue isn't "from"
     *  anywhere the way a playlist/artist/album view is). Folded into
     *  [currentSource] by [publishEffectivePlayback], same as
     *  [engineState] vs [remoteSnapshot] for [localPlayback]. */
    private var localPlaySource: CurrentSource? = null

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private val _localPlayback = MutableStateFlow<LocalPlayback?>(null)

    /** The engine's current track/transport state — [engineState] normally,
     *  but [remoteSnapshot] instead whenever *another* Connect device is
     *  the one actually playing, so the UI (and the lock-screen/notification
     *  controls in `PlaybackService`) always reflect whichever device the
     *  account is really using, not just this phone's own idle session. */
    val localPlayback: StateFlow<LocalPlayback?> = _localPlayback.asStateFlow()

    private val _currentSource = MutableStateFlow<CurrentSource?>(null)

    /** What [localPlayback] is currently playing from — a playlist, artist,
     *  or album, whichever device is actually playing — for the "go to
     *  source" button on [rocks.spotifast.android.ui.NowPlayingScreen].
     *  Null when nothing's playing, or it's not from one of those three
     *  (search results, the queue, a radio, Liked Songs — Spotify's own
     *  `context.type` for that one is `"collection"`, deliberately left
     *  out since there's no screen to navigate to for it). */
    val currentSource: StateFlow<CurrentSource?> = _currentSource.asStateFlow()

    /** Recomputes [localPlayback] from [engineState] and [remoteSnapshot].
     *  Call after changing either, or after [localDeviceId] first becomes
     *  known (a remote poll can land before that does). */
    private fun publishEffectivePlayback() {
        val remote = remoteSnapshot
        val isRemoteActive = remote != null && remote.deviceId != null && remote.deviceId != localDeviceId
        val playback = if (isRemoteActive) {
            LocalPlayback(
                phase = if (remote.isPlaying) PlaybackPhase.Playing else PlaybackPhase.Paused,
                track = remote.track,
                positionMs = remote.positionMs,
                shuffle = remote.shuffle,
                repeat = remote.repeat,
                error = null,
            )
        } else {
            engineState
        }
        val previous = _localPlayback.value?.track
        if (previous != null && playback?.track?.uri != null && playback.track.uri != previous.uri) {
            _previousTrack.value = previous
            // Keeps the queue fresh for the Queue screen — cheap, a local
            // Connect-state read rather than a network call.
            loadQueue()
        }
        // The foreground service's Doze/App Standby exemption can lapse
        // independently of the Connect session staying up — Android (or
        // media3 itself) can stop an idle service without the connection
        // status ever dropping, so a later song would play with no
        // exemption active and then die the moment the screen locks. Rather
        // than a one-shot start keyed to the connection status, re-assert on
        // every idle-to-playing edge: startService on an already-running
        // service is a safe, idempotent no-op.
        val localPhase = if (isRemoteActive) null else engineState?.phase
        if (localPhase == PlaybackPhase.Playing && lastLocalPhase != PlaybackPhase.Playing) {
            ensurePlaybackServiceStarted()
        }
        lastLocalPhase = localPhase
        _localPlayback.value = playback
        _currentSource.value = if (isRemoteActive) remote?.source else localPlaySource
        ListenBrainz.onPlaybackChanged(playback?.track, playback?.phase)
    }

    /** Starts [PlaybackService] if it isn't already running. Safe to call
     *  repeatedly — `startService` on an already-running service just
     *  redelivers `onStartCommand`, it doesn't recreate it. */
    private fun ensurePlaybackServiceStarted() {
        try {
            appContext?.startService(Intent(appContext, PlaybackService::class.java))
        } catch (error: Exception) {
            // Best-effort; the next idle-to-playing transition tries again.
        }
    }

    /** The id of the active Connect device, if it's one other than this
     *  phone — what [togglePlayback]/[skipNext]/[skipPrevious]/[seek]
     *  route to instead of the local engine when it's non-null. */
    private fun activeRemoteDeviceId(): String? {
        val remote = remoteSnapshot ?: return null
        return remote.deviceId?.takeIf { it != localDeviceId }
    }

    /** Known liked state per track URI; absent means not yet checked. */
    private val _likedTracks = MutableStateFlow<Map<String, Boolean>>(emptyMap())
    val likedTracks: StateFlow<Map<String, Boolean>> = _likedTracks.asStateFlow()

    private val _searchArtists = MutableStateFlow<List<ArtistItem>>(emptyList())
    val searchArtists: StateFlow<List<ArtistItem>> = _searchArtists.asStateFlow()

    private val _searchAlbums = MutableStateFlow<List<AlbumItem>>(emptyList())
    val searchAlbums: StateFlow<List<AlbumItem>> = _searchAlbums.asStateFlow()

    private val _searchPlaylists = MutableStateFlow<List<LibraryItem>>(emptyList())
    val searchPlaylists: StateFlow<List<LibraryItem>> = _searchPlaylists.asStateFlow()

    private val _topTracks = MutableStateFlow<List<TrackItem>>(emptyList())
    val topTracks: StateFlow<List<TrackItem>> = _topTracks.asStateFlow()

    private val _topArtists = MutableStateFlow<List<ArtistItem>>(emptyList())
    val topArtists: StateFlow<List<ArtistItem>> = _topArtists.asStateFlow()

    private val _devices = MutableStateFlow<List<DeviceItem>>(emptyList())
    val devices: StateFlow<List<DeviceItem>> = _devices.asStateFlow()

    private val _queue = MutableStateFlow<List<TrackItem>>(emptyList())
    val queue: StateFlow<List<TrackItem>> = _queue.asStateFlow()

    /** The track effective playback just moved on from — one step only, not
     *  a full history. There's no "previous tracks" API to read this from
     *  (Connect only exposes a forward queue), so [publishEffectivePlayback]
     *  captures it itself on every track change. Lets the swipeable artwork
     *  preview what a right-swipe would go back to. */
    private val _previousTrack = MutableStateFlow<PlayingTrack?>(null)
    val previousTrack: StateFlow<PlayingTrack?> = _previousTrack.asStateFlow()

    /** A brief confirmation/error banner (queue added, device switched); the
     *  UI clears it itself a couple seconds after showing it. */
    private val _toast = MutableStateFlow<String?>(null)
    val toast: StateFlow<String?> = _toast.asStateFlow()

    /** Pinned library item ids (playlists, or [LIKED_SONGS_ID]), in no
     *  particular order — the UI sorts pinned items first. */
    private val _pinnedIds = MutableStateFlow<Set<String>>(emptySet())
    val pinnedIds: StateFlow<Set<String>> = _pinnedIds.asStateFlow()

    /** The active color theme; a StateFlow (unlike [AudioQuality], which is
     *  read lazily) because [rocks.spotifast.android.ui.SpotifastTheme]
     *  needs to recompose the instant Settings changes it. */
    private val _appTheme = MutableStateFlow(AppTheme.Spotify)
    val appTheme: StateFlow<AppTheme> = _appTheme.asStateFlow()

    /** Whether Noir mode runs cover art through the Ink Outline bitmap
     *  filter. Independent of [appTheme] itself — the posterize+edge-trace
     *  pass has a real per-image cost, so this is an opt-out. */
    private val _inkOutlineEnabled = MutableStateFlow(true)
    val inkOutlineEnabled: StateFlow<Boolean> = _inkOutlineEnabled.asStateFlow()

    /** How Library and Home order their playlists. */
    private val _librarySort = MutableStateFlow(LibrarySort.Custom)
    val librarySort: StateFlow<LibrarySort> = _librarySort.asStateFlow()

    /** Library item id (a playlist id, or [LIKED_SONGS_ID]) to its rank in
     *  recently-played order, 0 = most recent. Only holds the contexts
     *  `/me/player/recently-played` actually returned — an id missing here
     *  hasn't been played from recently, not an error. */
    private val _recentlyPlayedRanks = MutableStateFlow<Map<String, Int>>(emptyMap())
    val recentlyPlayedRanks: StateFlow<Map<String, Int>> = _recentlyPlayedRanks.asStateFlow()

    /** Home's unified recently-played feed — playlists, artists, and albums
     *  a track was actually played from, newest first. Persisted so it
     *  survives restarts; see [recordPlayContext] and [upsertRecentEntries]. */
    private val _recentlyPlayedFeed = MutableStateFlow<List<RecentEntry>>(emptyList())
    val recentlyPlayedFeed: StateFlow<List<RecentEntry>> = _recentlyPlayedFeed.asStateFlow()

    /** Pinned Home cards — `"${kind}:${id}"` keys (see [recentKey]) rather
     *  than bare ids, since an artist and an album could in principle share
     *  one. Pinned cards float to the top of Home and are exempt from
     *  [RECENT_FEED_CAP] eviction. */
    private val _pinnedRecentKeys = MutableStateFlow<Set<String>>(emptySet())
    val pinnedRecentKeys: StateFlow<Set<String>> = _pinnedRecentKeys.asStateFlow()

    /** The track a long-press context menu is open for, or null when closed.
     *  Held centrally so any screen's [TrackRow] can open the same sheet
     *  without each screen wiring its own bottom-sheet state. */
    private val _trackMenu = MutableStateFlow<TrackItem?>(null)
    val trackMenu: StateFlow<TrackItem?> = _trackMenu.asStateFlow()

    /** The library item (playlist or Liked Songs) a long-press context menu
     *  is open for, or null when closed. */
    private val _libraryMenu = MutableStateFlow<LibraryItem?>(null)
    val libraryMenu: StateFlow<LibraryItem?> = _libraryMenu.asStateFlow()

    /** The Home card a long-press context menu is open for, or null when closed. */
    private val _recentMenu = MutableStateFlow<RecentEntry?>(null)
    val recentMenu: StateFlow<RecentEntry?> = _recentMenu.asStateFlow()

    /** The album (id, name) a long-press context menu is open for, or null
     *  when closed. */
    private val _albumMenu = MutableStateFlow<AlbumItem?>(null)
    val albumMenu: StateFlow<AlbumItem?> = _albumMenu.asStateFlow()

    /** The last [fetchLyrics] answer. Check its `uri` against whatever's
     *  playing before showing it — the answer can land after the track's
     *  moved on, and this isn't cleared out from under a still-loading
     *  request the way [pendingTrackUri] guards now-playing snapshots. */
    private val _lyrics = MutableStateFlow<LyricsResult?>(null)
    val lyrics: StateFlow<LyricsResult?> = _lyrics.asStateFlow()

    /** The label of the radio currently playing ("Future Islands Radio"),
     *  or null when what's playing isn't a radio — set once [goToRadio]
     *  resolves, cleared by any other [playTracks] call (skipping within
     *  the radio doesn't call [playTracks] again, so it stays set). */
    private val _activeRadio = MutableStateFlow<String?>(null)
    val activeRadio: StateFlow<String?> = _activeRadio.asStateFlow()

    /** The label passed to the in-flight [goToRadio] call, applied once
     *  `on_radio` answers — the response JSON itself carries no label. */
    private var pendingRadioLabel: String? = null

    /** Whether a [createPlaylist] call is still waiting on
     *  `onPlaylistCreated` — the Rust side only tracks one creation in
     *  flight at a time, so UI must disable its create/save button while
     *  this is true. */
    private val _creatingPlaylist = MutableStateFlow(false)
    val creatingPlaylist: StateFlow<Boolean> = _creatingPlaylist.asStateFlow()

    private var signedInUsername: String? = null

    // Tracks an in-flight addTracksToQueue batch, so onQueueAdded shows one
    // combined toast at the end instead of one per track.
    private var queueBatchPending = 0
    private var queueBatchSucceeded = 0
    private var queueBatchLabel = ""

    private var handle: CoreHandle? = null
    private var prefs: SharedPreferences? = null

    private val listener = object : PlaybackStateListener {
        override fun onAuthStatus(status: String, detail: String) {
            _authState.value = when (status) {
                "Starting", "SignedOut" -> AuthState.SignedOut
                "WaitingForBrowser" -> AuthState.WaitingForBrowser(detail)
                "Connecting" -> AuthState.Connecting
                "Connected" -> {
                    signedInUsername = detail
                    AuthState.Connected(detail)
                }
                "Failed" -> AuthState.Failed(detail)
                else -> AuthState.SignedOut
            }
        }

        override fun onError(message: String) {
            _error.value = message
        }

        override fun onLibrary(playlists: String) {
            _library.value = parseLibraryItems(playlists)
            libraryLoaded = true
            rebuildLibraryTree()
        }

        override fun onRootlist(rows: String) {
            rawRootlistRows = runCatching {
                val array = JSONArray(rows)
                (0 until array.length()).map { index ->
                    val row = array.getJSONObject(index)
                    RawRootlistRow(
                        kind = row.getString("kind"),
                        id = row.nullableString("id"),
                        name = row.nullableString("name"),
                    )
                }
            }.getOrDefault(emptyList())
            rebuildLibraryTree()
        }

        override fun onPlaylistInfo(id: String, name: String, artUrl: String?) {
            _library.value = _library.value.filterNot { it.id == id } + LibraryItem(id, name, artUrl)
            rebuildLibraryTree()
        }

        override fun onPlaylistUnavailable(id: String) {
            unavailablePlaylistIds.add(id)
            rebuildLibraryTree()
        }

        override fun onArtistInfo(id: String, name: String, artUrl: String?) {
            _artists.value = _artists.value + (id to ArtistItem(id, name, artUrl))
        }

        override fun onArtistDetails(id: String, details: String) {
            val parsed = runCatching {
                val json = org.json.JSONObject(details)
                val genres = json.optJSONArray("genres")?.let { array ->
                    (0 until array.length()).map { array.getString(it) }
                } ?: emptyList()
                val images = json.optJSONArray("images")?.let { array ->
                    (0 until array.length()).map { array.getString(it) }
                } ?: emptyList()
                ArtistDetails(
                    id = id,
                    followers = json.getLong("followers"),
                    genres = genres,
                    popularity = if (json.has("popularity") && !json.isNull("popularity")) json.getInt("popularity") else null,
                    images = images,
                )
            }.getOrNull() ?: return
            _artistDetails.value = _artistDetails.value + (id to parsed)
        }

        override fun onArtistTopTracks(id: String, tracks: String) {
            _artistTracks.value = _artistTracks.value + (id to parseTrackItems(tracks))
        }

        override fun onArtistAlbums(id: String, albums: String) {
            _artistAlbums.value = _artistAlbums.value + (id to parseAlbumItems(albums))
        }

        override fun onSavedAlbums(albums: String) {
            _savedAlbumItems.value = parseAlbumItems(albums)
        }

        override fun onFollowedArtists(artists: String) {
            _followedArtistItems.value = parseArtistItems(artists)
        }

        override fun onNewReleases(albums: String) {
            _newReleases.value = parseAlbumItems(albums)
        }

        override fun onAlbumInfo(id: String, name: String, artist: String, artUrl: String?) {
            _albums.value = _albums.value + (id to AlbumItem(id, name, artist, artUrl))
        }

        override fun onAlbumTracks(id: String, tracks: String) {
            _albumTracks.value = _albumTracks.value + (id to parseTrackItems(tracks))
        }

        override fun onRecentlyPlayed(contexts: String) {
            val ranks = mutableMapOf<String, Int>()
            val serverEntries = mutableListOf<RecentEntry>()
            runCatching {
                val array = JSONArray(contexts)
                for (index in 0 until array.length()) {
                    val obj = array.getJSONObject(index)
                    val uri = obj.getString("uri")
                    val id = when {
                        uri.endsWith(":collection") -> LIKED_SONGS_ID
                        uri.startsWith("spotify:playlist:") -> uri.removePrefix("spotify:playlist:")
                        else -> null
                    } ?: continue
                    if (!ranks.containsKey(id)) ranks[id] = ranks.size
                    val timestampMs = obj.nullableString("played_at")
                        ?.let { runCatching { java.time.Instant.parse(it).toEpochMilli() }.getOrNull() }
                        ?: continue
                    val item = if (id == LIKED_SONGS_ID) LIKED_SONGS_ITEM else _library.value.firstOrNull { it.id == id }
                    serverEntries += RecentEntry(RecentKind.Playlist, id, item?.name ?: "Playlist", "", item?.artUrl, timestampMs)
                }
            }
            _recentlyPlayedRanks.value = ranks
            // A device played elsewhere can report an older play than one
            // already recorded locally — upsertRecentEntries only lets a
            // *newer* timestamp move an entry, so this never overrides that.
            upsertRecentEntries(serverEntries)
        }

        override fun onPlaylistTracks(playlistId: String, tracks: String, hasMore: Boolean) {
            val parsed = parseTrackItems(tracks)
            // Liked Songs has no manual ordering of its own to preserve (unlike
            // a real playlist) — Spotify's own app shows it newest-saved-first,
            // so sort by addedAt rather than trusting the API/cache's own order.
            val ordered = if (playlistId == LIKED_SONGS_ID) {
                parsed.sortedByDescending { it.addedAt ?: "" }
            } else {
                parsed
            }
            _tracks.value = _tracks.value + (playlistId to ordered)
            _tracksHasMore.value = _tracksHasMore.value + (playlistId to hasMore)
            _tracksLoadingMore.value = _tracksLoadingMore.value - playlistId
        }

        override fun onSearchTracks(query: String, tracks: String) {
            _searchTracks.value = parseTrackItems(tracks)
        }

        override fun onSearchArtists(query: String, artists: String) {
            _searchArtists.value = parseArtistItems(artists)
        }

        override fun onSearchAlbums(query: String, albums: String) {
            _searchAlbums.value = parseAlbumItems(albums)
        }

        override fun onSearchPlaylists(query: String, playlists: String) {
            _searchPlaylists.value = parseLibraryItems(playlists)
        }

        override fun onTopTracks(tracks: String) {
            _topTracks.value = parseTrackItems(tracks)
        }

        override fun onTopArtists(artists: String) {
            _topArtists.value = parseArtistItems(artists)
        }

        override fun onDevices(devices: String) {
            _devices.value = runCatching {
                val array = JSONArray(devices)
                (0 until array.length()).map { index ->
                    val item = array.getJSONObject(index)
                    DeviceItem(
                        item.getString("id"),
                        item.getString("name"),
                        item.getString("kind"),
                        item.getBoolean("is_active"),
                        item.nullableInt("volume_percent"),
                        item.getBoolean("supports_volume"),
                    )
                }
            }.getOrDefault(emptyList())
        }

        override fun onDeviceTransferred(deviceId: String, ok: Boolean) {
            val name = _devices.value.firstOrNull { it.id == deviceId }?.name
            _toast.value = when {
                !ok -> "Couldn't switch devices"
                name != null -> "Playing on $name"
                else -> "Switched devices"
            }
            if (ok) handle?.devices()
        }

        override fun onQueue(tracks: String) {
            _queue.value = parseTrackItems(tracks)
        }

        override fun onQueueAdded(label: String, ok: Boolean) {
            if (queueBatchPending > 0) {
                queueBatchPending -= 1
                if (ok) queueBatchSucceeded += 1
                if (queueBatchPending == 0) {
                    _toast.value = if (queueBatchSucceeded > 0) {
                        "Added \"$queueBatchLabel\" to queue"
                    } else {
                        "Couldn't add to queue"
                    }
                }
                return
            }
            _toast.value = if (ok) "Added \"$label\" to queue" else "Couldn't add to queue"
        }

        override fun onPlaybackStatus(status: String, detail: String) {
            _playbackStatus.value = status to detail
            if (status == "Ready") {
                // `detail` is this phone's own Connect device id — the one
                // `remoteSnapshot.deviceId` is compared against to tell
                // "another device took over" from "that's just me".
                localDeviceId = detail
                publishEffectivePlayback()
            }
            // The playback grant's browser flow (reusing WaitingForBrowser) is
            // done; return to the library rather than leaving the spinner up.
            if (_authState.value is AuthState.WaitingForBrowser) {
                signedInUsername?.let { _authState.value = AuthState.Connected(it) }
            }
            // The engine is now a live Spotify Connect receiver — start the
            // foreground service so Doze/App Standby don't throttle its
            // background network connection. Plain startService, not
            // startForegroundService: this fires well after the user has
            // been actively using the app (they just authorized playback),
            // not at a cold launch where "foreground" isn't established yet.
            // publishEffectivePlayback's own idle-to-playing check is the
            // primary guard against the service having been separately
            // stopped since; this is just a second, equally-idempotent
            // trigger on the (re)connection itself.
            if (status == "Ready") {
                ensurePlaybackServiceStarted()
            }
            // The rootlist (unlike the flat Web API playlist list) reads
            // through the local session, so it's only reachable once that
            // session exists — calling it any earlier just silently yields
            // nothing. Repeat calls here are cheap no-ops once the first
            // one lands.
            if (status == "Ready") {
                handle?.rootlist()
            }
        }

        override fun onNowPlaying(state: String) {
            // The engine's own confirmation carries no album id (see
            // `PlayingTrack`'s doc) and fires on every transport change,
            // not just a track change — rebuilding from this JSON would
            // otherwise wipe out an album id already known (from an
            // optimistic `playTracks` placeholder, or a prior
            // `onTrackAlbum` resolve) every time playback merely pauses
            // or seeks. Carried forward whenever the uri is unchanged;
            // re-resolved only when it actually is.
            val previousTrack = engineState?.track
            val parsed = runCatching {
                val json = org.json.JSONObject(state)
                val track = json.optJSONObject("track")?.let {
                    val uri = it.getString("uri")
                    PlayingTrack(
                        uri = uri,
                        title = it.getString("title"),
                        artist = it.getString("artist"),
                        artistId = it.nullableString("artist_id"),
                        album = it.getString("album"),
                        albumId = if (uri == previousTrack?.uri) previousTrack.albumId else null,
                        artUrl = it.nullableString("art_url"),
                        durationMs = it.getLong("duration_ms"),
                    )
                }
                LocalPlayback(
                    phase = PlaybackPhase.valueOf(json.getString("playback")),
                    track = track,
                    positionMs = json.getLong("position_ms"),
                    shuffle = json.getBoolean("shuffle"),
                    repeat = json.getString("repeat"),
                    error = json.nullableString("error"),
                )
            }.getOrNull() ?: return

            // A track-change goes through the engine's own Stopped, then
            // Loading, before its TrackChanged finally reports the new
            // track — Stopped/Loading don't clear the previous one, so
            // until TrackChanged lands, this still reports whatever was
            // loaded before. Taking that at face value would flash the UI
            // back to the track playTracks just replaced, right after its
            // own optimistic update showed the tapped one. Ignored until a
            // report actually matches (or something goes wrong and the
            // fallback in playTracks gives up waiting).
            val expectedUri = pendingTrackUri
            if (expectedUri != null) {
                val matches = parsed.track?.uri == expectedUri || parsed.error != null
                if (!matches) return
                pendingTrackUri = null
            }

            engineState = parsed
            publishEffectivePlayback()
            val newUri = engineState?.track?.uri
            if (newUri != null && newUri != previousTrack?.uri) {
                handle?.trackAlbum(newUri)
            }
        }

        override fun onTrackAlbum(uri: String, albumId: String?) {
            val current = engineState
            val track = current?.track
            if (current != null && track != null && track.uri == uri) {
                engineState = current.copy(track = track.copy(albumId = albumId))
                publishEffectivePlayback()
            }
        }

        override fun onRemotePlaybackState(state: String) {
            remoteSnapshot = runCatching {
                val json = org.json.JSONObject(state)
                val track = json.optJSONObject("track")?.let {
                    PlayingTrack(
                        uri = it.getString("uri"),
                        title = it.getString("title"),
                        artist = it.getString("artist"),
                        artistId = it.nullableString("artist_id"),
                        album = it.getString("album"),
                        albumId = it.nullableString("album_id"),
                        artUrl = it.nullableString("art_url"),
                        durationMs = it.getLong("duration_ms"),
                    )
                }
                val sourceId = json.nullableString("context_id")
                val source = when (json.nullableString("context_kind")) {
                    "playlist" -> sourceId?.let { CurrentSource.Playlist(it) }
                    "artist" -> sourceId?.let { CurrentSource.Artist(it) }
                    "album" -> sourceId?.let { CurrentSource.Album(it) }
                    else -> null
                }
                RemoteSnapshot(
                    deviceId = json.nullableString("device_id"),
                    isPlaying = json.getBoolean("is_playing"),
                    positionMs = json.getLong("position_ms"),
                    shuffle = json.getBoolean("shuffle"),
                    repeat = json.getString("repeat"),
                    track = track,
                    source = source,
                )
            }.getOrNull()
            publishEffectivePlayback()
        }

        override fun onTrackSaved(uri: String, saved: Boolean) {
            _likedTracks.value = _likedTracks.value + (uri to saved)
        }

        override fun onLyrics(lyrics: String) {
            _lyrics.value = runCatching {
                val json = org.json.JSONObject(lyrics)
                val lines = json.optJSONArray("lines")?.let { array ->
                    (0 until array.length()).map { index ->
                        val line = array.getJSONObject(index)
                        LyricsLine(
                            atMs = if (line.has("at_ms") && !line.isNull("at_ms")) line.getInt("at_ms") else null,
                            text = line.getString("text"),
                        )
                    }
                } ?: emptyList()
                LyricsResult(
                    uri = json.getString("uri"),
                    found = json.getBoolean("found"),
                    synced = json.getBoolean("synced"),
                    instrumental = json.getBoolean("instrumental"),
                    lines = lines,
                    error = json.nullableString("error"),
                )
            }.getOrNull()
        }

        override fun onRadio(radio: String) {
            val label = pendingRadioLabel
            pendingRadioLabel = null
            runCatching {
                val json = org.json.JSONObject(radio)
                val error = json.nullableString("error")
                if (error != null) {
                    _toast.value = "Couldn't start radio: $error"
                    return@runCatching
                }
                val tracks = parseTrackItems(json.getJSONArray("tracks").toString())
                if (tracks.isNotEmpty()) {
                    playTracks(tracks, 0)
                    // After playTracks, which clears any previous radio —
                    // this is what's actually playing now.
                    _activeRadio.value = label
                }
            }
        }

        override fun onPlaylistCreated(name: String, id: String, added: UInt, requested: UInt, error: String?) {
            _creatingPlaylist.value = false
            _toast.value = when {
                error != null -> "Couldn't create \"$name\": $error"
                requested == 0u -> "Created \"$name\""
                added == requested -> "Created \"$name\" with $added track${if (added == 1u) "" else "s"}"
                else -> "Created \"$name\" — $added of $requested tracks added"
            }
        }
    }

    @Synchronized
    fun init(context: Context) {
        appContext = context.applicationContext
        ListenBrainz.init(context)
        if (handle == null) {
            val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            this.prefs = prefs
            _pinnedIds.value = prefs.getStringSet(PREF_PINNED_IDS, emptySet())?.toSet() ?: emptySet()
            _pinnedRecentKeys.value = prefs.getStringSet(PREF_PINNED_RECENT_KEYS, emptySet())?.toSet() ?: emptySet()
            _appTheme.value = AppTheme.entries.firstOrNull { it.name == prefs.getString(PREF_APP_THEME, null) }
                ?: AppTheme.Spotify
            _inkOutlineEnabled.value = prefs.getBoolean(PREF_INK_OUTLINE, true)
            _librarySort.value = LibrarySort.entries.firstOrNull { it.name == prefs.getString(PREF_LIBRARY_SORT, null) }
                ?: LibrarySort.Custom
            _searchHistory.value = runCatching {
                val array = JSONArray(prefs.getString(PREF_SEARCH_HISTORY, null) ?: "[]")
                (0 until array.length()).map { array.getString(it) }
            }.getOrDefault(emptyList())
            _recentlyPlayedFeed.value = loadRecentFeed(prefs)
            // UniFFI's generated bindings load this library through JNA, which
            // dlopen()s it directly and never triggers JNI_OnLoad — the hook
            // the Rust side relies on to hand cpal's AAudio backend a JVM/
            // Context pair (see android/rust/src/lib.rs). Loading it via the
            // JVM's own mechanism first makes that load the one that counts;
            // JNA's later dlopen just reuses the already-mapped library.
            System.loadLibrary("spotifast_android")
            val filesDir = context.applicationContext.filesDir.absolutePath
            val bitrateKbps = prefs.getInt(PREF_BITRATE_KBPS, AudioQuality.Normal.kbps)
            handle = CoreHandle(
                CLIENT_ID,
                EncryptedCredentialStore(context.applicationContext),
                filesDir,
                bitrateKbps.toUShort(),
            )
            handle?.setListener(listener)
            // Home's recently-played feed needs this regardless of
            // Library's own sort choice, so it's no longer conditional.
            handle?.recentlyPlayed()
            // The only way to notice playback that moved to, or started on,
            // some other Connect device — that device's own session never
            // tells this one anything directly, unlike the local engine's
            // `onNowPlaying` pushes. See `publishEffectivePlayback`.
            scope.launch {
                while (true) {
                    handle?.playbackState()
                    delay(6_000)
                }
            }
        }
    }

    fun signIn() = handle?.signIn()

    fun completeRedirect(code: String) = handle?.completeRedirect(code)

    fun signOut() = handle?.signOut()

    fun library() = handle?.library()

    /** Requests the account's playlist tree; the result arrives in
     *  [libraryTree] (combined with whatever [library] already has). */
    fun loadLibraryTree() = handle?.rootlist()

    private fun rebuildLibraryTree() {
        val playlistsById = _library.value.associateBy { it.id }
        val visibleRows = rawRootlistRows.filterNot { it.kind == "playlist" && unavailablePlaylistIds.contains(it.id) }
        _libraryTree.value = buildLibraryTree(visibleRows, playlistsById)
        if (!libraryLoaded) return
        for (row in rawRootlistRows) {
            val id = row.id
            val needsLookup = row.kind == "playlist" && id != null &&
                !playlistsById.containsKey(id) && !unavailablePlaylistIds.contains(id)
            if (needsLookup && id != null && requestedPlaylistInfoIds.add(id)) {
                handle?.playlistInfo(id)
            }
        }
    }

    fun playlist(id: String) {
        // A fresh open resets any leftover hasMore/loading flags from a
        // previous time this id was open — the first page's own answer
        // (always arriving, cache or live) sets them correctly again.
        _tracksHasMore.value = _tracksHasMore.value - id
        _tracksLoadingMore.value = _tracksLoadingMore.value - id
        handle?.playlist(id)
    }

    /** Fetches one more page of `id`'s tracks (a playlist id, or
     *  [LIKED_SONGS_ID]) on top of what [tracks] already has — meant for a
     *  scroll nearing the end of what's loaded. A no-op if [tracksHasMore]
     *  is already false, or another load is already in flight for `id`. */
    fun loadMoreTracks(id: String) {
        if (_tracksHasMore.value[id] != true || _tracksLoadingMore.value.contains(id)) return
        _tracksLoadingMore.value = _tracksLoadingMore.value + id
        if (id == LIKED_SONGS_ID) handle?.loadMoreLikedSongs() else handle?.loadMorePlaylistTracks(id)
    }

    /** Like [loadMoreTracks], but keeps following every remaining page on
     *  its own — the "Load all" button. */
    fun loadAllTracks(id: String) {
        if (_tracksHasMore.value[id] != true || _tracksLoadingMore.value.contains(id)) return
        _tracksLoadingMore.value = _tracksLoadingMore.value + id
        if (id == LIKED_SONGS_ID) handle?.loadAllLikedSongs() else handle?.loadAllPlaylistTracks(id)
    }

    /** [selectedPlaylist]/[selectedArtist]/[selectedAlbum] are mutually
     *  exclusive detail screens — opening one must clear the other two, or
     *  the one already open keeps winning the app's single `when` over the
     *  newly-requested screen until it's closed on its own (e.g. opening an
     *  artist from a playlist's track menu would otherwise show nothing
     *  until the playlist itself is backed out of). */
    private fun closeDetailScreens() {
        _selectedPlaylist.value = null
        _selectedArtist.value = null
        _selectedAlbum.value = null
    }

    fun openPlaylist(id: String) {
        closeDetailScreens()
        _selectedPlaylist.value = id
        if (id == LIKED_SONGS_ID) handle?.likedSongs() else handle?.playlist(id)
    }

    fun closePlaylist() {
        _selectedPlaylist.value = null
    }

    /** Opens an artist's profile; the result arrives in
     *  [artists]/[artistTracks]/[artistAlbums]. */
    fun openArtist(id: String) {
        closeDetailScreens()
        _selectedArtist.value = id
        handle?.artistInfo(id)
        handle?.artistTopTracks(id)
        handle?.artistAlbums(id)
    }

    fun closeArtist() {
        _selectedArtist.value = null
    }

    /** Opens an album's track list; the result arrives in [albums]/[albumTracks]. */
    fun openAlbum(id: String) {
        closeDetailScreens()
        _selectedAlbum.value = id
        handle?.albumInfo(id)
        handle?.albumTracks(id)
    }

    fun closeAlbum() {
        _selectedAlbum.value = null
    }

    fun search(query: String) {
        handle?.search(query)
        rememberSearch(query)
    }

    fun openSearch() {
        _searching.value = true
    }

    fun closeSearch() {
        _searching.value = false
        _searchTracks.value = emptyList()
        _searchArtists.value = emptyList()
        _searchAlbums.value = emptyList()
        _searchPlaylists.value = emptyList()
    }

    /** Adds `query` to [searchHistory], newest first. Called from [search]
     *  itself (after the screen's own debounce), so this fires once per
     *  pause in typing rather than once per keystroke — and even then, a
     *  query that merely extends or shortens the one [search] just added
     *  replaces it instead of adding a second entry for the same search. */
    private fun rememberSearch(query: String) {
        val trimmed = query.trim()
        if (trimmed.isEmpty()) return
        val history = _searchHistory.value.toMutableList()
        val previous = lastAutoRememberedQuery
        if (previous != null && history.firstOrNull() == previous &&
            (trimmed.startsWith(previous, ignoreCase = true) || previous.startsWith(trimmed, ignoreCase = true))
        ) {
            history.removeAt(0)
        }
        history.removeAll { it.equals(trimmed, ignoreCase = true) }
        history.add(0, trimmed)
        lastAutoRememberedQuery = trimmed
        _searchHistory.value = history.take(12)
        prefs?.edit()?.putString(PREF_SEARCH_HISTORY, JSONArray(_searchHistory.value).toString())?.apply()
    }

    /** Removes one entry from [searchHistory] — the recent-search list's
     *  own per-row dismiss, not a result of searching. */
    fun forgetSearch(query: String) {
        _searchHistory.value = _searchHistory.value.filterNot { it == query }
        prefs?.edit()?.putString(PREF_SEARCH_HISTORY, JSONArray(_searchHistory.value).toString())?.apply()
    }

    fun clearSearchHistory() {
        _searchHistory.value = emptyList()
        lastAutoRememberedQuery = null
        prefs?.edit()?.remove(PREF_SEARCH_HISTORY)?.apply()
    }

    fun authorizePlayback() = handle?.authorizePlayback()

    /**
     * Starts playing `tracks` from `startIndex`; the rest of the list becomes
     * the skip-next/previous queue. Shows the tapped track immediately —
     * the engine's own confirmation (album, art, real position) follows
     * shortly after via `onNowPlaying`.
     */
    fun playTracks(tracks: List<TrackItem>, startIndex: Int, context: PlayContext? = null) {
        val track = tracks.getOrNull(startIndex) ?: return
        // Any direct play starts a new queue outside the current radio —
        // onRadio sets this right back after calling playTracks itself,
        // so a radio's own start isn't immediately undone by this.
        _activeRadio.value = null
        context?.let { recordPlayContext(it) }
        val placeholder = PlayingTrack(
            uri = track.uri,
            title = track.name,
            artist = track.artist,
            artistId = track.artistId,
            album = "",
            albumId = track.albumId,
            artUrl = track.artUrl,
            durationMs = track.durationMs,
        )
        val remoteId = activeRemoteDeviceId()
        if (remoteId != null) {
            // Loading locally would call spirc.activate(), which forcibly
            // reclaims Connect control and stops the remote device instead
            // of handing it the new list — this has to go out to that
            // device's own session instead.
            remoteSnapshot = remoteSnapshot?.copy(isPlaying = true, positionMs = 0, track = placeholder)
            publishEffectivePlayback()
            handle?.remotePlayTracks(remoteId, tracks.map { it.uri }, startIndex.toUInt())
            pollRemoteSoon()
            return
        }
        localPlaySource = context?.toCurrentSource()
        engineState = LocalPlayback(
            phase = PlaybackPhase.Loading,
            track = placeholder,
            positionMs = 0,
            shuffle = engineState?.shuffle ?: false,
            repeat = engineState?.repeat ?: "Off",
            error = null,
        )
        publishEffectivePlayback()
        pendingTrackUri = track.uri
        scope.launch {
            delay(8_000)
            // Self-clearing safety net: if this uri never arrives (a
            // failed load that somehow reports no error), don't leave
            // onNowPlaying ignoring every future update forever.
            if (pendingTrackUri == track.uri) pendingTrackUri = null
        }
        handle?.playTracks(tracks.map { it.uri }, startIndex.toUInt())
    }

    private fun recordPlayContext(context: PlayContext) {
        val now = System.currentTimeMillis()
        val entry = when (context) {
            is PlayContext.Playlist -> RecentEntry(RecentKind.Playlist, context.id, context.name, "", context.artUrl, now)
            is PlayContext.Artist -> RecentEntry(RecentKind.Artist, context.id, context.name, "", context.artUrl, now)
            is PlayContext.Album -> RecentEntry(RecentKind.Album, context.id, context.name, context.artist, context.artUrl, now)
        }
        upsertRecentEntries(listOf(entry))
    }

    /** Merges [entries] into the feed: a (kind, id) already present only
     *  moves if the new timestamp is newer, so an older server-reported
     *  play can never bump a fresher local one backwards. */
    private fun upsertRecentEntries(entries: List<RecentEntry>) {
        if (entries.isEmpty()) return
        var feed = _recentlyPlayedFeed.value
        for (entry in entries) {
            val existingIndex = feed.indexOfFirst { it.kind == entry.kind && it.id == entry.id }
            feed = when {
                existingIndex == -1 -> feed + entry
                feed[existingIndex].timestampMs < entry.timestampMs -> feed.toMutableList().also { it[existingIndex] = entry }
                else -> feed
            }
        }
        feed = applyPinnedOrderAndCap(feed.sortedByDescending { it.timestampMs })
        _recentlyPlayedFeed.value = feed
        prefs?.edit()?.putString(PREF_RECENT_FEED, encodeRecentFeed(feed).toString())?.apply()
    }

    /** Pinned cards float first (in their own recency order) and are exempt
     *  from [RECENT_FEED_CAP] — a pinned card stays on Home until explicitly
     *  removed, not just until enough other plays bump it out. */
    private fun applyPinnedOrderAndCap(feed: List<RecentEntry>): List<RecentEntry> {
        val pinnedKeys = _pinnedRecentKeys.value
        val (pinned, unpinned) = feed.partition { pinnedKeys.contains(recentKey(it)) }
        return pinned + unpinned.take((RECENT_FEED_CAP - pinned.size).coerceAtLeast(0))
    }

    fun togglePlayback() {
        val remoteId = activeRemoteDeviceId()
        if (remoteId != null) {
            val playing = remoteSnapshot?.isPlaying ?: false
            if (playing) handle?.remotePause(remoteId) else handle?.remotePlay(remoteId)
            remoteSnapshot = remoteSnapshot?.copy(isPlaying = !playing)
            publishEffectivePlayback()
            return
        }
        val current = engineState ?: return
        engineState = current.copy(
            phase = if (current.phase == PlaybackPhase.Playing) PlaybackPhase.Paused else PlaybackPhase.Playing,
        )
        publishEffectivePlayback()
        handle?.togglePlayback()
    }

    /** A remote transport command has no optimistic update worth making
     *  (we don't know the next/previous track locally) — just nudge the
     *  next poll sooner than the regular interval so the change shows up
     *  in a beat rather than up to 6s later. */
    private fun pollRemoteSoon() {
        scope.launch {
            delay(500)
            handle?.playbackState()
        }
    }

    fun skipNext() {
        val remoteId = activeRemoteDeviceId()
        if (remoteId != null) {
            handle?.remoteNext(remoteId)
            pollRemoteSoon()
        } else {
            handle?.skipNext()
        }
    }

    fun skipPrevious() {
        val remoteId = activeRemoteDeviceId()
        if (remoteId != null) {
            handle?.remotePrevious(remoteId)
            pollRemoteSoon()
        } else {
            handle?.skipPrevious()
        }
    }

    /** Seeks within the current track — the local engine's own playback,
     *  or the active remote Connect device's, same split as
     *  [togglePlayback]. */
    fun seek(positionMs: Long) {
        val remoteId = activeRemoteDeviceId()
        if (remoteId != null) {
            handle?.remoteSeek(remoteId, positionMs.toUInt())
            remoteSnapshot = remoteSnapshot?.copy(positionMs = positionMs)
            publishEffectivePlayback()
            return
        }
        val current = engineState ?: return
        engineState = current.copy(positionMs = positionMs)
        publishEffectivePlayback()
        handle?.seek(positionMs.toUInt())
    }

    /** Looks up lyrics for [track]; the answer arrives in [lyrics]. Lazy —
     *  called only once the lyrics panel is actually opened, not on every
     *  track change, since most plays never open it. */
    fun fetchLyrics(track: PlayingTrack) {
        _lyrics.value = null
        handle?.lyrics(track.uri, track.artist, track.title, track.album, track.durationMs.toUInt())
    }

    /** Starts Spotify's radio seeded by [seed] (a track/album/artist/
     *  playlist uri) — resolved through the streaming session, not the Web
     *  API, so it works regardless of this app's API access tier. Starts
     *  playing immediately once resolved, and [activeRadio] reports
     *  [label] (e.g. "Future Islands Radio") while it's the active queue. */
    fun goToRadio(seed: String, label: String) {
        pendingRadioLabel = label
        localPlaySource = null
        handle?.radio(seed)
    }

    /** Creates a private Spotify playlist named [name] and adds [trackUris]
     *  to it; the combined outcome arrives via `onPlaylistCreated` as a
     *  [toast]. Only one creation can be in flight at a time — see
     *  [creatingPlaylist]; a call while one is already pending is ignored. */
    fun createPlaylist(name: String, trackUris: List<String> = emptyList()) {
        if (_creatingPlaylist.value) return
        _creatingPlaylist.value = true
        handle?.createPlaylist(name, trackUris)
    }

    /** [createPlaylist] populated from [albumId]'s tracks, already cached
     *  in [albumTracks] from viewing the album — the context menu this is
     *  reached from only opens once that album's tracks are showing. */
    fun createPlaylistFromAlbum(albumId: String, name: String) {
        val tracks = _albumTracks.value[albumId].orEmpty()
        createPlaylist(name, tracks.map { it.uri })
    }

    fun toggleShuffle() {
        val next = !(_localPlayback.value?.shuffle ?: false)
        _localPlayback.value = _localPlayback.value?.copy(shuffle = next)
        handle?.setShuffle(next)
    }

    fun cycleRepeat() {
        val next = when (_localPlayback.value?.repeat) {
            "Off" -> "Context"
            "Context" -> "Track"
            else -> "Off"
        }
        _localPlayback.value = _localPlayback.value?.copy(repeat = next)
        handle?.setRepeat(next)
    }

    /** Requests Liked Songs; the result arrives in [tracks] under [LIKED_SONGS_ID]. */
    fun likedSongs() {
        _tracksHasMore.value = _tracksHasMore.value - LIKED_SONGS_ID
        _tracksLoadingMore.value = _tracksLoadingMore.value - LIKED_SONGS_ID
        handle?.likedSongs()
    }

    /** Flips `uri`'s liked state immediately; the engine's confirmation
     *  follows via `onTrackSaved` moments later. */
    fun toggleLiked(uri: String) {
        setLiked(uri, !(_likedTracks.value[uri] ?: false))
    }

    /** Sets `uri`'s liked state directly, rather than toggling — the
     *  heart's own tap-to-like/hold-to-unlike split needs "like" and
     *  "unlike" as distinct actions, not opposite ends of one toggle. A
     *  no-op when `uri` is already in that state — a tap on an
     *  already-liked track shouldn't re-save it (or a hold on an
     *  already-unliked one re-remove it). */
    fun setLiked(uri: String, liked: Boolean) {
        if (_likedTracks.value[uri] == liked) return
        _likedTracks.value = _likedTracks.value + (uri to liked)
        handle?.setTrackSaved(uri, liked)
        appContext?.let { vibrateTick(it) }
    }

    /** Asks the engine whether `uri` is liked, unless already known. */
    fun checkLiked(uri: String) {
        if (_likedTracks.value.containsKey(uri)) return
        handle?.checkTrackSaved(uri)
    }

    /** Checks liked state for a whole track list at once — an artist's
     *  "Popular" tracks, a playlist, an album — so [TrackItem] rows can
     *  show a liked heart without each row checking for itself. Already-known
     *  uris are skipped; Spotify's `/me/library/contains` takes at most 50
     *  ids per call, so a longer list is chunked. */
    fun checkLikedBatch(uris: List<String>) {
        val unknown = uris.distinct().filterNot { _likedTracks.value.containsKey(it) }
        unknown.chunked(50).forEach { chunk -> handle?.checkTracksSaved(chunk) }
    }

    /** Requests the account's top tracks; the result arrives in [topTracks]. */
    fun loadTopTracks() = handle?.topTracks()

    /** Requests the account's top artists; the result arrives in [topArtists]. */
    fun loadTopArtists() = handle?.topArtists()

    /** Requests the account's saved albums; the result arrives in [savedAlbumItems]. */
    fun loadSavedAlbums() = handle?.savedAlbums()

    /** Requests the account's followed artists; the result arrives in [followedArtistItems]. */
    fun loadFollowedArtists() = handle?.followedArtists()

    /** Requests this week's new releases; the result arrives in [newReleases]. */
    fun loadNewReleases() = handle?.newReleases()

    /** Requests the available Spotify Connect devices; the result arrives
     *  in [devices]. */
    fun loadDevices() = handle?.devices()

    /** Switches playback to `deviceId`; confirmation arrives in [toast] and
     *  refreshes [devices]. */
    fun transferPlayback(deviceId: String) = handle?.transferPlayback(deviceId)

    /** Sets `deviceId`'s volume (0..100) through Spotify Connect — the
     *  only way to change a remote device's volume, since this phone's own
     *  volume controls only ever reach local playback. Updates [devices]
     *  optimistically so a dragged slider doesn't snap back before the
     *  next [loadDevices] poll confirms it. */
    fun setDeviceVolume(deviceId: String, percent: Int) {
        val clamped = percent.coerceIn(0, 100)
        _devices.value = _devices.value.map { if (it.id == deviceId) it.copy(volumePercent = clamped) else it }
        handle?.setDeviceVolume(deviceId, clamped.toUByte())
    }

    /** Requests the upcoming tracks in Spotify's queue; the result arrives
     *  in [queue]. */
    fun loadQueue() = handle?.queue()

    /** Queues `uri` after whatever's already queued; confirmation arrives
     *  in [toast] as a brief banner. */
    fun addToQueue(uri: String, label: String) = handle?.addToQueue(uri, label)

    /** Dismisses the current [toast] banner, once the UI has shown it. */
    fun clearToast() {
        _toast.value = null
    }

    /** Pins or unpins `id` (a playlist id or [LIKED_SONGS_ID]) at the top of
     *  Library. */
    fun togglePinned(id: String) {
        val next = if (_pinnedIds.value.contains(id)) _pinnedIds.value - id else _pinnedIds.value + id
        _pinnedIds.value = next
        prefs?.edit()?.putStringSet(PREF_PINNED_IDS, next)?.apply()
    }

    fun openRecentMenu(entry: RecentEntry) {
        _recentMenu.value = entry
    }

    fun closeRecentMenu() {
        _recentMenu.value = null
    }

    private fun recentKey(entry: RecentEntry) = "${entry.kind.name}:${entry.id}"

    /** Pins or unpins a Home card at the top of the recently-played feed,
     *  exempt from [RECENT_FEED_CAP] eviction while pinned. */
    fun togglePinnedRecent(entry: RecentEntry) {
        val key = recentKey(entry)
        val next = if (_pinnedRecentKeys.value.contains(key)) {
            _pinnedRecentKeys.value - key
        } else {
            _pinnedRecentKeys.value + key
        }
        _pinnedRecentKeys.value = next
        prefs?.edit()?.putStringSet(PREF_PINNED_RECENT_KEYS, next)?.apply()
        val feed = applyPinnedOrderAndCap(_recentlyPlayedFeed.value)
        _recentlyPlayedFeed.value = feed
        prefs?.edit()?.putString(PREF_RECENT_FEED, encodeRecentFeed(feed).toString())?.apply()
    }

    /** Drops a card from Home's recently-played feed — it reappears only if
     *  played again. Also unpins it, so a removed card can't come back
     *  pinned the next time something else is played from it. */
    fun removeRecentEntry(entry: RecentEntry) {
        val key = recentKey(entry)
        _recentlyPlayedFeed.value = _recentlyPlayedFeed.value.filterNot { it.kind == entry.kind && it.id == entry.id }
        prefs?.edit()?.putString(PREF_RECENT_FEED, encodeRecentFeed(_recentlyPlayedFeed.value).toString())?.apply()
        if (_pinnedRecentKeys.value.contains(key)) {
            val next = _pinnedRecentKeys.value - key
            _pinnedRecentKeys.value = next
            prefs?.edit()?.putStringSet(PREF_PINNED_RECENT_KEYS, next)?.apply()
        }
    }

    /** [entry]'s own tracks — the playlist's, the artist's top tracks, or
     *  the album's — fetching them first if nothing's cached yet (a fresh
     *  app start has an empty cache even for a card already in the feed).
     *  Null means the fetch timed out or came back empty. */
    private suspend fun resolveRecentTracks(entry: RecentEntry): List<TrackItem>? {
        val flow = when (entry.kind) {
            RecentKind.Playlist -> _tracks
            RecentKind.Artist -> _artistTracks
            RecentKind.Album -> _albumTracks
        }
        flow.value[entry.id]?.let { if (it.isNotEmpty()) return it }
        when (entry.kind) {
            RecentKind.Playlist -> if (entry.id == LIKED_SONGS_ID) handle?.likedSongs() else handle?.playlist(entry.id)
            RecentKind.Artist -> handle?.artistTopTracks(entry.id)
            RecentKind.Album -> handle?.albumTracks(entry.id)
        }
        return withTimeoutOrNull(5_000) {
            flow.filter { it[entry.id]?.isNotEmpty() == true }.first()[entry.id]
        }
    }

    private fun playContextFor(entry: RecentEntry): PlayContext = when (entry.kind) {
        RecentKind.Playlist -> PlayContext.Playlist(entry.id, entry.name, entry.artUrl)
        RecentKind.Artist -> PlayContext.Artist(entry.id, entry.name, entry.artUrl)
        RecentKind.Album -> PlayContext.Album(entry.id, entry.name, entry.subtitle, entry.artUrl)
    }

    /** Plays [entry] (a Home card) from the start — fetches its tracks
     *  first if they're not already cached. */
    fun playRecentEntry(entry: RecentEntry) {
        scope.launch {
            val tracks = resolveRecentTracks(entry) ?: return@launch
            playTracks(tracks, 0, playContextFor(entry))
        }
    }

    /** Queues every one of [entry]'s tracks — fetches them first if not
     *  already cached. */
    fun queueRecentEntry(entry: RecentEntry) {
        scope.launch {
            val tracks = resolveRecentTracks(entry) ?: return@launch
            addTracksToQueue(tracks, entry.name)
        }
    }

    /** Queues every track in [tracks] on the active device, showing one
     *  combined toast instead of [onQueueAdded] firing one per track. */
    private fun addTracksToQueue(tracks: List<TrackItem>, label: String) {
        if (tracks.isEmpty()) return
        queueBatchPending += tracks.size
        queueBatchLabel = label
        queueBatchSucceeded = 0
        tracks.forEach { handle?.addToQueue(it.uri, "") }
    }

    /** This install's own randomized Spotify Connect name
     *  (`spotifast-<word>`, generated once and cached on the Rust side) —
     *  distinct from a desktop Spotifast instance's fixed "Spotifast", so
     *  the two don't show up as the same device in Connect's picker. */
    val localDeviceName: String
        get() = handle?.deviceName() ?: "Spotifast"

    /** The streaming quality the engine will use the next time it starts a
     *  session — changing this does not affect the one already running. */
    fun audioQuality(): AudioQuality {
        val kbps = prefs?.getInt(PREF_BITRATE_KBPS, AudioQuality.Normal.kbps) ?: AudioQuality.Normal.kbps
        return AudioQuality.entries.firstOrNull { it.kbps == kbps } ?: AudioQuality.Normal
    }

    fun setAudioQuality(quality: AudioQuality) {
        prefs?.edit()?.putInt(PREF_BITRATE_KBPS, quality.kbps)?.apply()
    }

    fun setAppTheme(theme: AppTheme) {
        _appTheme.value = theme
        prefs?.edit()?.putString(PREF_APP_THEME, theme.name)?.apply()
    }

    fun setInkOutlineEnabled(enabled: Boolean) {
        _inkOutlineEnabled.value = enabled
        prefs?.edit()?.putBoolean(PREF_INK_OUTLINE, enabled)?.apply()
    }

    fun setLibrarySort(sort: LibrarySort) {
        _librarySort.value = sort
        prefs?.edit()?.putString(PREF_LIBRARY_SORT, sort.name)?.apply()
        // Fetched on demand rather than eagerly at sign-in: this is a Web
        // API round trip most sessions never need. RecentlyUpdated needs no
        // fetch of its own — it only reads whatever playlists are already
        // in [tracks] from being opened.
        if (sort == LibrarySort.RecentlyPlayed) handle?.recentlyPlayed()
    }

    /** Clears every cache the app keeps: the on-disk playlist track cache
     *  (Rust's half), Coil's cover-art memory/disk cache, and the
     *  in-memory "permanently unavailable" playlist list — so anything
     *  that failed once (a transient error, not just a real 404) gets a
     *  fresh attempt instead of staying hidden until the app restarts. */
    fun clearCache() {
        handle?.clearPlaylistCache()
        handle?.clearAudioCache()
        appContext?.let { context ->
            val imageLoader = coil3.SingletonImageLoader.get(context)
            imageLoader.memoryCache?.clear()
            runCatching { imageLoader.diskCache?.clear() }
        }
        unavailablePlaylistIds.clear()
        requestedPlaylistInfoIds.clear()
        rawRootlistRows = emptyList()
        rebuildLibraryTree()
        _toast.value = "Cache cleared"
        loadLibraryTree()
    }

    fun openTrackMenu(track: TrackItem) {
        _trackMenu.value = track
    }

    fun closeTrackMenu() {
        _trackMenu.value = null
    }

    fun openLibraryMenu(item: LibraryItem) {
        _libraryMenu.value = item
    }

    fun closeLibraryMenu() {
        _libraryMenu.value = null
    }

    fun openAlbumMenu(album: AlbumItem) {
        _albumMenu.value = album
    }

    fun closeAlbumMenu() {
        _albumMenu.value = null
    }
}

private const val PREFS_NAME = "spotifast_prefs"
private const val PREF_PINNED_IDS = "pinned_ids"
private const val PREF_PINNED_RECENT_KEYS = "pinned_recent_keys"
private const val PREF_BITRATE_KBPS = "bitrate_kbps"
private const val PREF_APP_THEME = "app_theme"
private const val PREF_INK_OUTLINE = "ink_outline_enabled"
private const val PREF_LIBRARY_SORT = "library_sort"
private const val PREF_SEARCH_HISTORY = "search_history"
private const val PREF_RECENT_FEED = "recent_feed"
private const val RECENT_FEED_CAP = 30

/** Parses the `{ "uri", "name", "artist", "duration_ms", "art_url" }` shape
 *  shared by playlist tracks, search tracks, top tracks, and the queue. */
private fun parseTrackItems(json: String): List<TrackItem> = runCatching {
    val array = JSONArray(json)
    (0 until array.length()).map { index ->
        val item = array.getJSONObject(index)
        TrackItem(
            item.getString("uri"),
            item.getString("name"),
            item.getString("artist"),
            item.getLong("duration_ms"),
            item.nullableString("art_url"),
            item.nullableString("added_at"),
            item.nullableString("artist_id"),
            item.nullableString("album_id"),
        )
    }
}.getOrDefault(emptyList())

/** Parses the `{ "id", "name", "art_url" }` shape shared by search artists
 *  and top artists. */
private fun parseArtistItems(json: String): List<ArtistItem> = runCatching {
    val array = JSONArray(json)
    (0 until array.length()).map { index ->
        val item = array.getJSONObject(index)
        ArtistItem(item.getString("id"), item.getString("name"), item.nullableString("art_url"))
    }
}.getOrDefault(emptyList())

/** Parses the `{ "id", "name", "artist", "art_url" }` shape shared by
 *  search albums and an artist's albums. */
private fun parseAlbumItems(json: String): List<AlbumItem> = runCatching {
    val array = JSONArray(json)
    (0 until array.length()).map { index ->
        val item = array.getJSONObject(index)
        AlbumItem(
            item.getString("id"),
            item.getString("name"),
            item.getString("artist"),
            item.nullableString("art_url"),
        )
    }
}.getOrDefault(emptyList())

/** Parses the `{ "id", "name", "art_url" }` shape shared by the library
 *  list and search playlists. */
private fun parseLibraryItems(json: String): List<LibraryItem> = runCatching {
    val array = JSONArray(json)
    (0 until array.length()).map { index ->
        val item = array.getJSONObject(index)
        LibraryItem(item.getString("id"), item.getString("name"), item.nullableString("art_url"))
    }
}.getOrDefault(emptyList())

/** One parsed `on_rootlist` row: `kind` is `"playlist"`, `"folder_start"`,
 *  or `"folder_end"`; `id`/`name` are present only where that kind carries
 *  them (see `android/rust/src/lib.rs`'s `RootlistRow`). */
private data class RawRootlistRow(val kind: String, val id: String?, val name: String?)

/** Folds the flat, bracket-matched rootlist rows into a [LibraryNode] tree,
 *  filling in each playlist's name/art from [playlistsById] — the rootlist
 *  itself only has bare ids. An unmatched `folder_end` (shouldn't happen;
 *  the Rust side already closes any folder Spotify left open) is ignored
 *  rather than collapsing something above the root. */
private fun buildLibraryTree(rows: List<RawRootlistRow>, playlistsById: Map<String, LibraryItem>): List<LibraryNode> {
    val root = mutableListOf<LibraryNode>()
    val stack = ArrayDeque<MutableList<LibraryNode>>()
    val folderMeta = ArrayDeque<Pair<String, String>>()
    stack.addLast(root)
    for (row in rows) {
        when (row.kind) {
            "playlist" -> {
                val id = row.id ?: continue
                val item = playlistsById[id] ?: LibraryItem(id, id, null)
                stack.last().add(LibraryNode.PlaylistNode(item))
            }
            "folder_start" -> {
                stack.addLast(mutableListOf())
                folderMeta.addLast((row.id ?: "") to (row.name ?: ""))
            }
            "folder_end" -> {
                if (stack.size > 1) {
                    val children = stack.removeLast()
                    val (id, name) = folderMeta.removeLast()
                    stack.last().add(LibraryNode.FolderNode(id, name, children))
                }
            }
        }
    }
    // Defensive only — the Rust side always closes trailing open folders.
    while (stack.size > 1) {
        val children = stack.removeLast()
        val (id, name) = folderMeta.removeLast()
        stack.last().add(LibraryNode.FolderNode(id, name, children))
    }
    return root
}

/** Every playlist in a [LibraryNode] tree, folders flattened out — for
 *  filtering/searching the tree without walking it recursively each time. */
fun List<LibraryNode>.flattenPlaylists(): List<LibraryItem> = flatMap { node ->
    when (node) {
        is LibraryNode.PlaylistNode -> listOf(node.item)
        is LibraryNode.FolderNode -> node.children.flattenPlaylists()
    }
}

/** Orders [items] by [sort]. [tracks] and [ranks] are read as plain
 *  snapshots rather than collected here, so this stays a plain function
 *  callable from a `remember` block on either Home or Library screen.
 *  [LibrarySort.Custom] returns [items] untouched — callers already have
 *  it in the order they want (the rootlist tree, or the flat Web API
 *  order). */
fun sortLibraryItems(
    items: List<LibraryItem>,
    sort: LibrarySort,
    tracks: Map<String, List<TrackItem>>,
    ranks: Map<String, Int>,
): List<LibraryItem> = when (sort) {
    LibrarySort.Custom -> items
    LibrarySort.Alphabetical -> items.sortedBy { it.name.lowercase() }
    LibrarySort.RecentlyPlayed -> items.sortedBy { ranks[it.id] ?: Int.MAX_VALUE }
    // The newest `added_at` among a playlist's cached tracks — a real but
    // partial signal (misses removes/reorders/renames) for "last changed",
    // and only known once the playlist has been opened at least once. An
    // uncached playlist sorts last, next to "" (ISO-8601 timestamps all
    // sort later than an empty string).
    LibrarySort.RecentlyUpdated -> items.sortedByDescending { item ->
        tracks[item.id]?.mapNotNull { it.addedAt }?.maxOrNull() ?: ""
    }
}

/** Floats Liked Songs to the very top, overriding every other ordering —
 *  sort, pin, or filter alike. Real Spotify always leads with it no matter
 *  how the rest of the library is arranged; apply this last, after any
 *  other ordering pass, so nothing can push it back down. */
fun pinLikedSongsFirst(items: List<LibraryItem>): List<LibraryItem> =
    items.sortedByDescending { it.id == SpotifyCore.LIKED_SONGS_ID }

/** Serializes Home's recently-played feed for [PREF_RECENT_FEED]. */
private fun encodeRecentFeed(feed: List<RecentEntry>): JSONArray {
    val array = JSONArray()
    for (entry in feed) {
        val obj = org.json.JSONObject()
        obj.put("kind", entry.kind.name)
        obj.put("id", entry.id)
        obj.put("name", entry.name)
        obj.put("subtitle", entry.subtitle)
        obj.put("artUrl", entry.artUrl)
        obj.put("timestampMs", entry.timestampMs)
        array.put(obj)
    }
    return array
}

/** The inverse of [encodeRecentFeed]; an entry whose [RecentKind] no longer
 *  matches a known value (shouldn't happen, but a future rename shouldn't
 *  crash on old prefs either) is dropped rather than failing the whole load. */
private fun loadRecentFeed(prefs: SharedPreferences): List<RecentEntry> = runCatching {
    val array = JSONArray(prefs.getString(PREF_RECENT_FEED, null) ?: "[]")
    (0 until array.length()).mapNotNull { index ->
        val obj = array.getJSONObject(index)
        val kind = RecentKind.entries.firstOrNull { it.name == obj.getString("kind") } ?: return@mapNotNull null
        RecentEntry(
            kind = kind,
            id = obj.getString("id"),
            name = obj.getString("name"),
            subtitle = obj.optString("subtitle", ""),
            artUrl = obj.nullableString("artUrl"),
            timestampMs = obj.getLong("timestampMs"),
        )
    }
}.getOrDefault(emptyList())

/** `org.json` has no nullable-string accessor: a JSON `null` isn't a missing
 *  key, it's the `JSONObject.NULL` sentinel, which `optString` stringifies
 *  to the literal text "null" instead of returning null. */
private fun org.json.JSONObject.nullableString(name: String): String? =
    if (has(name) && !isNull(name)) getString(name) else null

private fun org.json.JSONObject.nullableInt(name: String): Int? =
    if (has(name) && !isNull(name)) getInt(name) else null
