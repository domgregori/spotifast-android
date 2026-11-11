package rocks.spotifast.android.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.TrackItem

/** The "Add to playlist" sheet a track's context menu opens: every playlist
 *  in the account, tapping one adds the track to it via
 *  [SpotifyCore.addTrackToPlaylist]. Liked Songs isn't listed here — that's
 *  the heart on the track row/context menu, a different mechanism
 *  ([SpotifyCore.setLiked]), not a playlist add. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddToPlaylistSheet(track: TrackItem, onDismiss: () -> Unit) {
    val library by SpotifyCore.library.collectAsState()

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Text(
            text = "Add to playlist",
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        if (library.isEmpty()) {
            Text(
                text = "No playlists yet.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp),
            )
        }
        LazyColumn(modifier = Modifier.navigationBarsPadding()) {
            items(library, key = { it.id }) { playlist ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable {
                            SpotifyCore.addTrackToPlaylist(playlist.id, playlist.name, track.uri)
                            onDismiss()
                        }
                        .padding(horizontal = 24.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    ArtworkPlaceholder(key = playlist.id, label = playlist.name, modifier = Modifier.size(44.dp), artUrl = playlist.artUrl)
                    Spacer(Modifier.width(16.dp))
                    Text(
                        text = playlist.name,
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurface,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}
