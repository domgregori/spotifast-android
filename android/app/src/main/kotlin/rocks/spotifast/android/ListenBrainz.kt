package rocks.spotifast.android

import android.content.Context
import android.content.Intent
import android.net.Uri
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder

/**
 * Scrobbles local playback to [ListenBrainz](https://listenbrainz.org) — the
 * open-source listening-history service, independent of Spotify's own (now
 * Development-Mode-restricted) recommendations. ListenBrainz has no OAuth
 * flow for third-party apps; every client authenticates with a user token
 * copied from the user's own settings page, same as this one does.
 *
 * Entirely separate from [SpotifyCore] and the Rust core on purpose — this
 * has nothing to do with Spotify playback itself, just observing it, so it
 * stays out of the shared `core/` crate instead of adding an unrelated
 * dependency there.
 */
data class SimilarArtist(val name: String, val mbid: String, val score: Int)

data class RecentListen(val trackName: String, val artistName: String, val listenedAtMs: Long)

/** One track inside a ListenBrainz-generated/fetched playlist — the shared
 *  shape [parseJspf] produces from every JSPF response (`createdfor`'s
 *  per-playlist fetch, and `lb-radio`'s result). [recordingMbid] is null
 *  only for a track whose `identifier` isn't a `musicbrainz.org/recording/`
 *  url (rare, but the JSPF spec allows other identifier schemes).
 *  [spotifyUri] is filled in lazily by [matchToSpotify], not by the parser. */
data class GeneratedTrack(
    val title: String,
    val artist: String,
    val album: String?,
    val durationMs: Long,
    val recordingMbid: String?,
    val caaReleaseMbid: String?,
    val spotifyUri: String? = null,
)

/** A playlist fetched or generated from ListenBrainz — `createdfor`'s
 *  Weekly Jams/Exploration, or an `lb-radio` result. */
data class GeneratedPlaylist(val title: String, val tracks: List<GeneratedTrack>)

/** One of `createdfor`'s listed playlists, without its tracks — enough to
 *  show in a grid and tell Jams apart from Exploration via [sourcePatch];
 *  [playlistByMbid] fetches the actual tracks lazily. [date] is the ISO
 *  datetime ListenBrainz generated it at, straight from the API (not
 *  parsed out of [title]). */
data class CreatedForEntry(val mbid: String, val title: String, val sourcePatch: String?, val date: String?)

/** One global fresh-releases entry. [spotifyAlbumId] is never set by the
 *  parser — [ListenBrainz.resolveFreshReleaseSpotifyId] fills it in lazily,
 *  only for a release the user actually taps. */
data class FreshRelease(
    val title: String,
    val artist: String,
    val releaseDate: String,
    val releaseMbid: String,
    val artistMbids: List<String>,
    val caaReleaseMbid: String?,
    val spotifyAlbumId: String? = null,
)

object ListenBrainz {
    const val SETTINGS_URL = "https://listenbrainz.org/settings/"
    private const val SUBMIT_URL = "https://api.listenbrainz.org/1/submit-listens"
    private const val VALIDATE_URL = "https://api.listenbrainz.org/1/validate-token"
    private const val USER_BASE_URL = "https://api.listenbrainz.org/1/user"
    private const val STATS_BASE_URL = "https://api.listenbrainz.org/1/stats/user"
    private const val PLAYLIST_BASE_URL = "https://api.listenbrainz.org/1/playlist"
    private const val LB_RADIO_URL = "https://api.listenbrainz.org/1/explore/lb-radio"
    private const val JSPF_TRACK_EXTENSION_KEY = "https://musicbrainz.org/doc/jspf#track"
    private const val RECORDING_PREFIX = "https://musicbrainz.org/recording/"
    private const val SPOTIFY_TRACK_PREFIX = "https://open.spotify.com/track/"
    private val WEEK_DAYS =
        listOf("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday")
    private const val SIMILAR_ARTISTS_ALGORITHM =
        "session_based_days_7500_session_300_contribution_5_threshold_10_limit_100_filter_True_skip_30"
    private const val APP_USER_AGENT = "Spotifast/1.0 ( https://github.com/crmne/spotifast )"
    private const val TOKEN_KEY = "listenbrainz_token"
    private const val PREFS_NAME = "listenbrainz_prefs"
    private const val PREF_SCROBBLE_ENABLED = "scrobble_enabled"
    private const val PREF_USER_NAME = "user_name"

    /** A track only earns a scrobble once it's played this long — half its
     *  own length, or this, whichever is lower; ListenBrainz's own rule. */
    private const val MAX_THRESHOLD_MS = 4 * 60 * 1000L

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private lateinit var credentials: EncryptedCredentialStore
    private lateinit var prefs: android.content.SharedPreferences

    private var token: String? = null
    private var scrobbleEnabled = false
    private var connectedUserName: String? = null

    // Wall-clock accounting for the currently-loaded track: how long it's
    // actually been in the Playing phase (paused time doesn't count), so a
    // track-change can retroactively decide whether the one just left
    // behind earned a scrobble. An approximation of real position
    // tracking — good enough for scrobbling, not meant to be exact.
    private var trackedUri: String? = null
    private var trackedTrack: PlayingTrack? = null
    private var wasPlaying = false
    private var lastChangeAtMs = 0L
    private var accumulatedPlayingMs = 0L

    fun init(context: Context) {
        credentials = EncryptedCredentialStore(context.applicationContext)
        prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        token = credentials.read(TOKEN_KEY)?.toString(Charsets.UTF_8)
        scrobbleEnabled = prefs.getBoolean(PREF_SCROBBLE_ENABLED, false)
        connectedUserName = prefs.getString(PREF_USER_NAME, null)
    }

    val isConnected: Boolean get() = token != null
    val userName: String? get() = connectedUserName
    fun isScrobbleEnabled(): Boolean = scrobbleEnabled

    fun openTokenPage(context: Context) {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(SETTINGS_URL)))
    }

    /** Validates `pasted` against ListenBrainz, storing it only if it's
     *  real. [onResult] carries the resolved username on success. */
    fun connect(pasted: String, onResult: (Result<String>) -> Unit) {
        val trimmed = pasted.trim()
        if (trimmed.isEmpty()) {
            onResult(Result.failure(IllegalArgumentException("Paste a token first.")))
            return
        }
        scope.launch {
            val result = runCatching {
                val connection = URL(VALIDATE_URL).openConnection() as HttpURLConnection
                connection.setRequestProperty("User-Agent", APP_USER_AGENT)
                connection.setRequestProperty("Authorization", "Token $trimmed")
                connection.connectTimeout = 10_000
                connection.readTimeout = 10_000
                val body = connection.inputStream.bufferedReader().use { it.readText() }
                connection.disconnect()
                val json = JSONObject(body)
                if (!json.optBoolean("valid", false)) {
                    error("ListenBrainz says that token isn't valid.")
                }
                json.getString("user_name")
            }
            result.onSuccess { name ->
                credentials.write(TOKEN_KEY, trimmed.toByteArray(Charsets.UTF_8))
                token = trimmed
                connectedUserName = name
                prefs.edit().putString(PREF_USER_NAME, name).apply()
                setScrobbleEnabled(true)
            }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    fun disconnect() {
        credentials.delete(TOKEN_KEY)
        token = null
        connectedUserName = null
        prefs.edit().remove(PREF_USER_NAME).apply()
        setScrobbleEnabled(false)
    }

    fun setScrobbleEnabled(enabled: Boolean) {
        scrobbleEnabled = enabled
        prefs.edit().putBoolean(PREF_SCROBBLE_ENABLED, enabled).apply()
    }

    /** Artists similar to [spotifyArtistId], via ListenBrainz's own
     *  similarity graph — Spotify's own `/related-artists` 404s for this
     *  app's client id (Development Mode), and ListenBrainz's data is keyed
     *  by MusicBrainz ID rather than Spotify's, so this resolves the seed
     *  artist's MBID via MusicBrainz's reverse URL lookup first. Each result
     *  only carries a name and MBID — [resolveSpotifyArtistId] maps one back
     *  to a playable Spotify artist lazily, only when tapped. */
    fun similarArtists(spotifyArtistId: String, onResult: (Result<List<SimilarArtist>>) -> Unit) {
        scope.launch {
            val result = runCatching {
                val seedMbid = mbidForSpotifyArtist(spotifyArtistId)
                    ?: error("This artist isn't linked on MusicBrainz, so ListenBrainz has nothing to compare it to.")
                fetchSimilarArtists(seedMbid)
            }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    /** Maps a similar artist's MBID back to a Spotify artist id, via
     *  MusicBrainz's own link to that artist's Spotify page — exact when
     *  the link exists, `null` (not a guess) when it doesn't. */
    fun resolveSpotifyArtistId(mbid: String, onResult: (String?) -> Unit) {
        scope.launch {
            val id = runCatching { spotifyArtistIdForMbid(mbid) }.getOrNull()
            withContext(Dispatchers.Main) { onResult(id) }
        }
    }

    /** The MBID of the MusicBrainz artist linked from [spotifyArtistId]'s
     *  Spotify page — exact when the link exists, `null` when it doesn't.
     *  Public since both the artist context menu's LB Radio entry and the
     *  Generator tab's artist picker need a precise seed MBID rather than
     *  a (spellable, ambiguous) artist name. */
    fun mbidForSpotifyArtist(spotifyArtistId: String): String? {
        return mbidForSpotifyArtistBlocking(spotifyArtistId)
    }

    /** Async wrapper for callers outside a coroutine, like a Compose click
     *  handler — [mbidForSpotifyArtist] itself blocks on network I/O. */
    fun mbidForSpotifyArtistAsync(spotifyArtistId: String, onResult: (String?) -> Unit) {
        scope.launch {
            val mbid = runCatching { mbidForSpotifyArtistBlocking(spotifyArtistId) }.getOrNull()
            withContext(Dispatchers.Main) { onResult(mbid) }
        }
    }

    private fun mbidForSpotifyArtistBlocking(spotifyArtistId: String): String? {
        val resource = URLEncoder.encode("https://open.spotify.com/artist/$spotifyArtistId", "UTF-8")
        val json = getJson("https://musicbrainz.org/ws/2/url?resource=$resource&inc=artist-rels&fmt=json") ?: return null
        val relations = json.optJSONArray("relations") ?: return null
        for (i in 0 until relations.length()) {
            val id = relations.getJSONObject(i).optJSONObject("artist")?.optString("id")
            if (!id.isNullOrBlank()) return id
        }
        return null
    }

    private fun spotifyArtistIdForMbid(mbid: String): String? {
        val json = getJson("https://musicbrainz.org/ws/2/artist/$mbid?inc=url-rels&fmt=json") ?: return null
        val relations = json.optJSONArray("relations") ?: return null
        for (i in 0 until relations.length()) {
            val resource = relations.getJSONObject(i).optJSONObject("url")?.optString("resource") ?: continue
            if (resource.startsWith("https://open.spotify.com/artist/")) {
                return resource.removePrefix("https://open.spotify.com/artist/").trimEnd('/')
            }
        }
        return null
    }

    private fun fetchSimilarArtists(mbid: String): List<SimilarArtist> {
        val url = "https://labs.api.listenbrainz.org/similar-artists/json" +
            "?artist_mbids=$mbid&algorithm=$SIMILAR_ARTISTS_ALGORITHM"
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val array = JSONArray(body)
        return (0 until array.length()).mapNotNull { i ->
            val obj = array.getJSONObject(i)
            val name = obj.optString("name").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            SimilarArtist(name, obj.optString("artist_mbid"), obj.optInt("score"))
        }.sortedByDescending { it.score }.take(15)
    }

    /** The signed-in user's own listen history, newest first, straight from
     *  [GET /1/user/{user}/listens](https://listenbrainz.readthedocs.io/en/latest/users/api/core.html) —
     *  sent with the stored token so it still works if the user's LB
     *  profile is set to private. */
    fun recentListens(onResult: (Result<List<RecentListen>>) -> Unit) {
        val user = connectedUserName
        val tok = token
        if (user == null || tok == null) {
            onResult(Result.failure(IllegalStateException("Not connected to ListenBrainz.")))
            return
        }
        scope.launch {
            val result = runCatching { fetchRecentListens(user, tok) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchRecentListens(userName: String, tok: String): List<RecentListen> {
        val encodedUser = URLEncoder.encode(userName, "UTF-8")
        val connection = URL("$USER_BASE_URL/$encodedUser/listens?count=25").openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        connection.setRequestProperty("Authorization", "Token $tok")
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val listens = JSONObject(body).getJSONObject("payload").getJSONArray("listens")
        return (0 until listens.length()).mapNotNull { i ->
            val listen = listens.getJSONObject(i)
            val metadata = listen.optJSONObject("track_metadata") ?: return@mapNotNull null
            val trackName = metadata.optString("track_name").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            val artistName = metadata.optString("artist_name")
            RecentListen(trackName, artistName, listen.optLong("listened_at") * 1000L)
        }
    }

    /** Hour x day-of-week listening density for the signed-in user, from
     *  `GET /1/stats/user/{user}/daily-activity` — `result[day]` has 24
     *  entries, hour 0 through 23. [range] is one of ListenBrainz's own
     *  `AllowedStatisticsRange` values (`"week"`, `"month"`, `"year"`,
     *  `"all_time"`). Public (no token required), but sent anyway so it
     *  still works if the user's own profile is set to private. */
    fun dailyActivity(range: String, onResult: (Result<Map<String, List<Int>>>) -> Unit) {
        val user = connectedUserName
        if (user == null) {
            onResult(Result.failure(IllegalStateException("Not connected to ListenBrainz.")))
            return
        }
        scope.launch {
            val result = runCatching { fetchDailyActivity(user, range) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchDailyActivity(userName: String, range: String): Map<String, List<Int>> {
        val encodedUser = URLEncoder.encode(userName, "UTF-8")
        val url = "$STATS_BASE_URL/$encodedUser/daily-activity?range=$range"
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        token?.let { connection.setRequestProperty("Authorization", "Token $it") }
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val daily = JSONObject(body).getJSONObject("payload").getJSONObject("daily_activity")
        return WEEK_DAYS.associateWith { day ->
            val hours = IntArray(24)
            daily.optJSONArray(day)?.let { array ->
                for (i in 0 until array.length()) {
                    val entry = array.getJSONObject(i)
                    val hour = entry.optInt("hour")
                    if (hour in 0..23) hours[hour] = entry.optInt("listen_count")
                }
            }
            hours.toList()
        }
    }

    /** Whether the shared generated-playlist results sheet — reached from
     *  the artist context menu's "Generate ListenBrainz playlist", the
     *  Generator tab, and Weekly Jams/Exploration — is open. */
    private val _generatedPlaylistOpen = MutableStateFlow(false)
    val generatedPlaylistOpen: StateFlow<Boolean> = _generatedPlaylistOpen.asStateFlow()

    /** The sheet's current content: `null` while [fetch] is still in
     *  flight, matching the loading/error/list three-way pattern already
     *  used for Similar Artists. */
    private val _generatedPlaylistResult = MutableStateFlow<Result<GeneratedPlaylist>?>(null)
    val generatedPlaylistResult: StateFlow<Result<GeneratedPlaylist>?> = _generatedPlaylistResult.asStateFlow()

    /** Opens the shared results sheet in its loading state, then runs
     *  [fetch] (e.g. [lbRadio] or [playlistByMbid]) to populate it —
     *  every source funnels through the same sheet since they all produce
     *  the same [GeneratedPlaylist] shape. */
    fun openGeneratedPlaylist(fetch: ((onResult: (Result<GeneratedPlaylist>) -> Unit) -> Unit)) {
        _generatedPlaylistOpen.value = true
        _generatedPlaylistResult.value = null
        fetch { result -> _generatedPlaylistResult.value = result }
    }

    fun closeGeneratedPlaylist() {
        _generatedPlaylistOpen.value = false
        _generatedPlaylistResult.value = null
    }

    /** `createdfor`'s listed playlists for the signed-in user, metadata
     *  only (no tracks) — [playlistByMbid] fetches one's actual tracks. */
    fun createdForPlaylists(onResult: (Result<List<CreatedForEntry>>) -> Unit) {
        val user = connectedUserName
        if (user == null) {
            onResult(Result.failure(IllegalStateException("Not connected to ListenBrainz.")))
            return
        }
        scope.launch {
            val result = runCatching { fetchCreatedForPlaylists(user) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchCreatedForPlaylists(userName: String): List<CreatedForEntry> {
        val encodedUser = URLEncoder.encode(userName, "UTF-8")
        val url = "$USER_BASE_URL/$encodedUser/playlists/createdfor"
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        token?.let { connection.setRequestProperty("Authorization", "Token $it") }
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val playlists = JSONObject(body).getJSONArray("playlists")
        return (0 until playlists.length()).mapNotNull { i ->
            val playlist = playlists.getJSONObject(i).optJSONObject("playlist") ?: return@mapNotNull null
            val identifier = playlist.optString("identifier").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            val mbid = identifier.substringAfterLast('/')
            val title = playlist.optString("title").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            val sourcePatch = playlist
                .optJSONObject("extension")
                ?.optJSONObject("https://musicbrainz.org/doc/jspf#playlist")
                ?.optJSONObject("additional_metadata")
                ?.optJSONObject("algorithm_metadata")
                ?.optString("source_patch")
            val date = playlist.optString("date").takeIf { it.isNotBlank() }
            CreatedForEntry(mbid, title, sourcePatch, date)
        }
    }

    /** One `createdfor`-listed playlist's actual tracks, by mbid. */
    fun playlistByMbid(playlistMbid: String, onResult: (Result<GeneratedPlaylist>) -> Unit) {
        scope.launch {
            val result = runCatching { fetchPlaylistJspf(playlistMbid) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchPlaylistJspf(playlistMbid: String): GeneratedPlaylist {
        val connection = URL("$PLAYLIST_BASE_URL/$playlistMbid").openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        token?.let { connection.setRequestProperty("Authorization", "Token $it") }
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        return parseJspf(JSONObject(body))
    }

    /** Generates a playlist from an LB Radio prompt — `entity:(values)`
     *  terms per https://troi.readthedocs.io/en/latest/lb_radio.html,
     *  e.g. `artist:(${mbid})` or `tag:(deep house)` or `stats:username`.
     *  [mode] is `"easy"`, `"medium"`, or `"hard"`. */
    fun lbRadio(prompt: String, mode: String, onResult: (Result<GeneratedPlaylist>) -> Unit) {
        val tok = token
        if (tok == null) {
            onResult(Result.failure(IllegalStateException("Not connected to ListenBrainz.")))
            return
        }
        scope.launch {
            val result = runCatching { fetchLbRadio(tok, prompt, mode) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchLbRadio(tok: String, prompt: String, mode: String): GeneratedPlaylist {
        val encodedPrompt = URLEncoder.encode(prompt, "UTF-8")
        val url = "$LB_RADIO_URL?prompt=$encodedPrompt&mode=$mode"
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        connection.setRequestProperty("Authorization", "Token $tok")
        connection.connectTimeout = 10_000
        connection.readTimeout = 15_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val jspf = JSONObject(body).getJSONObject("payload").getJSONObject("jspf")
        return parseJspf(jspf)
    }

    /** Parses one JSPF payload (`{"playlist": {"title", "track": [...]}}`)
     *  into a [GeneratedPlaylist] — the one parser for both `createdfor`'s
     *  per-playlist fetch and `lb-radio`'s result, confirmed identical
     *  shape for both. */
    private fun parseJspf(root: JSONObject): GeneratedPlaylist {
        val playlist = root.getJSONObject("playlist")
        val title = playlist.optString("title").ifBlank { "Playlist" }
        val tracks = playlist.optJSONArray("track") ?: JSONArray()
        val parsed = (0 until tracks.length()).mapNotNull { i ->
            val track = tracks.getJSONObject(i)
            val trackTitle = track.optString("title").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            val identifiers = track.optJSONArray("identifier")
            val recordingMbid = (0 until (identifiers?.length() ?: 0))
                .map { identifiers!!.getString(it) }
                .firstOrNull { it.startsWith(RECORDING_PREFIX) }
                ?.removePrefix(RECORDING_PREFIX)
                ?.trimEnd('/')
            val trackExtension = track.optJSONObject("extension")?.optJSONObject(JSPF_TRACK_EXTENSION_KEY)
            val caaReleaseMbid = trackExtension
                ?.optJSONObject("additional_metadata")
                ?.optString("caa_release_mbid")
                ?.takeIf { it.isNotBlank() }
            GeneratedTrack(
                title = trackTitle,
                artist = track.optString("creator"),
                album = track.optString("album").takeIf { it.isNotBlank() },
                durationMs = track.optLong("duration"),
                recordingMbid = recordingMbid,
                caaReleaseMbid = caaReleaseMbid,
            )
        }
        return GeneratedPlaylist(title, parsed)
    }

    /** Resolves as many of [tracks]' recording MBIDs to Spotify track uris
     *  as MusicBrainz has url-rels for, via the same mechanism already
     *  proven for artist matching ([spotifyArtistIdForMbid]) — exact when
     *  the relation exists, left unmatched (not a guess) when it doesn't.
     *  Sequential, one MBID lookup at a time; [onProgress] lets the caller
     *  show "Matching 12/50…" instead of one long blocking wait. */
    fun matchToSpotify(
        tracks: List<GeneratedTrack>,
        onProgress: (matched: Int, total: Int) -> Unit,
        onResult: (List<GeneratedTrack>) -> Unit,
    ) {
        scope.launch {
            val total = tracks.size
            val resolved = tracks.mapIndexed { index, track ->
                val uri = track.recordingMbid?.let { mbid ->
                    runCatching { spotifyTrackIdForRecordingMbid(mbid) }.getOrNull()
                }
                val next = if (uri != null) track.copy(spotifyUri = "spotify:track:$uri") else track
                withContext(Dispatchers.Main) { onProgress(index + 1, total) }
                next
            }
            withContext(Dispatchers.Main) { onResult(resolved) }
        }
    }

    private fun spotifyTrackIdForRecordingMbid(mbid: String): String? {
        val json = getJson("https://musicbrainz.org/ws/2/recording/$mbid?inc=url-rels&fmt=json") ?: return null
        val relations = json.optJSONArray("relations") ?: return null
        for (i in 0 until relations.length()) {
            val resource = relations.getJSONObject(i).optJSONObject("url")?.optString("resource") ?: continue
            if (resource.startsWith(SPOTIFY_TRACK_PREFIX)) {
                return resource.removePrefix(SPOTIFY_TRACK_PREFIX).trimEnd('/')
            }
        }
        return null
    }

    /** Saves [playlist] to the user's own ListenBrainz account via
     *  `POST /1/playlist/create`, straight from the original JSPF track
     *  list (by recording MBID) — no Spotify matching involved at all,
     *  since ListenBrainz playlists are MBID-native. Tracks with no MBID
     *  are left out (shouldn't normally happen — every track [parseJspf]
     *  accepted came from ListenBrainz itself). */
    fun savePlaylistToListenBrainz(playlist: GeneratedPlaylist, title: String, onResult: (Result<Unit>) -> Unit) {
        val tok = token
        if (tok == null) {
            onResult(Result.failure(IllegalStateException("Not connected to ListenBrainz.")))
            return
        }
        scope.launch {
            val result = runCatching { createListenBrainzPlaylist(tok, title, playlist.tracks) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun createListenBrainzPlaylist(tok: String, title: String, tracks: List<GeneratedTrack>) {
        val trackArray = JSONArray()
        tracks.forEach { track ->
            track.recordingMbid?.let { mbid ->
                trackArray.put(
                    JSONObject().apply {
                        put("identifier", JSONArray().put("$RECORDING_PREFIX$mbid"))
                    },
                )
            }
        }
        val body = JSONObject().apply {
            put(
                "playlist",
                JSONObject().apply {
                    put("title", title)
                    put("track", trackArray)
                    put(
                        "extension",
                        JSONObject().apply {
                            put(
                                "https://musicbrainz.org/doc/jspf#playlist",
                                JSONObject().apply { put("public", true) },
                            )
                        },
                    )
                },
            )
        }
        val connection = URL("$PLAYLIST_BASE_URL/create").openConnection() as HttpURLConnection
        connection.requestMethod = "POST"
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        connection.setRequestProperty("Authorization", "Token $tok")
        connection.setRequestProperty("Content-Type", "application/json")
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        connection.doOutput = true
        connection.outputStream.use { it.write(body.toString().toByteArray(Charsets.UTF_8)) }
        val code = connection.responseCode
        if (code !in 200..299) {
            val errorBody = connection.errorStream?.bufferedReader()?.use { it.readText() }
            connection.disconnect()
            error("ListenBrainz returned $code: ${errorBody ?: "no details"}")
        }
        connection.disconnect()
    }

    /** The global fresh-releases feed — no server-side personalization
     *  param exists (confirmed against the OpenAPI spec), so "For You"
     *  filtering happens entirely client-side via [topArtistMbids].
     *  [spotifyAlbumId] is resolved lazily, only when a release is tapped —
     *  checking all of a window's releases eagerly would be far more
     *  MusicBrainz traffic than the generator sheets' bounded ~50 tracks. */
    fun freshReleases(days: Int, onResult: (Result<List<FreshRelease>>) -> Unit) {
        scope.launch {
            val result = runCatching { fetchFreshReleases(days) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchFreshReleases(days: Int): List<FreshRelease> {
        val url = "https://api.listenbrainz.org/1/explore/fresh-releases/?days=$days&sort=release_date&past=true&future=true"
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val releases = JSONObject(body).getJSONObject("payload").getJSONArray("releases")
        return (0 until releases.length()).mapNotNull { i ->
            val release = releases.getJSONObject(i)
            val name = release.optString("release_name").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            val mbid = release.optString("release_mbid").takeIf { it.isNotBlank() } ?: return@mapNotNull null
            val artistMbids = release.optJSONArray("artist_mbids")?.let { array ->
                (0 until array.length()).map { array.getString(it) }
            } ?: emptyList()
            FreshRelease(
                title = name,
                artist = release.optString("artist_credit_name"),
                releaseDate = release.optString("release_date"),
                releaseMbid = mbid,
                artistMbids = artistMbids,
                caaReleaseMbid = release.optString("caa_release_mbid").takeIf { it.isNotBlank() },
            )
        }
    }

    /** The signed-in user's own top-artist MBIDs (all-time, up to 200) —
     *  the basis for Fresh Releases' client-side "For You" filter. */
    fun topArtistMbids(onResult: (Result<Set<String>>) -> Unit) {
        val user = connectedUserName
        if (user == null) {
            onResult(Result.failure(IllegalStateException("Not connected to ListenBrainz.")))
            return
        }
        scope.launch {
            val result = runCatching { fetchTopArtistMbids(user) }
            withContext(Dispatchers.Main) { onResult(result) }
        }
    }

    private fun fetchTopArtistMbids(userName: String): Set<String> {
        val encodedUser = URLEncoder.encode(userName, "UTF-8")
        val url = "$STATS_BASE_URL/$encodedUser/artists?range=all_time&count=200"
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        token?.let { connection.setRequestProperty("Authorization", "Token $it") }
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        val body = connection.inputStream.bufferedReader().use { it.readText() }
        connection.disconnect()
        val artists = JSONObject(body).getJSONObject("payload").getJSONArray("artists")
        return (0 until artists.length()).mapNotNull { i ->
            artists.getJSONObject(i).optString("artist_mbid").takeIf { it.isNotBlank() }
        }.toSet()
    }

    /** Resolves one fresh release's Spotify album id lazily, only when
     *  tapped — same exact-or-null mechanism as every other MBID match-back
     *  in this file, via the release's own `url-rels`. */
    fun resolveFreshReleaseSpotifyId(releaseMbid: String, onResult: (String?) -> Unit) {
        scope.launch {
            val id = runCatching { spotifyAlbumIdForReleaseMbid(releaseMbid) }.getOrNull()
            withContext(Dispatchers.Main) { onResult(id) }
        }
    }

    private fun spotifyAlbumIdForReleaseMbid(releaseMbid: String): String? {
        val json = getJson("https://musicbrainz.org/ws/2/release/$releaseMbid?inc=url-rels&fmt=json") ?: return null
        val relations = json.optJSONArray("relations") ?: return null
        for (i in 0 until relations.length()) {
            val resource = relations.getJSONObject(i).optJSONObject("url")?.optString("resource") ?: continue
            if (resource.startsWith("https://open.spotify.com/album/")) {
                return resource.removePrefix("https://open.spotify.com/album/").trimEnd('/')
            }
        }
        return null
    }

    private fun getJson(urlStr: String): JSONObject? {
        repeat(2) { attempt ->
            val connection = URL(urlStr).openConnection() as HttpURLConnection
            connection.setRequestProperty("User-Agent", APP_USER_AGENT)
            connection.connectTimeout = 10_000
            connection.readTimeout = 10_000
            val code = connection.responseCode
            if (code == 200) {
                val body = connection.inputStream.bufferedReader().use { it.readText() }
                connection.disconnect()
                return JSONObject(body)
            }
            connection.disconnect()
            if (code != 503 || attempt == 1) return null
            Thread.sleep(600)
        }
        return null
    }

    /** Call on every effective-playback update (track, or just phase/
     *  position) — [SpotifyCore.publishEffectivePlayback] already fires on
     *  every transport change, local or remote, so this rides that same
     *  signal rather than polling anything of its own. */
    fun onPlaybackChanged(track: PlayingTrack?, phase: PlaybackPhase?) {
        val now = System.currentTimeMillis()
        if (wasPlaying) accumulatedPlayingMs += now - lastChangeAtMs
        if (track?.uri != trackedUri) {
            if (scrobbleEnabled && token != null) {
                trackedTrack?.let { previous ->
                    val threshold = if (previous.durationMs > 0) {
                        minOf(previous.durationMs / 2, MAX_THRESHOLD_MS)
                    } else {
                        Long.MAX_VALUE
                    }
                    if (accumulatedPlayingMs >= threshold) {
                        submit(previous, listenedAtMs = now - accumulatedPlayingMs)
                    }
                }
                if (track != null) submitPlayingNow(track)
            }
            trackedUri = track?.uri
            trackedTrack = track
            accumulatedPlayingMs = 0L
        }
        wasPlaying = phase == PlaybackPhase.Playing
        lastChangeAtMs = now
    }

    private fun submitPlayingNow(track: PlayingTrack) {
        val tok = token ?: return
        scope.launch {
            runCatching {
                post(
                    tok,
                    JSONObject().apply {
                        put("listen_type", "playing_now")
                        put("payload", JSONArray().put(trackMetadata(track)))
                    },
                )
            }
        }
    }

    private fun submit(track: PlayingTrack, listenedAtMs: Long) {
        val tok = token ?: return
        scope.launch {
            runCatching {
                post(
                    tok,
                    JSONObject().apply {
                        put("listen_type", "single")
                        put(
                            "payload",
                            JSONArray().put(
                                JSONObject().apply {
                                    put("listened_at", listenedAtMs / 1000)
                                    put("track_metadata", trackMetadata(track))
                                },
                            ),
                        )
                    },
                )
            }
        }
    }

    private fun trackMetadata(track: PlayingTrack) = JSONObject().apply {
        put("artist_name", track.artist)
        put("track_name", track.title)
        if (track.album.isNotBlank()) put("release_name", track.album)
        put(
            "additional_info",
            JSONObject().apply {
                put("duration_ms", track.durationMs)
                put("media_player", "Spotifast")
            },
        )
    }

    private fun post(token: String, body: JSONObject) {
        val connection = URL(SUBMIT_URL).openConnection() as HttpURLConnection
        connection.requestMethod = "POST"
        connection.setRequestProperty("User-Agent", APP_USER_AGENT)
        connection.setRequestProperty("Authorization", "Token $token")
        connection.setRequestProperty("Content-Type", "application/json")
        connection.connectTimeout = 10_000
        connection.readTimeout = 10_000
        connection.doOutput = true
        connection.outputStream.use { it.write(body.toString().toByteArray(Charsets.UTF_8)) }
        connection.responseCode
        connection.disconnect()
    }
}
