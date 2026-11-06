package rocks.spotifast.android.ui.components

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.LyricsLine
import rocks.spotifast.android.PlayingTrack
import rocks.spotifast.android.SpotifyCore

/** The line being sung at [positionMs] in a sorted, synced [lines] list —
 *  the last one whose timestamp hasn't passed yet — or null before the
 *  first line starts, and always for unsynced lyrics. */
private fun activeLineIndex(lines: List<LyricsLine>, positionMs: Long): Int? {
    var active: Int? = null
    for ((index, line) in lines.withIndex()) {
        val at = line.atMs ?: continue
        if (at <= positionMs) active = index else break
    }
    return active
}

/** The lyrics panel for [track]: fetches on open (Spotify's own
 *  transcription first, LRCLIB after — see `core::lyrics`), and follows
 *  [positionMs] to highlight and auto-scroll the active line when the
 *  answer is time-synced. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LyricsSheet(track: PlayingTrack, positionMs: Long, onDismiss: () -> Unit) {
    val result by SpotifyCore.lyrics.collectAsState()
    LaunchedEffect(track.uri) { SpotifyCore.fetchLyrics(track) }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding().height(480.dp)) {
            Text(
                text = "Lyrics",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            val current = result?.takeIf { it.uri == track.uri }
            when {
                current == null -> LyricsHint {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        CircularProgressIndicator(modifier = Modifier.padding(bottom = 12.dp))
                        Text(
                            "Looking for lyrics…",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                current.error != null -> LyricsHint { Text("Couldn't load lyrics right now.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                current.instrumental -> LyricsHint { Text("Instrumental — no lyrics.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                !current.found || current.lines.isEmpty() -> LyricsHint {
                    Text("No lyrics found for this track.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                else -> {
                    val listState = rememberLazyListState()
                    val activeIndex = remember(current.lines, positionMs, current.synced) {
                        if (current.synced) activeLineIndex(current.lines, positionMs) else null
                    }
                    LaunchedEffect(activeIndex) {
                        activeIndex?.let { listState.animateScrollToItem((it - 2).coerceAtLeast(0)) }
                    }
                    LazyColumn(state = listState, modifier = Modifier.fillMaxWidth()) {
                        itemsIndexed(current.lines) { index, line ->
                            val isActive = current.synced && index == activeIndex
                            Text(
                                text = line.text.ifBlank { "···" },
                                style = if (isActive) MaterialTheme.typography.titleMedium else MaterialTheme.typography.bodyLarge,
                                fontWeight = if (isActive) FontWeight.Bold else FontWeight.Normal,
                                color = if (isActive) {
                                    MaterialTheme.colorScheme.onSurface
                                } else {
                                    MaterialTheme.colorScheme.onSurfaceVariant
                                },
                                modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 8.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LyricsHint(content: @Composable () -> Unit) {
    Box(
        modifier = Modifier.fillMaxWidth().padding(32.dp),
        contentAlignment = Alignment.Center,
    ) {
        content()
    }
}
