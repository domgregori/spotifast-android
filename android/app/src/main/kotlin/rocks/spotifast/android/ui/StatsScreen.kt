package rocks.spotifast.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.ArtistRow
import rocks.spotifast.android.ui.components.TrackRow

private enum class StatsFilter(val label: String) {
    Songs("Top songs"),
    Artists("Top artists"),
}

/** A scoped-down "Your Stats": the account's top tracks and artists, from
 *  the Web API's public top-tracks and top-artists endpoints — the same
 *  kind of thing Wrapped shows, without Spotify's private analytics
 *  backend. Toggled between the two rather than stacking both in one
 *  scroll. */
@Composable
fun StatsScreen(onClose: () -> Unit) {
    val topTracks by SpotifyCore.topTracks.collectAsState()
    val topArtists by SpotifyCore.topArtists.collectAsState()
    var filter by remember { mutableStateOf(StatsFilter.Songs) }

    LaunchedEffect(Unit) {
        SpotifyCore.loadTopTracks()
        SpotifyCore.loadTopArtists()
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        Row(modifier = Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onClose) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
            Text(
                text = "Your Stats",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
            )
        }
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            StatsFilter.entries.forEach { candidate ->
                FilterChip(
                    selected = filter == candidate,
                    onClick = { filter = candidate },
                    label = { Text(candidate.label) },
                )
            }
        }
        when (filter) {
            StatsFilter.Songs -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                if (topTracks.isEmpty()) {
                    item { EmptyStatsHint() }
                } else {
                    items(topTracks, key = { it.uri }) { track ->
                        TrackRow(
                            track = track,
                            onClick = { SpotifyCore.playTracks(topTracks, topTracks.indexOf(track)) },
                            onLongClick = { SpotifyCore.openTrackMenu(track) },
                        )
                    }
                }
            }
            StatsFilter.Artists -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                if (topArtists.isEmpty()) {
                    item { EmptyStatsHint() }
                } else {
                    items(topArtists, key = { it.id }) { artist ->
                        ArtistRow(artist = artist)
                    }
                }
            }
        }
    }
}

@Composable
private fun EmptyStatsHint() {
    Text(
        text = "Not enough listening history yet.",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
    )
}
