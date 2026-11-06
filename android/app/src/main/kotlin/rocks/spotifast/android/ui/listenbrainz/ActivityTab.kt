package rocks.spotifast.android.ui.listenbrainz

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
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
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.ListenBrainz

private enum class ActivityRange(val label: String, val apiValue: String) {
    Week("Week", "week"),
    Month("Month", "month"),
    Year("Year", "year"),
    AllTime("All time", "all_time"),
}

private val WEEKDAY_LABELS = listOf(
    "Monday" to "Mon",
    "Tuesday" to "Tue",
    "Wednesday" to "Wed",
    "Thursday" to "Thu",
    "Friday" to "Fri",
    "Saturday" to "Sat",
    "Sunday" to "Sun",
)

/** An hour x day-of-week heatmap of the signed-in user's own listening
 *  history — no Spotify matching involved at all, pure ListenBrainz
 *  stats, so unlike the generator tabs this never has an "unmatched"
 *  state to show. */
@Composable
fun ActivityTab() {
    var range by remember { mutableStateOf(ActivityRange.Week) }
    var result by remember { mutableStateOf<Result<Map<String, List<Int>>>?>(null) }

    LaunchedEffect(range) {
        result = null
        ListenBrainz.dailyActivity(range.apiValue) { fetched -> result = fetched }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            ActivityRange.entries.forEach { candidate ->
                FilterChip(
                    selected = range == candidate,
                    onClick = { range = candidate },
                    label = { Text(candidate.label) },
                )
            }
        }
        when {
            result == null -> Box(
                modifier = Modifier.fillMaxWidth().padding(32.dp),
                contentAlignment = Alignment.Center,
            ) {
                CircularProgressIndicator()
            }
            result?.isFailure == true -> Text(
                text = result?.exceptionOrNull()?.message ?: "Couldn't load activity.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
            )
            else -> ActivityHeatmap(result?.getOrNull().orEmpty())
        }
    }
}

@Composable
private fun ActivityHeatmap(data: Map<String, List<Int>>) {
    val total = data.values.sumOf { hours -> hours.sum() }
    val maxCount = data.values.flatten().maxOrNull()?.coerceAtLeast(1) ?: 1
    val filled = MaterialTheme.colorScheme.primary
    val empty = MaterialTheme.colorScheme.surfaceVariant
    val labelColor = MaterialTheme.colorScheme.onSurfaceVariant

    Column(modifier = Modifier.padding(16.dp)) {
        Text(
            text = if (total == 1) "1 listen" else "$total listens",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Spacer(Modifier.height(12.dp))
        Row(modifier = Modifier.horizontalScroll(rememberScrollState())) {
            Column {
                Row(modifier = Modifier.padding(start = 40.dp)) {
                    for (hour in 0 until 24 step 4) {
                        Text(
                            text = "%02d".format(hour),
                            style = MaterialTheme.typography.labelSmall,
                            color = labelColor,
                            modifier = Modifier.width(16.dp * 4),
                        )
                    }
                }
                WEEKDAY_LABELS.forEach { (key, label) ->
                    val counts = data[key].orEmpty()
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            text = label,
                            style = MaterialTheme.typography.labelSmall,
                            color = labelColor,
                            modifier = Modifier.width(40.dp),
                        )
                        for (hour in 0 until 24) {
                            val count = counts.getOrElse(hour) { 0 }
                            val intensity = count / maxCount.toFloat()
                            Box(
                                modifier = Modifier
                                    .size(16.dp)
                                    .padding(1.dp)
                                    .background(
                                        color = if (count == 0) empty.copy(alpha = 0.4f) else filled.copy(alpha = 0.15f + intensity * 0.85f),
                                        shape = RoundedCornerShape(2.dp),
                                    ),
                            )
                        }
                    }
                }
            }
        }
    }
}
