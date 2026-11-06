package rocks.spotifast.android.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.LibraryAdd
import androidx.compose.material.icons.filled.LibraryAddCheck
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.LibraryItem
import rocks.spotifast.android.PlayContext
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.ArtworkPlaceholder
import rocks.spotifast.android.ui.components.FilterBar
import rocks.spotifast.android.ui.components.TrackRow
import rocks.spotifast.android.ui.components.rememberPullToRevealConnection

/**
 * Spotify's playlist detail screen: a large header, then the track list.
 * Pulling down from the top (the header fully visible) reveals a track
 * filter, the same gesture [LibraryScreen] uses for playlists.
 */
@Composable
fun PlaylistScreen(playlistId: String, onBack: () -> Unit) {
    val tracks by SpotifyCore.tracks.collectAsState()
    val library by SpotifyCore.library.collectAsState()
    val likedTracks by SpotifyCore.likedTracks.collectAsState()
    val tracksHasMore by SpotifyCore.tracksHasMore.collectAsState()
    val tracksLoadingMore by SpotifyCore.tracksLoadingMore.collectAsState()
    val playlistTracks = tracks[playlistId] ?: emptyList()
    val hasMore = tracksHasMore[playlistId] == true
    val loadingMore = tracksLoadingMore.contains(playlistId)
    val playlist = if (playlistId == SpotifyCore.LIKED_SONGS_ID) {
        SpotifyCore.LIKED_SONGS_ITEM
    } else {
        library.firstOrNull { it.id == playlistId }
    }
    val isLiked = playlistId == SpotifyCore.LIKED_SONGS_ID
    val inLibrary = isLiked || likedTracks["spotify:playlist:$playlistId"] == true

    LaunchedEffect(playlistTracks) { SpotifyCore.checkLikedBatch(playlistTracks.map { it.uri }) }
    LaunchedEffect(playlistId) {
        if (!isLiked) SpotifyCore.checkLiked("spotify:playlist:$playlistId")
    }

    var filterQuery by remember { mutableStateOf("") }
    var filterRevealed by remember { mutableStateOf(false) }
    val listState = rememberLazyListState()
    val pullConnection = rememberPullToRevealConnection(
        listState = listState,
        revealed = filterRevealed,
        onReveal = { filterRevealed = true },
    )

    val filteredTracks = remember(playlistTracks, filterQuery) {
        if (filterQuery.isBlank()) {
            playlistTracks
        } else {
            playlistTracks.filter {
                it.name.contains(filterQuery, ignoreCase = true) || it.artist.contains(filterQuery, ignoreCase = true)
            }
        }
    }

    // Scrolling near the end of what's loaded quietly fetches the next
    // page — re-launched whenever the list or load state actually changes,
    // so each run's closure sees the current size rather than the one from
    // whenever this composable first ran.
    LaunchedEffect(playlistId, filteredTracks, hasMore, loadingMore) {
        if (!hasMore || loadingMore) return@LaunchedEffect
        snapshotFlow { listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0 }
            .collect { lastVisible ->
                if (lastVisible >= filteredTracks.size - 5) {
                    SpotifyCore.loadMoreTracks(playlistId)
                }
            }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        if (filterRevealed) {
            FilterBar(
                query = filterQuery,
                onQueryChange = { filterQuery = it },
                onClose = {
                    filterRevealed = false
                    filterQuery = ""
                },
                placeholder = "Filter this playlist",
            )
        }
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .nestedScroll(pullConnection),
            state = listState,
        ) {
            item {
                PlaylistHeader(
                    playlistId = playlistId,
                    name = playlist?.name ?: "Playlist",
                    artUrl = playlist?.artUrl,
                    trackCount = playlistTracks.size,
                    inLibrary = inLibrary,
                    isLikedSongs = isLiked,
                    onToggleSaved = { SpotifyCore.setLiked("spotify:playlist:$playlistId", !inLibrary) },
                    onBack = onBack,
                    onOpenMenu = {
                        SpotifyCore.openLibraryMenu(
                            playlist ?: LibraryItem(playlistId, "Playlist", null),
                        )
                    },
                )
            }
            if (playlistTracks.isEmpty()) {
                item {
                    Text(
                        text = "No tracks in this playlist.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(32.dp),
                    )
                }
            } else if (filteredTracks.isEmpty()) {
                item {
                    Text(
                        text = "No tracks match \"$filterQuery\".",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(32.dp),
                    )
                }
            } else {
                itemsIndexed(filteredTracks, key = { _, track -> track.uri }) { index, track ->
                    // The queue on tap is the filtered list — playing from a
                    // filtered view queues what's shown, not the whole playlist.
                    TrackRow(
                        track = track,
                        onClick = {
                            SpotifyCore.playTracks(
                                filteredTracks,
                                index,
                                PlayContext.Playlist(playlistId, playlist?.name ?: "Playlist", playlist?.artUrl),
                            )
                        },
                        onLongClick = { SpotifyCore.openTrackMenu(track) },
                        showLiked = playlistId != SpotifyCore.LIKED_SONGS_ID,
                    )
                }
            }
            if (hasMore) {
                item {
                    LoadAllRow(loading = loadingMore, onClick = { SpotifyCore.loadAllTracks(playlistId) })
                }
            }
        }
    }
}

@Composable
private fun LoadAllRow(loading: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = !loading, onClick = onClick)
            .padding(horizontal = 24.dp, vertical = 16.dp),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (loading) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(12.dp))
            Text(
                text = "Loading more…",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            Text(
                text = "Load all",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.primary,
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PlaylistHeader(
    playlistId: String,
    name: String,
    artUrl: String?,
    trackCount: Int,
    inLibrary: Boolean,
    isLikedSongs: Boolean,
    onToggleSaved: () -> Unit,
    onBack: () -> Unit,
    onOpenMenu: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceContainer),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(4.dp)) {
            IconButton(onClick = onBack) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 24.dp, vertical = 8.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            Column {
                ArtworkPlaceholder(
                    key = playlistId,
                    label = name,
                    modifier = Modifier.size(160.dp).combinedClickable(onClick = {}, onLongClick = onOpenMenu),
                    corner = 8.dp,
                    artUrl = artUrl,
                )
                Spacer(Modifier.height(16.dp))
                Text(
                    text = name,
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.ExtraBold,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                Spacer(Modifier.height(4.dp))
                Text(
                    text = if (trackCount == 1) "1 song" else "$trackCount songs",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(8.dp))
                if (isLikedSongs) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            imageVector = Icons.Filled.LibraryAddCheck,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(
                            text = "In Your Library",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                } else {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.clickable(onClick = onToggleSaved),
                    ) {
                        Icon(
                            imageVector = if (inLibrary) Icons.Filled.LibraryAddCheck else Icons.Filled.LibraryAdd,
                            contentDescription = null,
                            tint = if (inLibrary) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(
                            text = if (inLibrary) "In Your Library" else "Save to library",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (inLibrary) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
                }
                Spacer(Modifier.height(12.dp))
            }
        }
    }
}
