package rocks.spotifast.android.ui.components

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ClipOp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.vector.rememberVectorPainter
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.GraphicEq
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.SkipNext
import androidx.compose.material.icons.filled.SkipPrevious
import kotlinx.coroutines.delay
import rocks.spotifast.android.PlaybackPhase
import rocks.spotifast.android.PlayingTrack
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.LocalIsNoir

/**
 * A persistent "now playing" style bar anchored above the bottom nav,
 * mirroring Spotify's mini-player chrome. Once the engine has loaded a
 * track, [track] shows it with a play/pause toggle; before that, it falls
 * back to the playback-engine's connection status. Tapping the bar opens
 * the fullscreen player once there's a track, or triggers the existing
 * `authorizePlayback()` flow before that (both via [onClick] — the caller
 * decides which, since it knows [status]).
 */
@Composable
fun MiniPlayerBar(
    status: String,
    detail: String,
    track: PlayingTrack?,
    phase: PlaybackPhase?,
    positionMs: Long,
    onClick: () -> Unit,
    onToggle: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // `track` can be non-null even when this phone's own engine was never
    // authorized (status != "Ready") — it's whatever's actually playing on
    // the account's active Connect device, local or not. See
    // `SpotifyCore.publishEffectivePlayback`.
    if (track != null) {
        NowPlayingMiniBar(
            track = track,
            isPlaying = phase == PlaybackPhase.Playing,
            positionMs = positionMs,
            onClick = onClick,
            onToggle = onToggle,
            modifier = modifier,
        )
        return
    }

    val (headline, subtext, showSpinner) = when (status) {
        "Unavailable" -> Triple("Playback not connected", "Tap to connect this device", false)
        "Authorizing" -> Triple("Connecting playback…", "Waiting for authorization", true)
        "Connecting" -> Triple("Connecting playback…", "", true)
        "WaitingForNetwork" -> Triple("Waiting for network…", "", true)
        // `detail` here is this phone's own raw Connect device id, not a
        // name — shown as this install's own randomized Connect name
        // instead (see SpotifyCore.localDeviceName).
        "Ready" -> Triple("Playback ready", SpotifyCore.localDeviceName, false)
        "Failed" -> Triple("Playback unavailable", detail.ifBlank { "Something went wrong" }, false)
        else -> Triple(status, detail, false)
    }

    val noir = LocalIsNoir.current
    Row(
        modifier = modifier
            .fillMaxWidth()
            .let {
                if (noir) {
                    it.padding(horizontal = 12.dp, vertical = 8.dp)
                        .height(64.dp)
                        .clip(RoundedCornerShape(16.dp))
                        .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                        .border(2.5.dp, MaterialTheme.colorScheme.onSurface, RoundedCornerShape(16.dp))
                } else {
                    it.height(56.dp).background(MaterialTheme.colorScheme.surfaceContainerHigh)
                }
            }
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(40.dp)
                .clip(RoundedCornerShape(if (noir) 10.dp else 6.dp))
                .let { if (noir) it.border(2.dp, MaterialTheme.colorScheme.onSurface, RoundedCornerShape(10.dp)) else it }
                .background(
                    if (status == "Ready") {
                        MaterialTheme.colorScheme.primaryContainer
                    } else {
                        MaterialTheme.colorScheme.surfaceContainerHighest
                    },
                ),
            contentAlignment = Alignment.Center,
        ) {
            if (showSpinner) {
                CircularProgressIndicator(
                    modifier = Modifier.size(18.dp),
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            } else {
                Icon(
                    imageVector = Icons.Filled.GraphicEq,
                    contentDescription = null,
                    tint = if (status == "Ready") {
                        MaterialTheme.colorScheme.onPrimaryContainer
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
        }
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = headline,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (subtext.isNotBlank()) {
                Text(
                    text = subtext,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (status != "Ready" && !showSpinner) {
            Spacer(Modifier.width(8.dp))
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .let {
                        if (noir) {
                            it.border(2.dp, MaterialTheme.colorScheme.onSurface, CircleShape)
                                .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                        } else {
                            it.background(MaterialTheme.colorScheme.primary)
                        }
                    },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Filled.PlayArrow,
                    contentDescription = "Connect playback",
                    tint = if (noir) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onPrimary,
                    modifier = Modifier.size(20.dp),
                )
            }
        }
    }
}

@Composable
private fun NowPlayingMiniBar(
    track: PlayingTrack,
    isPlaying: Boolean,
    positionMs: Long,
    onClick: () -> Unit,
    onToggle: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val noir = LocalIsNoir.current

    // Mirrors NowPlayingScreen's own interpolation, so the pie fill on the
    // Noir play/pause button advances smoothly rather than only jumping on
    // each engine poll.
    var elapsedMs by remember(track.uri, positionMs) { mutableLongStateOf(positionMs) }
    LaunchedEffect(track.uri, positionMs, isPlaying) {
        elapsedMs = positionMs
        if (isPlaying) {
            while (true) {
                delay(500)
                val next = elapsedMs + 500
                elapsedMs = if (track.durationMs > 0) next.coerceAtMost(track.durationMs) else next
            }
        }
    }
    val progress = if (track.durationMs > 0) (elapsedMs.toFloat() / track.durationMs).coerceIn(0f, 1f) else 0f

    Row(
        modifier = modifier
            .fillMaxWidth()
            .let {
                if (noir) {
                    // A floating cream-bordered panel rather than a flush
                    // bar — the comic reference's mini player reads as its
                    // own inked card sitting just above the nav, not a
                    // strip glued to the screen edge.
                    it.padding(horizontal = 12.dp, vertical = 8.dp)
                        .height(64.dp)
                        .clip(RoundedCornerShape(16.dp))
                        .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                        .border(2.5.dp, MaterialTheme.colorScheme.onSurface, RoundedCornerShape(16.dp))
                } else {
                    it.height(56.dp).background(MaterialTheme.colorScheme.surfaceContainerHigh)
                }
            }
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ArtworkPlaceholder(
            key = track.uri,
            label = track.title,
            artUrl = track.artUrl,
            modifier = Modifier.size(40.dp),
            corner = if (noir) 8.dp else 6.dp,
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = if (noir) track.title.uppercase() else track.title,
                style = if (noir) MaterialTheme.typography.titleSmall else MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = track.artist,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.width(4.dp))
        if (noir) {
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .border(2.dp, MaterialTheme.colorScheme.onSurface, CircleShape)
                    .clickable(onClick = { SpotifyCore.skipPrevious() }),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Filled.SkipPrevious,
                    contentDescription = "Previous",
                    tint = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.size(18.dp),
                )
            }
            Spacer(Modifier.width(4.dp))
            val pieBase = MaterialTheme.colorScheme.surfaceContainerHigh
            val pieElapsed = MaterialTheme.colorScheme.onSurface
            val playPauseIcon = if (isPlaying) Icons.Filled.Pause else Icons.Filled.PlayArrow
            val playPauseIconPainter = rememberVectorPainter(image = playPauseIcon)
            Box(
                modifier = Modifier
                    .size(40.dp)
                    .clip(CircleShape)
                    .border(2.dp, MaterialTheme.colorScheme.onSurface, CircleShape)
                    .clickable(onClick = onToggle)
                    .semantics { contentDescription = if (isPlaying) "Pause" else "Play" },
            ) {
                // A pie-chart fill tracking playback position — inked-in
                // progress rather than a ring, matching the Noir style's
                // flat, solid-shape look elsewhere. The icon is drawn twice,
                // each half clipped to one side of the wedge boundary and
                // tinted opposite the fill under it, so it stays legible
                // right at the intersection instead of just picking one
                // tint for the whole icon based on overall progress.
                Canvas(modifier = Modifier.matchParentSize()) {
                    drawCircle(color = pieBase)
                    val wedge = Path().apply {
                        moveTo(center.x, center.y)
                        arcTo(
                            rect = Rect(Offset.Zero, size),
                            startAngleDegrees = -90f,
                            sweepAngleDegrees = 360f * progress,
                            forceMoveTo = false,
                        )
                        close()
                    }
                    if (progress > 0f) {
                        drawPath(wedge, color = pieElapsed)
                    }

                    val iconSizePx = 22.dp.toPx()
                    val iconOffset = Offset(
                        (this.size.width - iconSizePx) / 2f,
                        (this.size.height - iconSizePx) / 2f,
                    )
                    fun drawIcon(tint: Color) = translate(iconOffset.x, iconOffset.y) {
                        with(playPauseIconPainter) {
                            draw(size = Size(iconSizePx, iconSizePx), colorFilter = ColorFilter.tint(tint))
                        }
                    }
                    if (progress <= 0f) {
                        drawIcon(pieElapsed)
                    } else {
                        clipPath(wedge, clipOp = ClipOp.Difference) { drawIcon(pieElapsed) }
                        clipPath(wedge, clipOp = ClipOp.Intersect) { drawIcon(pieBase) }
                    }
                }
            }
            Spacer(Modifier.width(4.dp))
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .border(2.dp, MaterialTheme.colorScheme.onSurface, CircleShape)
                    .clickable(onClick = { SpotifyCore.skipNext() }),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Filled.SkipNext,
                    contentDescription = "Next",
                    tint = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.size(18.dp),
                )
            }
        } else {
            IconButton(onClick = { SpotifyCore.skipPrevious() }) {
                Icon(
                    imageVector = Icons.Filled.SkipPrevious,
                    contentDescription = "Previous",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
            IconButton(onClick = onToggle) {
                if (isPlaying) {
                    Icon(
                        imageVector = Icons.Filled.Pause,
                        contentDescription = "Pause",
                        tint = MaterialTheme.colorScheme.onSurface,
                    )
                } else {
                    Icon(
                        imageVector = Icons.Filled.PlayArrow,
                        contentDescription = "Play",
                        tint = MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
            IconButton(onClick = { SpotifyCore.skipNext() }) {
                Icon(
                    imageVector = Icons.Filled.SkipNext,
                    contentDescription = "Next",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
    }
}
