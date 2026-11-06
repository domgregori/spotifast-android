package rocks.spotifast.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.BarChart
import androidx.compose.material.icons.filled.NewReleases
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
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.LibraryItem
import rocks.spotifast.android.LibraryNode
import rocks.spotifast.android.LibrarySort
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.flattenPlaylists
import rocks.spotifast.android.pinLikedSongsFirst
import rocks.spotifast.android.sortLibraryItems
import rocks.spotifast.android.ui.components.AlbumRow
import rocks.spotifast.android.ui.components.ArtistRow
import rocks.spotifast.android.ui.components.FilterBar
import rocks.spotifast.android.ui.components.FolderRow
import rocks.spotifast.android.ui.components.PlaylistRow
import rocks.spotifast.android.ui.components.SectionHeader
import rocks.spotifast.android.ui.components.SortMenuButton
import rocks.spotifast.android.ui.components.rememberPullToRevealConnection

private enum class LibraryFilter(val label: String) {
    Playlists("Playlists"),
    Albums("Albums"),
    Artists("Artists"),
}

/**
 * Spotify's "Your Library" tab: a bold header and a scrollable playlist
 * list, following Spotify's own folder structure (read from the session's
 * rootlist — folders have no public Web API representation) with pinned
 * items pulled out to the top regardless of which folder they're in,
 * matching real Spotify's behavior. Pulling down from the very top reveals
 * a filter field, which searches every playlist flat, folders or not.
 */
@Composable
fun LibraryScreen(
    onOpenPlaylist: (String) -> Unit,
    onOpenArtist: (String) -> Unit,
    onOpenAlbum: (String) -> Unit,
    onOpenStats: () -> Unit,
    onOpenNewReleases: () -> Unit,
) {
    val library by SpotifyCore.library.collectAsState()
    val libraryTree by SpotifyCore.libraryTree.collectAsState()
    val pinnedIds by SpotifyCore.pinnedIds.collectAsState()
    val librarySort by SpotifyCore.librarySort.collectAsState()
    val tracks by SpotifyCore.tracks.collectAsState()
    val recentlyPlayedRanks by SpotifyCore.recentlyPlayedRanks.collectAsState()
    val savedAlbumItems by SpotifyCore.savedAlbumItems.collectAsState()
    val followedArtistItems by SpotifyCore.followedArtistItems.collectAsState()
    var filter by remember { mutableStateOf(LibraryFilter.Playlists) }
    var filterQuery by remember { mutableStateOf("") }
    var filterRevealed by remember { mutableStateOf(false) }
    var collapsedFolders by remember { mutableStateOf(setOf<String>()) }
    val listState = rememberLazyListState()
    val pullConnection = rememberPullToRevealConnection(
        listState = listState,
        revealed = filterRevealed,
        onReveal = { filterRevealed = true },
    )

    LaunchedEffect(Unit) {
        SpotifyCore.loadSavedAlbums()
        SpotifyCore.loadFollowedArtists()
    }

    // The rootlist reads through the local playback session, so it can lag
    // behind (or never arrive, if the user hasn't authorized playback) —
    // fall back to the flat Web API list rather than showing nothing.
    val allNodes = remember(library, libraryTree) {
        val liked = LibraryNode.PlaylistNode(SpotifyCore.LIKED_SONGS_ITEM)
        val rest = libraryTree.ifEmpty { library.map { LibraryNode.PlaylistNode(it) } }
        listOf(liked) + rest
    }

    // Folders only make sense under Spotify's own ordering — any other sort
    // (like filtering) flattens them, same as real Spotify's own A-Z view.
    // Frozen at the moment librarySort is picked, not reactive to tracks
    // afterward — RecentlyUpdated is the only sort that reads tracks, and
    // without this, merely opening a playlist (which caches its tracks)
    // would reshuffle the list on its own, as if viewing it were an update.
    val sortSnapshotTracks = remember(librarySort) { tracks }
    val filteredFlat = remember(allNodes, filterQuery, librarySort, sortSnapshotTracks, recentlyPlayedRanks) {
        when {
            filterQuery.isNotBlank() ->
                allNodes.flattenPlaylists().filter { it.name.contains(filterQuery, ignoreCase = true) }
            librarySort != LibrarySort.Custom ->
                sortLibraryItems(allNodes.flattenPlaylists(), librarySort, sortSnapshotTracks, recentlyPlayedRanks)
            else -> null
        }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            SectionHeader("Your Library", modifier = Modifier.weight(1f))
            if (filter == LibraryFilter.Playlists) {
                SortMenuButton(current = librarySort, onSelect = { SpotifyCore.setLibrarySort(it) })
            }
            IconButton(onClick = onOpenNewReleases) {
                Icon(
                    imageVector = Icons.Filled.NewReleases,
                    contentDescription = "New Releases",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            IconButton(onClick = onOpenStats) {
                Icon(
                    imageVector = Icons.Filled.BarChart,
                    contentDescription = "Your Stats",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            LibraryFilter.entries.forEach { candidate ->
                FilterChip(
                    selected = filter == candidate,
                    onClick = { filter = candidate },
                    label = { Text(candidate.label) },
                )
            }
        }
        when (filter) {
            LibraryFilter.Albums -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(savedAlbumItems, key = { it.id }) { album ->
                    AlbumRow(album = album, onClick = { onOpenAlbum(album.id) })
                }
            }
            LibraryFilter.Artists -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(followedArtistItems, key = { it.id }) { artist ->
                    ArtistRow(artist = artist, onClick = { onOpenArtist(artist.id) })
                }
            }
            LibraryFilter.Playlists -> {
                if (filterRevealed) {
                    FilterBar(
                        query = filterQuery,
                        onQueryChange = { filterQuery = it },
                        onClose = {
                            filterRevealed = false
                            filterQuery = ""
                        },
                        placeholder = "Filter your library",
                    )
                }
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .nestedScroll(pullConnection),
                ) {
                    val flat = filteredFlat
                    if (flat != null && flat.isEmpty() && filterQuery.isNotBlank()) {
                        Text(
                            text = "No playlists match \"$filterQuery\".",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier
                                .align(Alignment.Center)
                                .padding(32.dp),
                        )
                    } else {
                        LazyColumn(modifier = Modifier.fillMaxSize(), state = listState) {
                            if (flat != null) {
                                val ordered = pinLikedSongsFirst(flat.sortedByDescending { pinnedIds.contains(it.id) })
                                items(ordered, key = { it.id }) { item ->
                                    PlaylistRow(
                                        item = item,
                                        onClick = { onOpenPlaylist(item.id) },
                                        pinned = pinnedIds.contains(item.id),
                                        onLongClick = { SpotifyCore.openLibraryMenu(item) },
                                    )
                                }
                            } else {
                                // Liked Songs always belongs in this top section,
                                // pinned or not — effectivePinnedIds is what keeps
                                // it out of the folder tree below regardless.
                                val effectivePinnedIds = pinnedIds + SpotifyCore.LIKED_SONGS_ID
                                val pinned = pinLikedSongsFirst(
                                    allNodes.flattenPlaylists().filter { effectivePinnedIds.contains(it.id) },
                                )
                                items(pinned, key = { "pin:${it.id}" }) { item ->
                                    PlaylistRow(
                                        item = item,
                                        onClick = { onOpenPlaylist(item.id) },
                                        pinned = pinnedIds.contains(item.id),
                                        onLongClick = { SpotifyCore.openLibraryMenu(item) },
                                    )
                                }
                                val unpinned = pruneForDisplay(allNodes, effectivePinnedIds)
                                val rows = flattenForDisplay(unpinned, depth = 0, collapsed = collapsedFolders)
                                items(rows, key = { it.key }) { row ->
                                    when (row) {
                                        is LibraryRow.Playlist -> PlaylistRow(
                                            item = row.item,
                                            onClick = { onOpenPlaylist(row.item.id) },
                                            pinned = false,
                                            onLongClick = { SpotifyCore.openLibraryMenu(row.item) },
                                            modifier = Modifier.padding(start = (row.depth * 20).dp),
                                        )
                                        is LibraryRow.Folder -> FolderRow(
                                            name = row.name,
                                            count = row.count,
                                            expanded = !collapsedFolders.contains(row.id),
                                            onClick = {
                                                collapsedFolders = if (collapsedFolders.contains(row.id)) {
                                                    collapsedFolders - row.id
                                                } else {
                                                    collapsedFolders + row.id
                                                }
                                            },
                                            modifier = Modifier.padding(start = (row.depth * 20).dp),
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** A flattened row for the library [LazyColumn] — folders can't nest
 *  `items()` calls, so the tree is unrolled into a depth-tagged list first,
 *  skipping a collapsed folder's children. */
private sealed interface LibraryRow {
    val key: String

    data class Playlist(val item: LibraryItem, val depth: Int) : LibraryRow {
        override val key get() = "p:${item.id}"
    }

    data class Folder(val id: String, val name: String, val count: Int, val depth: Int) : LibraryRow {
        override val key get() = "f:$id"
    }
}

private fun flattenForDisplay(nodes: List<LibraryNode>, depth: Int, collapsed: Set<String>): List<LibraryRow> =
    nodes.flatMap { node ->
        when (node) {
            is LibraryNode.PlaylistNode -> listOf(LibraryRow.Playlist(node.item, depth))
            is LibraryNode.FolderNode -> {
                val header = LibraryRow.Folder(node.id, node.name, node.children.flattenPlaylists().size, depth)
                if (collapsed.contains(node.id)) {
                    listOf(header)
                } else {
                    listOf(header) + flattenForDisplay(node.children, depth + 1, collapsed)
                }
            }
        }
    }

/** Drops every pinned playlist out of the tree so it doesn't show twice —
 *  once in the pinned section up top, once in its folder. */
private fun pruneForDisplay(nodes: List<LibraryNode>, pinnedIds: Set<String>): List<LibraryNode> =
    nodes.mapNotNull { node ->
        when (node) {
            is LibraryNode.PlaylistNode -> if (pinnedIds.contains(node.item.id)) null else node
            is LibraryNode.FolderNode -> node.copy(children = pruneForDisplay(node.children, pinnedIds))
        }
    }
