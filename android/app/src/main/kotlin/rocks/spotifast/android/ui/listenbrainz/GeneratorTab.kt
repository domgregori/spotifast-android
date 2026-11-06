package rocks.spotifast.android.ui.listenbrainz

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import rocks.spotifast.android.ArtistItem
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.SpotifyCore

private enum class GeneratorEntity(val label: String) {
    Artist("Artist"),
    Genre("Genre"),
    Stats("My Stats"),
}

private enum class GeneratorMode(val label: String, val apiValue: String, val caption: String) {
    Easy("Easy", "easy", "Takes more liberties — expect surprises."),
    Medium("Medium", "medium", "A balance of familiar and new."),
    Hard("Hard", "hard", "Sticks close to what you asked for."),
}

/** A guided, 3-step wrapper around LB Radio's prompt syntax
 *  (troi.readthedocs.io/en/latest/lb_radio.html) — pick what to generate
 *  from, fill in one field, pick a mode, Generate. Builds exactly one
 *  `entity:(values)` term; troi supports combining several weighted terms,
 *  deliberately out of scope for this v1. */
@Composable
fun GeneratorTab() {
    var entity by remember { mutableStateOf(GeneratorEntity.Artist) }
    var mode by remember { mutableStateOf(GeneratorMode.Medium) }
    var artistQuery by remember { mutableStateOf("") }
    var selectedArtist by remember { mutableStateOf<ArtistItem?>(null) }
    var tagText by remember { mutableStateOf("") }
    var generating by remember { mutableStateOf(false) }
    val searchArtists by SpotifyCore.searchArtists.collectAsState()

    LaunchedEffect(artistQuery) {
        if (artistQuery.isNotBlank() && artistQuery != selectedArtist?.name) {
            delay(300)
            SpotifyCore.search(artistQuery)
        }
    }

    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
        Text(
            text = "What should it be based on?",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            GeneratorEntity.entries.forEach { candidate ->
                FilterChip(
                    selected = entity == candidate,
                    onClick = {
                        entity = candidate
                        selectedArtist = null
                        artistQuery = ""
                        tagText = ""
                    },
                    label = { Text(candidate.label) },
                )
            }
        }
        Spacer(Modifier.height(16.dp))
        when (entity) {
            GeneratorEntity.Artist -> {
                OutlinedTextField(
                    value = artistQuery,
                    onValueChange = { artistQuery = it; selectedArtist = null },
                    placeholder = { Text("Search for an artist") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (selectedArtist == null && artistQuery.isNotBlank() && searchArtists.isNotEmpty()) {
                    Column {
                        searchArtists.take(5).forEach { artist ->
                            Text(
                                text = artist.name,
                                style = MaterialTheme.typography.bodyLarge,
                                color = MaterialTheme.colorScheme.onSurface,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .clickable {
                                        selectedArtist = artist
                                        artistQuery = artist.name
                                    }
                                    .padding(vertical = 12.dp),
                            )
                        }
                    }
                }
            }
            GeneratorEntity.Genre -> {
                OutlinedTextField(
                    value = tagText,
                    onValueChange = { tagText = it },
                    placeholder = { Text("e.g. deep house, shoegaze") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            GeneratorEntity.Stats -> {
                Text(
                    text = "Uses your own ListenBrainz listening history as the seed.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Spacer(Modifier.height(24.dp))
        Text(
            text = "Mode",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            GeneratorMode.entries.forEach { candidate ->
                FilterChip(
                    selected = mode == candidate,
                    onClick = { mode = candidate },
                    label = { Text(candidate.label) },
                )
            }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            text = mode.caption,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        val enabled = !generating && when (entity) {
            GeneratorEntity.Artist -> selectedArtist != null
            GeneratorEntity.Genre -> tagText.isNotBlank()
            GeneratorEntity.Stats -> ListenBrainz.userName != null
        }
        Button(
            enabled = enabled,
            onClick = {
                when (entity) {
                    GeneratorEntity.Artist -> {
                        val artist = selectedArtist ?: return@Button
                        generating = true
                        ListenBrainz.mbidForSpotifyArtistAsync(artist.id) { mbid ->
                            generating = false
                            ListenBrainz.openGeneratedPlaylist { onResult ->
                                if (mbid != null) {
                                    ListenBrainz.lbRadio("artist:($mbid)", mode.apiValue, onResult)
                                } else {
                                    onResult(
                                        Result.failure(
                                            IllegalStateException(
                                                "${artist.name} isn't linked on MusicBrainz, so LB Radio has nothing to generate from.",
                                            ),
                                        ),
                                    )
                                }
                            }
                        }
                    }
                    GeneratorEntity.Genre -> {
                        val tag = tagText.trim()
                        if (tag.isNotEmpty()) {
                            ListenBrainz.openGeneratedPlaylist { onResult ->
                                ListenBrainz.lbRadio("tag:($tag)", mode.apiValue, onResult)
                            }
                        }
                    }
                    GeneratorEntity.Stats -> {
                        // Unlike artist:(...)/tag:(...), troi's own examples show
                        // stats with no parens around the value (e.g. "stats:rob").
                        val user = ListenBrainz.userName ?: return@Button
                        ListenBrainz.openGeneratedPlaylist { onResult ->
                            ListenBrainz.lbRadio("stats:$user", mode.apiValue, onResult)
                        }
                    }
                }
            },
        ) {
            Text(if (generating) "Resolving artist…" else "Generate")
        }
    }
}
