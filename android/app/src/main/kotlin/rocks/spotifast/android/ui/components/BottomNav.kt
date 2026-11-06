package rocks.spotifast.android.ui.components

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.LibraryMusic
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.LibraryMusic
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.R
import rocks.spotifast.android.ui.AppTab

/** Spotify's three-tab bottom bar — Home, Search, Your Library — plus a
 *  fourth ListenBrainz tab once connected. */
@Composable
fun SpotifastBottomBar(selected: AppTab, onSelect: (AppTab) -> Unit, listenBrainzConnected: Boolean) {
    NavigationBar(
        containerColor = MaterialTheme.colorScheme.surfaceContainer,
        tonalElevation = 0.dp,
    ) {
        NavigationBarItem(
            selected = selected == AppTab.Home,
            onClick = { onSelect(AppTab.Home) },
            icon = {
                Icon(
                    imageVector = if (selected == AppTab.Home) Icons.Filled.Home else Icons.Outlined.Home,
                    contentDescription = "Home",
                )
            },
            label = { Text("Home") },
            colors = spotifyNavColors(),
        )
        NavigationBarItem(
            selected = selected == AppTab.Search,
            onClick = { onSelect(AppTab.Search) },
            icon = {
                Icon(
                    imageVector = if (selected == AppTab.Search) Icons.Filled.Search else Icons.Outlined.Search,
                    contentDescription = "Search",
                )
            },
            label = { Text("Search") },
            colors = spotifyNavColors(),
        )
        NavigationBarItem(
            selected = selected == AppTab.Library,
            onClick = { onSelect(AppTab.Library) },
            icon = {
                Icon(
                    imageVector = if (selected == AppTab.Library) {
                        Icons.Filled.LibraryMusic
                    } else {
                        Icons.Outlined.LibraryMusic
                    },
                    contentDescription = "Your Library",
                )
            },
            label = { Text("Your Library") },
            colors = spotifyNavColors(),
        )
        if (listenBrainzConnected) {
            NavigationBarItem(
                selected = selected == AppTab.ListenBrainz,
                onClick = { onSelect(AppTab.ListenBrainz) },
                icon = {
                    Icon(
                        painter = painterResource(R.drawable.ic_listenbrainz),
                        contentDescription = "ListenBrainz",
                    )
                },
                label = { Text("ListenBrainz") },
                colors = spotifyNavColors(),
            )
        }
    }
}

@Composable
private fun spotifyNavColors() = NavigationBarItemDefaults.colors(
    selectedIconColor = MaterialTheme.colorScheme.onSurface,
    selectedTextColor = MaterialTheme.colorScheme.onSurface,
    unselectedIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
    unselectedTextColor = MaterialTheme.colorScheme.onSurfaceVariant,
    indicatorColor = MaterialTheme.colorScheme.surfaceContainer,
)
