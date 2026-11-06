package rocks.spotifast.android.ui.listenbrainz

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.CreatedForEntry
import rocks.spotifast.android.ListenBrainz

private val MONTH_NAMES = listOf(
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
)

/** Weekly Jams and Weekly Exploration, combined into one grid — each tile
 *  is "Jams" or "Exploration" (from [CreatedForEntry.sourcePatch]) plus the
 *  date ListenBrainz generated it, newest first; this week's and (while
 *  still listed) last week's instance of each show up as peers, same as
 *  listenbrainz.org's own four tiles. Tapping one opens the shared
 *  [rocks.spotifast.android.ui.GeneratedPlaylistScreen]. */
@Composable
fun WeeklyPlaylistsTab() {
    var result by remember { mutableStateOf<Result<List<CreatedForEntry>>?>(null) }

    LaunchedEffect(Unit) {
        ListenBrainz.createdForPlaylists { fetched -> result = fetched }
    }

    when {
        result == null -> Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        result?.isFailure == true -> Text(
            text = result?.exceptionOrNull()?.message ?: "Couldn't load this.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
        )
        else -> {
            val entries = result?.getOrNull().orEmpty()
                .filter { it.sourcePatch == "weekly-jams" || it.sourcePatch == "weekly-exploration" }
                .sortedByDescending { it.date }
            if (entries.isEmpty()) {
                Text(
                    text = "Not generated yet — check back Monday morning.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
                )
            } else {
                LazyVerticalGrid(
                    columns = GridCells.Fixed(2),
                    contentPadding = PaddingValues(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(entries, key = { it.mbid }) { entry ->
                        WeeklyPlaylistCard(
                            entry = entry,
                            onClick = {
                                ListenBrainz.openGeneratedPlaylist { onResult ->
                                    ListenBrainz.playlistByMbid(entry.mbid, onResult)
                                }
                            },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun WeeklyPlaylistCard(entry: CreatedForEntry, onClick: () -> Unit) {
    val kind = when (entry.sourcePatch) {
        "weekly-jams" -> "Jams"
        "weekly-exploration" -> "Exploration"
        else -> "Playlist"
    }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .aspectRatio(1f)
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceContainer)
            .clickable(onClick = onClick)
            .padding(16.dp),
        verticalArrangement = Arrangement.Bottom,
    ) {
        Text(
            text = kind,
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Text(
            text = formatOrdinalDate(entry.date),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** "2026-09-14T00:14:16..." -> "Sep 14th". No `java.time` dependency —
 *  just slices the ISO date's fixed-width prefix. */
private fun formatOrdinalDate(iso: String?): String {
    if (iso == null || iso.length < 10) return ""
    val month = iso.substring(5, 7).toIntOrNull() ?: return ""
    val day = iso.substring(8, 10).toIntOrNull() ?: return ""
    val monthName = MONTH_NAMES.getOrNull(month - 1) ?: return ""
    val suffix = when {
        day in 11..13 -> "th"
        day % 10 == 1 -> "st"
        day % 10 == 2 -> "nd"
        day % 10 == 3 -> "rd"
        else -> "th"
    }
    return "$monthName $day$suffix"
}
