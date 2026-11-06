package rocks.spotifast.android.ui.components

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.FolderOpen
import androidx.compose.material.icons.filled.GraphicEq
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.random.Random
import rocks.spotifast.android.AlbumItem
import rocks.spotifast.android.ArtistItem
import rocks.spotifast.android.LibraryItem
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.TrackItem
import rocks.spotifast.android.ui.LocalIsNoir
import java.util.concurrent.TimeUnit

/** A single row in the "Your Library" list: artwork tile, name, subtitle.
 *  Long-press opens the pin/unpin context menu. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun PlaylistRow(
    item: LibraryItem,
    onClick: () -> Unit,
    pinned: Boolean = false,
    onLongClick: () -> Unit = {},
    subtitle: String = "Playlist",
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .combinedClickable(onClick = onClick, onLongClick = onLongClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ArtworkPlaceholder(key = item.id, label = item.name, modifier = Modifier.size(56.dp), artUrl = item.artUrl)
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = item.name,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.size(2.dp))
            Text(
                text = subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
        if (pinned) {
            Spacer(Modifier.width(8.dp))
            Icon(
                imageVector = Icons.Filled.PushPin,
                contentDescription = "Pinned",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(16.dp),
            )
        }
    }
}

/** A folder row in "Your Library": a folder glyph, its name and playlist
 *  count, and a chevron — tap anywhere to expand or collapse it. */
@Composable
fun FolderRow(name: String, count: Int, expanded: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            imageVector = if (expanded) Icons.Filled.FolderOpen else Icons.Filled.Folder,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(40.dp),
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = name,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.size(2.dp))
            Text(
                text = if (count == 1) "1 playlist" else "$count playlists",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
        Icon(
            imageVector = if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
            contentDescription = if (expanded) "Collapse" else "Expand",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** A square artwork tile with a caption underneath, for Home's
 *  recently-played grid — playlist, artist, and album cards all share this
 *  one card language. [tag] is the small type label under the title
 *  ("PLAYLIST"/"ARTIST"/"ALBUM") — [subtitle], when non-blank, replaces it
 *  (an album's artist name is more useful there than a static tag). */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun RecentCardTile(
    id: String,
    title: String,
    tag: String,
    subtitle: String = "",
    artUrl: String? = null,
    onClick: () -> Unit,
    onLongClick: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val noir = LocalIsNoir.current
    // Stable per item (not re-rolled every recomposition), tiny — a
    // scrapbook/corkboard feel, not a cartoon tilt. Only in Noir mode;
    // Spotify's own look stays grid-straight.
    val rotation = remember(id, noir) {
        if (noir) Random(id.hashCode()).nextFloat() * 4f - 2f else 0f
    }
    val caption = subtitle.ifBlank { tag }
    Column(
        modifier = modifier
            .rotate(rotation)
            .clip(RoundedCornerShape(8.dp))
            .combinedClickable(onClick = onClick, onLongClick = onLongClick)
            .padding(8.dp),
    ) {
        if (noir) {
            // One connected card: a single border wraps art and label
            // together, with no gap at the seam between them — the art's
            // own corner/border is suppressed so the two don't double up,
            // and its top corners read as rounded only because the outer
            // clip rounds them where they meet this border.
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(8.dp))
                    .border(2.5.dp, MaterialTheme.colorScheme.onSurface, RoundedCornerShape(8.dp)),
            ) {
                ArtworkPlaceholder(
                    key = id,
                    label = title,
                    modifier = Modifier
                        .fillMaxWidth()
                        .aspectRatio(1f),
                    corner = 0.dp,
                    artUrl = artUrl,
                    drawBorder = false,
                )
                // The seam itself, in the same ink as the card's outer
                // border — without it, the art and label just touch with
                // no line between them.
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(2.5.dp)
                        .background(MaterialTheme.colorScheme.onSurface),
                )
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(MaterialTheme.colorScheme.surfaceContainerLowest)
                        .padding(horizontal = 10.dp, vertical = 8.dp),
                ) {
                    Text(
                        text = title.uppercase(),
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurface,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Spacer(Modifier.size(2.dp))
                    Text(
                        text = caption.uppercase(),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        letterSpacing = 1.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        } else {
            ArtworkPlaceholder(
                key = id,
                label = title,
                modifier = Modifier
                    .fillMaxWidth()
                    .aspectRatio(1f),
                corner = 8.dp,
                artUrl = artUrl,
            )
            Spacer(Modifier.size(8.dp))
            Text(
                text = title,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = caption,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** A single track row: small artwork, name/artist stack, duration.
 *  Long-press opens the like/add-to-queue context menu. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun TrackRow(
    track: TrackItem,
    onClick: (() -> Unit)? = null,
    onLongClick: () -> Unit = {},
    showLiked: Boolean = true,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .combinedClickable(onClick = onClick ?: {}, onLongClick = onLongClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ArtworkPlaceholder(key = track.uri, label = track.name, modifier = Modifier.size(44.dp), artUrl = track.artUrl)
        Spacer(Modifier.width(12.dp))
        // Whichever device is actually playing, not just this phone's own
        // engine — matches [SpotifyCore.currentSource]'s "go to source"
        // button, both reading the same effective-playback reconciliation.
        val nowPlaying by SpotifyCore.localPlayback.collectAsState()
        val isPlaying = nowPlaying?.track?.uri == track.uri
        Column(modifier = Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (isPlaying) {
                    Icon(
                        imageVector = Icons.Filled.GraphicEq,
                        contentDescription = "Playing",
                        tint = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.size(14.dp),
                    )
                    Spacer(Modifier.width(4.dp))
                }
                Text(
                    text = track.name,
                    style = MaterialTheme.typography.bodyLarge,
                    color = if (isPlaying) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(Modifier.size(2.dp))
            Text(
                text = track.artist,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.width(8.dp))
        // Only ever drawn when already known — this row never fetches it
        // itself; the containing screen batch-checks the whole visible
        // list up front (see SpotifyCore.checkLikedBatch) when the API
        // response it loaded from doesn't already carry it inline.
        // Suppressed entirely on a screen where every row is already known
        // to be liked (Liked Songs itself, or Liked Songs filtered by
        // artist) — the heart would just repeat what's already implied.
        val likedTracks by SpotifyCore.likedTracks.collectAsState()
        if (showLiked && likedTracks[track.uri] == true) {
            Icon(
                imageVector = Icons.Filled.Favorite,
                contentDescription = "Liked",
                tint = MaterialTheme.colorScheme.primary,
                modifier = Modifier.size(16.dp),
            )
            Spacer(Modifier.width(8.dp))
        }
        Text(
            text = formatDuration(track.durationMs),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** A single artist row: circular artwork, name. */
@Composable
fun ArtistRow(artist: ArtistItem, onClick: () -> Unit = {}, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ArtworkPlaceholder(
            key = artist.id,
            label = artist.name,
            modifier = Modifier.size(48.dp),
            corner = 24.dp,
            artUrl = artist.artUrl,
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = artist.name,
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.size(2.dp))
            Text(
                text = "Artist",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
    }
}

/** A single album row: square artwork, name, artist. */
@Composable
fun AlbumRow(album: AlbumItem, onClick: () -> Unit = {}, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ArtworkPlaceholder(key = album.id, label = album.name, modifier = Modifier.size(48.dp), artUrl = album.artUrl)
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = album.name,
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.size(2.dp))
            Text(
                text = album.artist,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** Formats a millisecond duration as `m:ss`, matching Spotify's track rows. */
fun formatDuration(durationMs: Long): String {
    if (durationMs <= 0) return "--:--"
    val totalSeconds = TimeUnit.MILLISECONDS.toSeconds(durationMs)
    val minutes = totalSeconds / 60
    val seconds = totalSeconds % 60
    return "%d:%02d".format(minutes, seconds)
}

/** A bold section heading, e.g. "Recently visited" / "Your playlists". */
@Composable
fun SectionHeader(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.titleLarge,
        color = MaterialTheme.colorScheme.onSurface,
        modifier = modifier.padding(horizontal = 16.dp, vertical = 12.dp),
    )
}
