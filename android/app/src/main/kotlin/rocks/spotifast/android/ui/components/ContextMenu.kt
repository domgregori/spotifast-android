package rocks.spotifast.android.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.PlaylistAdd
import androidx.compose.material.icons.automirrored.filled.QueueMusic
import androidx.compose.material.icons.filled.Album
import androidx.compose.material.icons.filled.AutoAwesome
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PersonRemove
import androidx.compose.material.icons.filled.People
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.filled.Radio
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.LibraryItem
import rocks.spotifast.android.RecentEntry
import rocks.spotifast.android.TrackItem

/** One tappable row in a context-menu bottom sheet: an icon, a label, and
 *  a click that performs the action and closes the sheet. */
@Composable
private fun MenuRow(label: String, onClick: () -> Unit, icon: @Composable () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        icon()
        Spacer(Modifier.width(20.dp))
        Text(text = label, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurface)
    }
}

/** The long-press menu for a track: like/unlike, add to queue, song radio,
 *  and share. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TrackContextMenu(
    track: TrackItem,
    liked: Boolean,
    onDismiss: () -> Unit,
    onToggleLiked: () -> Unit,
    onAddToQueue: () -> Unit,
    onGoToRadio: () -> Unit,
    onShare: () -> Unit,
    onViewArtist: (() -> Unit)? = null,
    onViewAlbum: (() -> Unit)? = null,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = track.name,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            MenuRow(
                label = if (liked) "Remove from Liked Songs" else "Save to Liked Songs",
                onClick = { onToggleLiked(); onDismiss() },
            ) {
                Icon(
                    imageVector = if (liked) Icons.Filled.Favorite else Icons.Outlined.FavoriteBorder,
                    contentDescription = null,
                    tint = if (liked) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Add to Queue", onClick = { onAddToQueue(); onDismiss() }) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.QueueMusic,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Go to song radio", onClick = { onGoToRadio(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Radio,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Share", onClick = { onShare(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Share,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (onViewArtist != null) {
                MenuRow(label = "View Artist", onClick = { onViewArtist(); onDismiss() }) {
                    Icon(
                        imageVector = Icons.Filled.Person,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (onViewAlbum != null) {
                MenuRow(label = "View Album", onClick = { onViewAlbum(); onDismiss() }) {
                    Icon(
                        imageVector = Icons.Filled.Album,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/** The long-press menu for a library item (a playlist or Liked Songs):
 *  pin/unpin to the top of Home and Library, share, and delete. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LibraryItemContextMenu(
    item: LibraryItem,
    pinned: Boolean,
    onDismiss: () -> Unit,
    onTogglePinned: () -> Unit,
    onShare: (() -> Unit)? = null,
    onDelete: (() -> Unit)? = null,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = item.name,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            MenuRow(
                label = if (pinned) "Unpin" else "Pin to Home",
                onClick = { onTogglePinned(); onDismiss() },
            ) {
                Icon(
                    imageVector = Icons.Filled.PushPin,
                    contentDescription = null,
                    tint = if (pinned) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (onShare != null) {
                MenuRow(label = "Share", onClick = { onShare(); onDismiss() }) {
                    Icon(
                        imageVector = Icons.Filled.Share,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (onDelete != null) {
                MenuRow(label = "Delete", onClick = { onDelete(); onDismiss() }) {
                    Icon(
                        imageVector = Icons.Filled.Delete,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

/** The long-press menu for an album: create a playlist from its tracks,
 *  and share. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AlbumContextMenu(
    name: String,
    onDismiss: () -> Unit,
    onCreatePlaylist: () -> Unit,
    onShare: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = name,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            MenuRow(label = "Create playlist", onClick = { onCreatePlaylist(); onDismiss() }) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.PlaylistAdd,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Share", onClick = { onShare(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Share,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** The long-press menu for a Home card (a playlist, artist, or album that
 *  was actually played from): pin/unpin, remove, play, and add to queue. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RecentEntryContextMenu(
    entry: RecentEntry,
    pinned: Boolean,
    onDismiss: () -> Unit,
    onTogglePinned: () -> Unit,
    onRemove: () -> Unit,
    onPlay: () -> Unit,
    onAddToQueue: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = entry.name,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            MenuRow(label = "Play", onClick = { onPlay(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.PlayArrow,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Add to Queue", onClick = { onAddToQueue(); onDismiss() }) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.QueueMusic,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(
                label = if (pinned) "Unpin" else "Pin to Home",
                onClick = { onTogglePinned(); onDismiss() },
            ) {
                Icon(
                    imageVector = Icons.Filled.PushPin,
                    contentDescription = null,
                    tint = if (pinned) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Remove", onClick = { onRemove(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Delete,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** The long-press menu for an artist's picture: info, follow/unfollow,
 *  artist radio, similar artists and LB Radio generation (only once
 *  ListenBrainz is connected — Spotify's own related-artists/
 *  recommendations endpoints 404 for this app), and share. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ArtistArtContextMenu(
    name: String,
    following: Boolean,
    listenBrainzConnected: Boolean,
    onDismiss: () -> Unit,
    onViewInfo: () -> Unit,
    onToggleFollow: () -> Unit,
    onGoToRadio: () -> Unit,
    onSimilarArtists: () -> Unit,
    onLbRadio: () -> Unit,
    onShare: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = name,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            MenuRow(label = "Artist info", onClick = { onViewInfo(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Info,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(
                label = if (following) "Unfollow" else "Follow",
                onClick = { onToggleFollow(); onDismiss() },
            ) {
                Icon(
                    imageVector = if (following) Icons.Filled.PersonRemove else Icons.Filled.Person,
                    contentDescription = null,
                    tint = if (following) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            MenuRow(label = "Go to artist radio", onClick = { onGoToRadio(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Radio,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (listenBrainzConnected) {
                MenuRow(label = "Similar artists", onClick = { onSimilarArtists(); onDismiss() }) {
                    Icon(
                        imageVector = Icons.Filled.People,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                MenuRow(label = "Generate ListenBrainz playlist", onClick = { onLbRadio(); onDismiss() }) {
                    Icon(
                        imageVector = Icons.Filled.AutoAwesome,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            MenuRow(label = "Share", onClick = { onShare(); onDismiss() }) {
                Icon(
                    imageVector = Icons.Filled.Share,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
