package rocks.spotifast.android.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.LibraryAdd
import androidx.compose.material.icons.filled.LibraryAddCheck
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.AlbumItem
import rocks.spotifast.android.PlayContext
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.ArtworkPlaceholder
import rocks.spotifast.android.ui.components.TrackRow

/** An album's detail screen: cover, name, artist, then its tracks in order. */
@Composable
fun AlbumScreen(albumId: String, onBack: () -> Unit) {
    val albums by SpotifyCore.albums.collectAsState()
    val albumTracks by SpotifyCore.albumTracks.collectAsState()
    val likedTracks by SpotifyCore.likedTracks.collectAsState()
    val album = albums[albumId]
    val tracks = albumTracks[albumId] ?: emptyList()
    val albumUri = "spotify:album:$albumId"
    val saved = likedTracks[albumUri] == true

    LaunchedEffect(tracks) { SpotifyCore.checkLikedBatch(tracks.map { it.uri }) }
    LaunchedEffect(albumId) { SpotifyCore.checkLiked(albumUri) }

    LazyColumn(modifier = Modifier.fillMaxSize()) {
        item {
            AlbumHeader(
                albumId = albumId,
                name = album?.name ?: "Album",
                artist = album?.artist ?: "",
                artUrl = album?.artUrl,
                trackCount = tracks.size,
                saved = saved,
                onToggleSaved = { SpotifyCore.setLiked(albumUri, !saved) },
                onBack = onBack,
                onOpenMenu = {
                    SpotifyCore.openAlbumMenu(album ?: AlbumItem(albumId, "Album", "", null))
                },
            )
        }
        itemsIndexed(tracks, key = { _, track -> track.uri }) { index, track ->
            TrackRow(
                track = track,
                onClick = {
                    SpotifyCore.playTracks(
                        tracks,
                        index,
                        PlayContext.Album(albumId, album?.name ?: "Album", album?.artist ?: "", album?.artUrl),
                    )
                },
                onLongClick = { SpotifyCore.openTrackMenu(track) },
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun AlbumHeader(
    albumId: String,
    name: String,
    artist: String,
    artUrl: String?,
    trackCount: Int,
    saved: Boolean,
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
                    key = albumId,
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
                    text = if (artist.isBlank()) songCountLabel(trackCount) else "$artist · ${songCountLabel(trackCount)}",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(8.dp))
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.clickable(onClick = onToggleSaved),
                ) {
                    Icon(
                        imageVector = if (saved) Icons.Filled.LibraryAddCheck else Icons.Filled.LibraryAdd,
                        contentDescription = null,
                        tint = if (saved) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(16.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = if (saved) "Saved" else "Save",
                        style = MaterialTheme.typography.bodySmall,
                        color = if (saved) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Spacer(Modifier.height(12.dp))
            }
        }
    }
}

private fun songCountLabel(count: Int) = if (count == 1) "1 song" else "$count songs"
