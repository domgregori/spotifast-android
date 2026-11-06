package rocks.spotifast.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import rocks.spotifast.android.PlaybackPhase
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.ArtworkPlaceholder
import rocks.spotifast.android.ui.components.formatDuration

/**
 * The content of [rocks.spotifast.android.service.PlaybackDreamService] —
 * a wide, landscape-oriented ambient "now playing" view: large artwork on
 * one side, track details and a progress bar on the other. Purely a
 * read-only display of [SpotifyCore.localPlayback] (whichever device is
 * actually playing, same as every other screen) — the dream itself is
 * non-interactive, so there's nothing here to tap.
 *
 * `DreamService` has no API to lock its own orientation (unlike an
 * `Activity`), so this is always laid out wide — on a device that happens
 * to be dreaming in portrait it will look cramped rather than switching to
 * a stacked layout. Daydreams run while docked/charging, where a phone is
 * typically already lying in landscape, so this is the common case rather
 * than the exception.
 */
@Composable
fun DreamNowPlayingScreen() {
    val localPlayback by SpotifyCore.localPlayback.collectAsState()
    val track = localPlayback?.track
    val phase = localPlayback?.phase ?: PlaybackPhase.Stopped
    val positionMs = localPlayback?.positionMs ?: 0

    var elapsedMs by remember(track?.uri, positionMs) { mutableLongStateOf(positionMs) }
    LaunchedEffect(track?.uri, positionMs, phase) {
        elapsedMs = positionMs
        if (phase == PlaybackPhase.Playing) {
            while (true) {
                delay(500)
                val next = elapsedMs + 500
                val duration = track?.durationMs ?: 0
                elapsedMs = if (duration > 0) next.coerceAtMost(duration) else next
            }
        }
    }

    Box(
        modifier = Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background),
        contentAlignment = Alignment.Center,
    ) {
        if (track == null) {
            Text(
                text = "Spotifast",
                style = MaterialTheme.typography.headlineMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            Row(
                modifier = Modifier.fillMaxSize().padding(64.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ArtworkPlaceholder(
                    key = track.uri,
                    label = track.title,
                    artUrl = track.artUrl,
                    modifier = Modifier.fillMaxHeight().aspectRatio(1f).clip(RoundedCornerShape(16.dp)),
                    corner = 16.dp,
                )
                Spacer(Modifier.width(56.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = track.title,
                        style = MaterialTheme.typography.displaySmall,
                        fontWeight = FontWeight.ExtraBold,
                        color = MaterialTheme.colorScheme.onSurface,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Spacer(Modifier.height(12.dp))
                    Text(
                        text = track.artist,
                        style = MaterialTheme.typography.headlineSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (track.album.isNotBlank()) {
                        Spacer(Modifier.height(4.dp))
                        Text(
                            text = track.album,
                            style = MaterialTheme.typography.titleMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    Spacer(Modifier.height(40.dp))
                    val fraction = if (track.durationMs > 0) {
                        (elapsedMs.toFloat() / track.durationMs).coerceIn(0f, 1f)
                    } else {
                        0f
                    }
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
                                .fillMaxWidth(fraction)
                                .background(MaterialTheme.colorScheme.primary),
                        )
                    }
                    Spacer(Modifier.height(8.dp))
                    Row(modifier = Modifier.fillMaxWidth()) {
                        Text(
                            text = formatDuration(elapsedMs),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Spacer(Modifier.weight(1f))
                        Text(
                            text = formatDuration(track.durationMs),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}
