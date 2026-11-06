package rocks.spotifast.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import java.time.LocalTime
import rocks.spotifast.android.RecentEntry
import rocks.spotifast.android.RecentKind
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.RecentCardTile

/**
 * Spotify's Home tab: a time-of-day greeting followed by a feed of what's
 * actually been played recently — playlists (including ones played on
 * other devices, via the Web API's own play history), plus artists and
 * albums played from their own screens in this app. Nothing here is a
 * static library listing; an item earns its place by being played, not
 * just viewed. See [SpotifyCore.recentlyPlayedFeed].
 */
@Composable
fun HomeScreen(
    username: String,
    onOpenPlaylist: (String) -> Unit,
    onOpenArtist: (String) -> Unit,
    onOpenAlbum: (String) -> Unit,
    onOpenSettings: () -> Unit,
) {
    val feed by SpotifyCore.recentlyPlayedFeed.collectAsState()

    LazyVerticalGrid(
        columns = GridCells.Fixed(2),
        contentPadding = PaddingValues(16.dp),
        modifier = Modifier.fillMaxSize(),
    ) {
        item(span = { GridItemSpan(2) }) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(bottom = 16.dp, top = 8.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = greeting(username),
                    style = MaterialTheme.typography.headlineMedium,
                    color = MaterialTheme.colorScheme.onBackground,
                )
                IconButton(onClick = onOpenSettings) {
                    Icon(
                        imageVector = Icons.Filled.Settings,
                        contentDescription = "Settings",
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        if (feed.isEmpty()) {
            item(span = { GridItemSpan(2) }) {
                Text(
                    text = "Tracks you play will show up here.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(vertical = 32.dp),
                )
            }
        }
        items(feed, key = { "${it.kind}:${it.id}" }) { entry: RecentEntry ->
            RecentCardTile(
                id = entry.id,
                title = entry.name,
                tag = entry.kind.name.uppercase(),
                subtitle = entry.subtitle,
                artUrl = entry.artUrl,
                onClick = {
                    when (entry.kind) {
                        RecentKind.Playlist -> onOpenPlaylist(entry.id)
                        RecentKind.Artist -> onOpenArtist(entry.id)
                        RecentKind.Album -> onOpenAlbum(entry.id)
                    }
                },
                onLongClick = { SpotifyCore.openRecentMenu(entry) },
            )
        }
    }
}

private fun greeting(username: String): String {
    val hour = LocalTime.now().hour
    val time = when {
        hour < 12 -> "Good morning"
        hour < 18 -> "Good afternoon"
        else -> "Good evening"
    }
    return "$time, $username"
}
