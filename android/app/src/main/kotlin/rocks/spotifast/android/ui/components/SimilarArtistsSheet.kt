package rocks.spotifast.android.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PersonOff
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.SimilarArtist

/** Artists similar to the one whose context menu this was opened from,
 *  sourced from ListenBrainz's listening-similarity graph rather than
 *  Spotify's own (Development-Mode-restricted) related-artists endpoint.
 *  Each row only resolves to a playable Spotify artist when tapped —
 *  [resolving] names the one in flight, and a tap that comes back with no
 *  Spotify link just marks that row [unresolvable] instead of navigating. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SimilarArtistsSheet(
    artistName: String,
    result: Result<List<SimilarArtist>>?,
    resolving: String?,
    unresolvable: Set<String>,
    onSelect: (SimilarArtist) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Text(
            text = "Similar to $artistName",
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        Text(
            text = "From ListenBrainz's listening-similarity data.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(start = 24.dp, end = 24.dp, bottom = 8.dp),
        )
        when {
            result == null -> Box(
                modifier = Modifier.fillMaxWidth().padding(32.dp),
                contentAlignment = Alignment.Center,
            ) {
                CircularProgressIndicator()
            }
            result.isFailure -> Text(
                text = result.exceptionOrNull()?.message ?: "Couldn't load similar artists.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp).navigationBarsPadding(),
            )
            else -> {
                val list = result.getOrDefault(emptyList())
                if (list.isEmpty()) {
                    Text(
                        text = "No similar artists found.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp).navigationBarsPadding(),
                    )
                } else {
                    LazyColumn(modifier = Modifier.heightIn(max = 420.dp)) {
                        items(list, key = { it.mbid }) { similar ->
                            SimilarArtistRow(
                                similar = similar,
                                resolving = resolving == similar.mbid,
                                unresolvable = similar.mbid in unresolvable,
                                onClick = { onSelect(similar) },
                            )
                        }
                        item { Spacer(Modifier.navigationBarsPadding()) }
                    }
                }
            }
        }
    }
}

@Composable
private fun SimilarArtistRow(
    similar: SimilarArtist,
    resolving: Boolean,
    unresolvable: Boolean,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = !resolving, onClick = onClick)
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            resolving -> CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
            unresolvable -> Icon(
                imageVector = Icons.Filled.PersonOff,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else -> Icon(
                imageVector = Icons.Filled.Person,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.width(20.dp))
        Text(
            text = similar.name,
            style = MaterialTheme.typography.bodyLarge,
            color = if (unresolvable) {
                MaterialTheme.colorScheme.onSurfaceVariant
            } else {
                MaterialTheme.colorScheme.onSurface
            },
        )
        if (unresolvable) {
            Spacer(Modifier.width(8.dp))
            Text(
                text = "not on Spotify",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
