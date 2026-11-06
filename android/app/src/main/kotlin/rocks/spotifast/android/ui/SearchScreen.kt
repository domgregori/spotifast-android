package rocks.spotifast.android.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.History
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
import kotlinx.coroutines.delay
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.AlbumRow
import rocks.spotifast.android.ui.components.ArtistRow
import rocks.spotifast.android.ui.components.PlaylistRow
import rocks.spotifast.android.ui.components.SpotifySearchField
import rocks.spotifast.android.ui.components.TrackRow

/** The kind of result shown below the search field. */
private enum class SearchFilter(val label: String) {
    Songs("Songs"),
    Artists("Artists"),
    Albums("Albums"),
    Playlists("Playlists"),
}

/** Spotify's Search tab: the rounded search field, a row of result-type
 *  chips, and the matching results. The backend already fetches all four
 *  kinds for every query — the chips just pick which one is on screen. */
@Composable
fun SearchScreen(onOpenPlaylist: (String) -> Unit, onOpenArtist: (String) -> Unit, onOpenAlbum: (String) -> Unit) {
    var query by remember { mutableStateOf("") }
    var filter by remember { mutableStateOf(SearchFilter.Songs) }
    val tracks by SpotifyCore.searchTracks.collectAsState()
    val artists by SpotifyCore.searchArtists.collectAsState()
    val albums by SpotifyCore.searchAlbums.collectAsState()
    val playlists by SpotifyCore.searchPlaylists.collectAsState()
    val library by SpotifyCore.library.collectAsState()
    val searchHistory by SpotifyCore.searchHistory.collectAsState()

    LaunchedEffect(query) {
        if (query.isNotBlank()) {
            delay(300)
            SpotifyCore.search(query.trim())
        }
    }

    // A local name match against the library, not a server round trip —
    // Spotify's playlist search only ever indexes public playlists, so a
    // private one of the user's own would otherwise never show up here.
    val myPlaylists = remember(library, query) {
        val needle = query.trim()
        if (needle.isBlank()) {
            emptyList()
        } else {
            (listOf(SpotifyCore.LIKED_SONGS_ITEM) + library).filter { it.name.contains(needle, ignoreCase = true) }
        }
    }
    val otherPlaylists = remember(playlists, myPlaylists) {
        val mineIds = myPlaylists.map { it.id }.toSet()
        playlists.filterNot { it.id in mineIds }
    }

    val resultCount = when (filter) {
        SearchFilter.Songs -> tracks.size
        SearchFilter.Artists -> artists.size
        SearchFilter.Albums -> albums.size
        SearchFilter.Playlists -> myPlaylists.size + otherPlaylists.size
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(top = 8.dp),
    ) {
        SpotifySearchField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        if (query.isNotBlank()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                SearchFilter.entries.forEach { candidate ->
                    FilterChip(
                        selected = filter == candidate,
                        onClick = { filter = candidate },
                        label = { Text(candidate.label) },
                    )
                }
            }
        }
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            when {
                query.isBlank() && searchHistory.isNotEmpty() -> RecentSearches(
                    history = searchHistory,
                    onPick = { query = it },
                    onForget = { SpotifyCore.forgetSearch(it) },
                    onClearAll = { SpotifyCore.clearSearchHistory() },
                )
                query.isBlank() -> Hint("Search for a song, artist, album, or playlist.")
                resultCount == 0 -> Hint("No ${filter.label.lowercase()} for \"$query\".")
                else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                    when (filter) {
                        SearchFilter.Songs -> itemsIndexed(tracks, key = { _, track -> track.uri }) { index, track ->
                            TrackRow(
                                track = track,
                                onClick = { SpotifyCore.playTracks(tracks, index) },
                                onLongClick = { SpotifyCore.openTrackMenu(track) },
                            )
                        }
                        SearchFilter.Artists -> itemsIndexed(artists, key = { _, artist -> artist.id }) { _, artist ->
                            ArtistRow(artist = artist, onClick = { onOpenArtist(artist.id) })
                        }
                        SearchFilter.Albums -> itemsIndexed(albums, key = { _, album -> album.id }) { _, album ->
                            AlbumRow(album = album, onClick = { onOpenAlbum(album.id) })
                        }
                        SearchFilter.Playlists -> {
                            if (myPlaylists.isNotEmpty()) {
                                item { SearchSectionHeader("Your Library") }
                                itemsIndexed(myPlaylists, key = { _, playlist -> "mine:${playlist.id}" }) { _, playlist ->
                                    PlaylistRow(
                                        item = playlist,
                                        subtitle = "Your Library",
                                        onClick = { onOpenPlaylist(playlist.id) },
                                    )
                                }
                            }
                            if (otherPlaylists.isNotEmpty()) {
                                item { SearchSectionHeader("Playlists") }
                                itemsIndexed(otherPlaylists, key = { _, playlist -> "other:${playlist.id}" }) { _, playlist ->
                                    PlaylistRow(item = playlist, onClick = { onOpenPlaylist(playlist.id) })
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun RecentSearches(history: List<String>, onPick: (String) -> Unit, onForget: (String) -> Unit, onClearAll: () -> Unit) {
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        item {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Recent searches",
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                Text(
                    text = "Clear all",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.clickable(onClick = onClearAll),
                )
            }
        }
        items(history, key = { it }) { entry ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { onPick(entry) }
                    .padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    imageVector = Icons.Filled.History,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    text = entry,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.weight(1f).padding(horizontal = 16.dp),
                )
                IconButton(onClick = { onForget(entry) }) {
                    Icon(
                        imageVector = Icons.Filled.Close,
                        contentDescription = "Remove \"$entry\" from recent searches",
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun SearchSectionHeader(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.titleSmall,
        fontWeight = FontWeight.Bold,
        color = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
    )
}

@Composable
private fun Hint(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(32.dp),
    )
}
