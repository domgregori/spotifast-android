package rocks.spotifast.android.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Album
import androidx.compose.material.icons.filled.Cast
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Radio
import androidx.compose.material.icons.filled.Repeat
import androidx.compose.material.icons.filled.RepeatOne
import androidx.compose.material.icons.filled.Shuffle
import androidx.compose.material.icons.filled.SkipNext
import androidx.compose.material.icons.filled.SkipPrevious
import androidx.compose.material.icons.automirrored.filled.QueueMusic
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import rocks.spotifast.android.CurrentSource
import rocks.spotifast.android.PlaybackPhase
import rocks.spotifast.android.PlayingTrack
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.ArtworkPlaceholder
import rocks.spotifast.android.ui.components.DevicesSheet
import rocks.spotifast.android.ui.components.LyricsSheet
import rocks.spotifast.android.ui.components.formatDuration

/**
 * Spotify's fullscreen now-playing view: large artwork, transport controls,
 * and a progress bar interpolated locally between the engine's position
 * reports (ticking every half second while [phase] is [PlaybackPhase.Playing],
 * the same way the engine's own `position_now()` does on the Rust side).
 *
 * Dismisses on a downward swipe anywhere on the screen (the chevron button
 * stays as a tap target too). A long press on the artwork opens the same
 * track context menu every track row elsewhere in the app uses.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun NowPlayingScreen(
    track: PlayingTrack,
    phase: PlaybackPhase,
    positionMs: Long,
    shuffle: Boolean,
    repeat: String,
    activeRadio: String?,
    currentSource: CurrentSource?,
    onClose: () -> Unit,
    onOpenQueue: () -> Unit,
    onLongPressCover: () -> Unit,
    onGoToSource: (() -> Unit)? = null,
) {
    var devicesOpen by remember { mutableStateOf(false) }
    var lyricsOpen by remember { mutableStateOf(false) }
    val likedTracks by SpotifyCore.likedTracks.collectAsState()
    val liked = likedTracks[track.uri] ?: false
    LaunchedEffect(track.uri) { SpotifyCore.checkLiked(track.uri) }

    var elapsedMs by remember(track.uri, positionMs) { mutableLongStateOf(positionMs) }
    LaunchedEffect(track.uri, positionMs, phase) {
        elapsedMs = positionMs
        if (phase == PlaybackPhase.Playing) {
            while (true) {
                delay(500)
                val next = elapsedMs + 500
                elapsedMs = if (track.durationMs > 0) next.coerceAtMost(track.durationMs) else next
            }
        }
    }

    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    val dismissThresholdPx = with(density) { 120.dp.toPx() }

    val offsetY = remember { Animatable(0f) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding()
            .offset { IntOffset(0, offsetY.value.roundToInt()) }
            .pointerInput(Unit) {
                detectVerticalDragGestures(
                    onVerticalDrag = { change, dragAmount ->
                        change.consume()
                        scope.launch { offsetY.snapTo((offsetY.value + dragAmount).coerceAtLeast(0f)) }
                    },
                    onDragEnd = {
                        if (offsetY.value > dismissThresholdPx) {
                            onClose()
                        } else {
                            scope.launch { offsetY.animateTo(0f) }
                        }
                    },
                )
            }
            .padding(horizontal = 24.dp, vertical = 12.dp),
    ) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            IconButton(onClick = onClose) {
                Icon(
                    imageVector = Icons.Filled.KeyboardArrowDown,
                    contentDescription = "Close",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
            Row {
                IconButton(onClick = { devicesOpen = true }) {
                    Icon(
                        imageVector = Icons.Filled.Cast,
                        contentDescription = "Connect to a device",
                        tint = MaterialTheme.colorScheme.onSurface,
                    )
                }
                IconButton(onClick = { lyricsOpen = true }) {
                    Icon(
                        imageVector = Icons.Filled.MusicNote,
                        contentDescription = "Lyrics",
                        tint = MaterialTheme.colorScheme.onSurface,
                    )
                }
                IconButton(onClick = onOpenQueue) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.QueueMusic,
                        contentDescription = "Queue",
                        tint = MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
        }
        if (devicesOpen) {
            DevicesSheet(onDismiss = { devicesOpen = false })
        }
        if (lyricsOpen) {
            LyricsSheet(track = track, positionMs = elapsedMs, onDismiss = { lyricsOpen = false })
        }

        // The art and album name are grouped into one tight block (the
        // name sits a fixed gap below the art, no slack between them),
        // and that whole block — not the art alone — is what's centered
        // in the flexible space this box absorbs.
        Box(modifier = Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                ArtworkPlaceholder(
                    key = track.uri,
                    label = track.title,
                    artUrl = track.artUrl,
                    modifier = Modifier
                        .fillMaxWidth(0.82f)
                        .aspectRatio(1f)
                        .pointerInput(track.uri) {
                            detectTapGestures(onLongPress = { onLongPressCover() })
                        },
                    corner = 8.dp,
                )
                if (track.album.isNotBlank()) {
                    // dp is a physical-size unit (160dp = 1 inch), so this
                    // is a real ~5mm regardless of screen density.
                    Spacer(Modifier.height(32.dp))
                    Text(
                        text = track.album,
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }

        Spacer(Modifier.height(16.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(modifier = Modifier.weight(1f)) {
                if (activeRadio != null) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            imageVector = Icons.Filled.Radio,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.size(14.dp),
                        )
                        Spacer(Modifier.width(4.dp))
                        Text(
                            text = activeRadio,
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.primary,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    Spacer(Modifier.height(2.dp))
                } else if (currentSource != null && onGoToSource != null) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.clickable(onClick = onGoToSource),
                    ) {
                        Icon(
                            imageVector = when (currentSource) {
                                is CurrentSource.Playlist -> Icons.AutoMirrored.Filled.QueueMusic
                                is CurrentSource.Artist -> Icons.Filled.Person
                                is CurrentSource.Album -> Icons.Filled.Album
                            },
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.size(14.dp),
                        )
                        Spacer(Modifier.width(4.dp))
                        Text(
                            text = when (currentSource) {
                                is CurrentSource.Playlist -> "Go to playlist"
                                is CurrentSource.Artist -> "Go to artist"
                                is CurrentSource.Album -> "Go to album"
                            },
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.primary,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    Spacer(Modifier.height(2.dp))
                }
                Text(
                    text = track.title,
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.ExtraBold,
                    color = MaterialTheme.colorScheme.onSurface,
                    textAlign = TextAlign.Start,
                )
                Spacer(Modifier.height(4.dp))
                Text(
                    text = track.artist,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // Tap likes; a stray tap on an already-liked track should never
            // unlike it — that needs a deliberate hold instead.
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .combinedClickable(
                        onClick = { SpotifyCore.setLiked(track.uri, true) },
                        onLongClick = { SpotifyCore.setLiked(track.uri, false) },
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = if (liked) Icons.Filled.Favorite else Icons.Outlined.FavoriteBorder,
                    contentDescription = if (liked) "Hold to remove from Liked Songs" else "Save to Liked Songs",
                    tint = if (liked) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        Spacer(Modifier.height(16.dp))
        val fraction = if (track.durationMs > 0) {
            (elapsedMs.toFloat() / track.durationMs).coerceIn(0f, 1f)
        } else {
            0f
        }
        // A seek drag/tap shows its own fraction immediately — not the
        // engine's elapsedMs, which only catches up once the seek call's
        // own `onNowPlaying`/poll confirmation lands.
        var seekFraction by remember(track.uri) { mutableFloatStateOf(-1f) }
        var barWidthPx by remember { mutableFloatStateOf(0f) }

        fun fractionAt(x: Float): Float = if (barWidthPx > 0f) (x / barWidthPx).coerceIn(0f, 1f) else 0f
        fun commitSeek(seekTo: Float) {
            if (track.durationMs <= 0) return
            val positionMs = (seekTo * track.durationMs).toLong()
            elapsedMs = positionMs
            SpotifyCore.seek(positionMs)
        }

        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(24.dp)
                .onSizeChanged { barWidthPx = it.width.toFloat() }
                .pointerInput(track.uri, track.durationMs) {
                    detectTapGestures(onTap = { offset ->
                        val tapped = fractionAt(offset.x)
                        seekFraction = tapped
                        commitSeek(tapped)
                        seekFraction = -1f
                    })
                }
                .pointerInput(track.uri, track.durationMs) {
                    detectHorizontalDragGestures(
                        onHorizontalDrag = { change, _ ->
                            change.consume()
                            seekFraction = fractionAt(change.position.x)
                        },
                        onDragEnd = {
                            val dragged = seekFraction
                            if (dragged >= 0f) commitSeek(dragged)
                            seekFraction = -1f
                        },
                    )
                },
            contentAlignment = Alignment.CenterStart,
        ) {
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(4.dp)
                    .clip(RoundedCornerShape(2.dp))
                    .background(MaterialTheme.colorScheme.outlineVariant),
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxHeight()
                        .fillMaxWidth(if (seekFraction >= 0f) seekFraction else fraction)
                        .background(MaterialTheme.colorScheme.onSurface),
                )
            }
        }
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                text = formatDuration(
                    if (seekFraction >= 0f) (seekFraction * track.durationMs).toLong() else elapsedMs,
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = formatDuration(track.durationMs),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Spacer(Modifier.height(24.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = { SpotifyCore.toggleShuffle() }) {
                Icon(
                    imageVector = Icons.Filled.Shuffle,
                    contentDescription = "Shuffle",
                    tint = if (shuffle) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            IconButton(onClick = { SpotifyCore.skipPrevious() }) {
                Icon(
                    imageVector = Icons.Filled.SkipPrevious,
                    contentDescription = "Previous",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
            IconButton(onClick = { SpotifyCore.togglePlayback() }, modifier = Modifier.size(64.dp)) {
                if (phase == PlaybackPhase.Playing) {
                    Icon(
                        imageVector = Icons.Filled.Pause,
                        contentDescription = "Pause",
                        tint = MaterialTheme.colorScheme.onSurface,
                        modifier = Modifier.size(36.dp),
                    )
                } else {
                    Icon(
                        imageVector = Icons.Filled.PlayArrow,
                        contentDescription = "Play",
                        tint = MaterialTheme.colorScheme.onSurface,
                        modifier = Modifier.size(36.dp),
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
            IconButton(onClick = { SpotifyCore.cycleRepeat() }) {
                Icon(
                    imageVector = if (repeat == "Track") Icons.Filled.RepeatOne else Icons.Filled.Repeat,
                    contentDescription = "Repeat",
                    tint = if (repeat != "Off") MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
