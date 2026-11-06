package rocks.spotifast.android.ui.listenbrainz

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Album
import androidx.compose.material.icons.filled.Block
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
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
import rocks.spotifast.android.FreshRelease
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.SpotifyCore

/** The global fresh-releases feed, with a client-side "For You" filter
 *  (ListenBrainz has no server-side personalization param for this
 *  endpoint — confirmed against its OpenAPI spec) against the user's own
 *  top-artist MBIDs. Each release matches to a playable Spotify album
 *  lazily, only when tapped — release-level MusicBrainz coverage has real
 *  gaps (confirmed live, worse for brand-new/indie releases specifically),
 *  so an unmatched release just shows a "not on Spotify yet" state rather
 *  than being hidden or blocking the rest of the feed. */
@Composable
fun FreshReleasesTab() {
    var showForYou by remember { mutableStateOf(true) }
    var releasesResult by remember { mutableStateOf<Result<List<FreshRelease>>?>(null) }
    var topArtistMbids by remember { mutableStateOf<Set<String>?>(null) }
    var resolvingMbid by remember { mutableStateOf<String?>(null) }
    var unresolvable by remember { mutableStateOf<Set<String>>(emptySet()) }

    LaunchedEffect(Unit) {
        ListenBrainz.freshReleases(days = 14) { result -> releasesResult = result }
        ListenBrainz.topArtistMbids { result -> topArtistMbids = result.getOrNull() ?: emptySet() }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            FilterChip(selected = showForYou, onClick = { showForYou = true }, label = { Text("For You") })
            FilterChip(selected = !showForYou, onClick = { showForYou = false }, label = { Text("All") })
        }
        when {
            releasesResult == null -> Box(
                modifier = Modifier.fillMaxWidth().padding(32.dp),
                contentAlignment = Alignment.Center,
            ) {
                CircularProgressIndicator()
            }
            releasesResult?.isFailure == true -> Text(
                text = releasesResult?.exceptionOrNull()?.message ?: "Couldn't load fresh releases.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
            )
            else -> {
                val all = releasesResult?.getOrNull().orEmpty()
                val mine = topArtistMbids
                val filtered = when {
                    !showForYou -> all
                    mine == null -> null // still loading top artists
                    else -> all.filter { release -> release.artistMbids.any { it in mine } }
                }
                when {
                    filtered == null -> Box(
                        modifier = Modifier.fillMaxWidth().padding(32.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator()
                    }
                    filtered.isEmpty() -> Text(
                        text = "No releases from artists you've listened to recently.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
                    )
                    else -> LazyColumn {
                        items(filtered, key = { it.releaseMbid }) { release ->
                            FreshReleaseRow(
                                release = release,
                                resolving = resolvingMbid == release.releaseMbid,
                                unresolvable = release.releaseMbid in unresolvable,
                                onClick = {
                                    resolvingMbid = release.releaseMbid
                                    ListenBrainz.resolveFreshReleaseSpotifyId(release.releaseMbid) { spotifyId ->
                                        resolvingMbid = null
                                        if (spotifyId != null) {
                                            SpotifyCore.openAlbum(spotifyId)
                                        } else {
                                            unresolvable = unresolvable + release.releaseMbid
                                        }
                                    }
                                },
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun FreshReleaseRow(release: FreshRelease, resolving: Boolean, unresolvable: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = !resolving && !unresolvable, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            resolving -> CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
            unresolvable -> Icon(
                imageVector = Icons.Filled.Block,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else -> Icon(
                imageVector = Icons.Filled.Album,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.width(16.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = release.title,
                style = MaterialTheme.typography.bodyLarge,
                color = if (unresolvable) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            Text(
                text = release.artist,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
        Text(
            text = if (unresolvable) "not on Spotify yet" else release.releaseDate,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
