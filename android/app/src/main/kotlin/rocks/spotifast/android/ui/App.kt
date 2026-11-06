package rocks.spotifast.android.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.AlbumItem
import rocks.spotifast.android.AuthState
import rocks.spotifast.android.CurrentSource
import rocks.spotifast.android.LibraryItem
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.PlaybackPhase
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.TrackItem
import rocks.spotifast.android.ui.components.AlbumContextMenu
import rocks.spotifast.android.ui.components.DevicesSheet
import rocks.spotifast.android.ui.components.LibraryItemContextMenu
import rocks.spotifast.android.ui.components.MiniPlayerBar
import rocks.spotifast.android.ui.components.PlaylistNameDialog
import rocks.spotifast.android.ui.components.RecentEntryContextMenu
import rocks.spotifast.android.ui.components.SpotifastBottomBar
import rocks.spotifast.android.ui.components.ToastBanner
import rocks.spotifast.android.ui.components.TrackContextMenu
import rocks.spotifast.android.util.shareUri

/** Spotify's three bottom-nav destinations, plus ListenBrainz once connected. */
enum class AppTab { Home, Search, Library, ListenBrainz }

/**
 * The app's root composable: routes on [AuthState], then — once connected —
 * hosts the tabbed Home/Search/Library experience plus the playlist detail
 * screen and the persistent mini-player, replacing the old ad-hoc
 * boolean/nullable screen switching in `MainActivity`.
 */
@Composable
fun SpotifastApp() {
    val authState by SpotifyCore.authState.collectAsState()
    when (val state = authState) {
        is AuthState.WaitingForBrowser -> BrowserRedirectScreen(state.url)
        is AuthState.Connecting -> ConnectingScreen()
        is AuthState.Connected -> ConnectedApp(username = state.username)
        is AuthState.Failed -> AuthFailedScreen(state.message)
        AuthState.SignedOut -> LoginScreen()
    }
}

@Composable
private fun ConnectedApp(username: String) {
    var selectedTab by remember { mutableStateOf(AppTab.Home) }
    var nowPlayingOpen by remember { mutableStateOf(false) }
    var queueOpen by remember { mutableStateOf(false) }
    var statsOpen by remember { mutableStateOf(false) }
    var settingsOpen by remember { mutableStateOf(false) }
    var devicesOpen by remember { mutableStateOf(false) }
    var newReleasesOpen by remember { mutableStateOf(false) }
    var createPlaylistAlbum by remember { mutableStateOf<AlbumItem?>(null) }
    var deletePlaylistItem by remember { mutableStateOf<LibraryItem?>(null) }
    val context = LocalContext.current
    val selectedPlaylist by SpotifyCore.selectedPlaylist.collectAsState()
    val selectedArtist by SpotifyCore.selectedArtist.collectAsState()
    val selectedAlbum by SpotifyCore.selectedAlbum.collectAsState()
    val playbackStatus by SpotifyCore.playbackStatus.collectAsState()
    val localPlayback by SpotifyCore.localPlayback.collectAsState()
    val trackMenu by SpotifyCore.trackMenu.collectAsState()
    val libraryMenu by SpotifyCore.libraryMenu.collectAsState()
    val recentMenu by SpotifyCore.recentMenu.collectAsState()
    val albumMenu by SpotifyCore.albumMenu.collectAsState()
    val likedTracks by SpotifyCore.likedTracks.collectAsState()
    val pinnedIds by SpotifyCore.pinnedIds.collectAsState()
    val pinnedRecentKeys by SpotifyCore.pinnedRecentKeys.collectAsState()
    val activeRadio by SpotifyCore.activeRadio.collectAsState()
    val currentSource by SpotifyCore.currentSource.collectAsState()
    val toast by SpotifyCore.toast.collectAsState()

    LaunchedEffect(Unit) {
        SpotifyCore.library()
        SpotifyCore.loadLibraryTree()
    }

    fun selectTab(tab: AppTab) {
        if (selectedTab == AppTab.Search && tab != AppTab.Search) {
            SpotifyCore.closeSearch()
        }
        if (tab == AppTab.Search) {
            SpotifyCore.openSearch()
        }
        selectedTab = tab
        if (selectedPlaylist != null) {
            SpotifyCore.closePlaylist()
        }
        if (selectedArtist != null) {
            SpotifyCore.closeArtist()
        }
        if (selectedAlbum != null) {
            SpotifyCore.closeAlbum()
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        Scaffold(
            containerColor = MaterialTheme.colorScheme.background,
            bottomBar = {
                Column {
                    MiniPlayerBar(
                        status = playbackStatus.first,
                        detail = playbackStatus.second,
                        track = localPlayback?.track,
                        phase = localPlayback?.phase,
                        positionMs = localPlayback?.positionMs ?: 0,
                        onClick = {
                            // A track can be showing from another Connect
                            // device even when this phone's own engine was
                            // never authorized — prefer opening the player
                            // over nagging for local authorization it
                            // doesn't actually need for that.
                            if (localPlayback?.track != null) {
                                nowPlayingOpen = true
                            } else if (playbackStatus.first == "Ready") {
                                // Nothing playing yet, but this phone's own
                                // engine is reachable — offer to switch to
                                // a different Connect device instead of
                                // doing nothing.
                                devicesOpen = true
                            } else {
                                SpotifyCore.authorizePlayback()
                            }
                        },
                        onToggle = { SpotifyCore.togglePlayback() },
                    )
                    SpotifastBottomBar(
                        selected = selectedTab,
                        onSelect = ::selectTab,
                        listenBrainzConnected = ListenBrainz.isConnected,
                    )
                }
            },
        ) { padding ->
            Box(
                modifier = Modifier
                    .padding(padding)
                    .fillMaxSize(),
            ) {
                val playlistId = selectedPlaylist
                val artistId = selectedArtist
                val albumId = selectedAlbum
                // Each branch below is a distinct screen that fully replaces
                // the others in composition — without this holder, leaving
                // one (switching tabs, opening a playlist) would discard its
                // rememberSaveable state, including each list's scroll
                // position, and coming back would always restart at the top.
                val screenState = rememberSaveableStateHolder()
                when {
                    playlistId != null -> screenState.SaveableStateProvider("playlist:$playlistId") {
                        PlaylistScreen(playlistId = playlistId, onBack = { SpotifyCore.closePlaylist() })
                    }
                    artistId != null -> screenState.SaveableStateProvider("artist:$artistId") {
                        ArtistScreen(
                            artistId = artistId,
                            onBack = { SpotifyCore.closeArtist() },
                            onOpenAlbum = SpotifyCore::openAlbum,
                        )
                    }
                    albumId != null -> screenState.SaveableStateProvider("album:$albumId") {
                        AlbumScreen(albumId = albumId, onBack = { SpotifyCore.closeAlbum() })
                    }
                    else -> screenState.SaveableStateProvider("tab:$selectedTab") {
                        when (selectedTab) {
                            AppTab.Home -> HomeScreen(
                                username = username,
                                onOpenPlaylist = SpotifyCore::openPlaylist,
                                onOpenArtist = SpotifyCore::openArtist,
                                onOpenAlbum = SpotifyCore::openAlbum,
                                onOpenSettings = { settingsOpen = true },
                            )
                            AppTab.Search -> SearchScreen(
                                onOpenPlaylist = SpotifyCore::openPlaylist,
                                onOpenArtist = SpotifyCore::openArtist,
                                onOpenAlbum = SpotifyCore::openAlbum,
                            )
                            AppTab.Library -> LibraryScreen(
                                onOpenPlaylist = SpotifyCore::openPlaylist,
                                onOpenArtist = SpotifyCore::openArtist,
                                onOpenAlbum = SpotifyCore::openAlbum,
                                onOpenStats = { statsOpen = true },
                                onOpenNewReleases = { newReleasesOpen = true },
                            )
                            AppTab.ListenBrainz -> ListenBrainzScreen()
                        }
                    }
                }
            }
        }

        val track = localPlayback?.track
        if (nowPlayingOpen && track != null) {
            NowPlayingScreen(
                track = track,
                phase = localPlayback?.phase ?: PlaybackPhase.Stopped,
                positionMs = localPlayback?.positionMs ?: 0,
                shuffle = localPlayback?.shuffle ?: false,
                repeat = localPlayback?.repeat ?: "Off",
                activeRadio = activeRadio,
                currentSource = currentSource,
                onClose = { nowPlayingOpen = false },
                onOpenQueue = { queueOpen = true },
                onGoToSource = {
                    nowPlayingOpen = false
                    when (val source = currentSource) {
                        is CurrentSource.Playlist -> SpotifyCore.openPlaylist(source.id)
                        is CurrentSource.Artist -> SpotifyCore.openArtist(source.id)
                        is CurrentSource.Album -> SpotifyCore.openAlbum(source.id)
                        null -> {}
                    }
                },
                onLongPressCover = {
                    SpotifyCore.openTrackMenu(
                        TrackItem(
                            uri = track.uri,
                            name = track.title,
                            artist = track.artist,
                            durationMs = track.durationMs,
                            artUrl = track.artUrl,
                            artistId = track.artistId,
                            albumId = track.albumId,
                        ),
                    )
                },
            )
        }
        if (queueOpen) {
            QueueScreen(onClose = { queueOpen = false })
        }
        if (statsOpen) {
            StatsScreen(onClose = { statsOpen = false })
        }
        if (newReleasesOpen) {
            NewReleasesScreen(
                onBack = { newReleasesOpen = false },
                onOpenAlbum = {
                    newReleasesOpen = false
                    SpotifyCore.openAlbum(it)
                },
            )
        }
        if (settingsOpen) {
            SettingsScreen(onClose = { settingsOpen = false })
        }
        if (devicesOpen) {
            DevicesSheet(onDismiss = { devicesOpen = false })
        }
        GeneratedPlaylistScreen()

        val menuTrack = trackMenu
        if (menuTrack != null) {
            TrackContextMenu(
                track = menuTrack,
                liked = likedTracks[menuTrack.uri] ?: false,
                onDismiss = { SpotifyCore.closeTrackMenu() },
                onToggleLiked = { SpotifyCore.toggleLiked(menuTrack.uri) },
                onAddToQueue = { SpotifyCore.addToQueue(menuTrack.uri, menuTrack.name) },
                onGoToRadio = { SpotifyCore.goToRadio(menuTrack.uri, "${menuTrack.name} Radio") },
                onShare = { shareUri(context, menuTrack.uri, menuTrack.name) },
                onViewArtist = menuTrack.artistId?.let { id ->
                    {
                        nowPlayingOpen = false
                        SpotifyCore.openArtist(id)
                    }
                },
                onViewAlbum = menuTrack.albumId?.let { id ->
                    {
                        nowPlayingOpen = false
                        SpotifyCore.openAlbum(id)
                    }
                },
            )
        }
        val menuItem = libraryMenu
        if (menuItem != null) {
            LibraryItemContextMenu(
                item = menuItem,
                pinned = pinnedIds.contains(menuItem.id),
                onDismiss = { SpotifyCore.closeLibraryMenu() },
                onTogglePinned = { SpotifyCore.togglePinned(menuItem.id) },
                onShare = if (menuItem.id == SpotifyCore.LIKED_SONGS_ID) {
                    null
                } else {
                    { shareUri(context, "spotify:playlist:${menuItem.id}", menuItem.name) }
                },
                onDelete = if (menuItem.id == SpotifyCore.LIKED_SONGS_ID) {
                    null
                } else {
                    { deletePlaylistItem = menuItem }
                },
            )
        }
        val deleteItem = deletePlaylistItem
        if (deleteItem != null) {
            AlertDialog(
                onDismissRequest = { deletePlaylistItem = null },
                title = { Text("Delete \"${deleteItem.name}\"?") },
                text = { Text("This removes it from your library.") },
                confirmButton = {
                    TextButton(onClick = {
                        SpotifyCore.setLiked("spotify:playlist:${deleteItem.id}", false)
                        deletePlaylistItem = null
                    }) {
                        Text("Delete")
                    }
                },
                dismissButton = {
                    TextButton(onClick = { deletePlaylistItem = null }) {
                        Text("Cancel")
                    }
                },
            )
        }
        val menuAlbum = albumMenu
        if (menuAlbum != null) {
            AlbumContextMenu(
                name = menuAlbum.name,
                onDismiss = { SpotifyCore.closeAlbumMenu() },
                onCreatePlaylist = { createPlaylistAlbum = menuAlbum },
                onShare = { shareUri(context, "spotify:album:${menuAlbum.id}", menuAlbum.name) },
            )
        }
        val playlistNameAlbum = createPlaylistAlbum
        if (playlistNameAlbum != null) {
            PlaylistNameDialog(
                initialName = playlistNameAlbum.name,
                onConfirm = { name ->
                    SpotifyCore.createPlaylistFromAlbum(playlistNameAlbum.id, name)
                    createPlaylistAlbum = null
                },
                onDismiss = { createPlaylistAlbum = null },
            )
        }
        val menuEntry = recentMenu
        if (menuEntry != null) {
            RecentEntryContextMenu(
                entry = menuEntry,
                pinned = pinnedRecentKeys.contains("${menuEntry.kind.name}:${menuEntry.id}"),
                onDismiss = { SpotifyCore.closeRecentMenu() },
                onTogglePinned = { SpotifyCore.togglePinnedRecent(menuEntry) },
                onRemove = { SpotifyCore.removeRecentEntry(menuEntry) },
                onPlay = { SpotifyCore.playRecentEntry(menuEntry) },
                onAddToQueue = { SpotifyCore.queueRecentEntry(menuEntry) },
            )
        }

        val toastMessage = toast
        if (toastMessage != null) {
            ToastBanner(
                message = toastMessage,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 120.dp),
                onDismiss = { SpotifyCore.clearToast() },
            )
        }
    }
}
