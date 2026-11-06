package rocks.spotifast.android.ui

import android.content.Intent
import android.net.Uri
import android.text.format.DateUtils
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.RecentListen
import rocks.spotifast.android.ui.listenbrainz.ActivityTab
import rocks.spotifast.android.ui.listenbrainz.FreshReleasesTab
import rocks.spotifast.android.ui.listenbrainz.GeneratorTab
import rocks.spotifast.android.ui.listenbrainz.WeeklyPlaylistsTab

private enum class ListenBrainzTab(val label: String) {
    Recent("Recent listens"),
    Activity("Activity"),
    WeeklyPlaylists("Weekly playlists"),
    Generator("Playlist generator"),
    FreshReleases("Fresh Releases"),
    Profile("Profile"),
}

/** The ListenBrainz tab — its own bottom-nav destination once connected,
 *  so no back button here, same as Home/Search/Your Library. Tabbed from
 *  the start; the chip row scrolls horizontally rather than assuming a
 *  fixed small set. */
@Composable
fun ListenBrainzScreen() {
    var tab by remember { mutableStateOf(ListenBrainzTab.Recent) }
    var listensResult by remember { mutableStateOf<Result<List<RecentListen>>?>(null) }

    LaunchedEffect(Unit) {
        ListenBrainz.recentListens { result -> listensResult = result }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        Row(modifier = Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "ListenBrainz",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.padding(start = 8.dp),
            )
        }
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            ListenBrainzTab.entries.forEach { candidate ->
                FilterChip(
                    selected = tab == candidate,
                    onClick = { tab = candidate },
                    label = { Text(candidate.label) },
                )
            }
        }
        when (tab) {
            ListenBrainzTab.Recent -> RecentListensTab(listensResult)
            ListenBrainzTab.Activity -> ActivityTab()
            ListenBrainzTab.WeeklyPlaylists -> WeeklyPlaylistsTab()
            ListenBrainzTab.Generator -> GeneratorTab()
            ListenBrainzTab.FreshReleases -> FreshReleasesTab()
            ListenBrainzTab.Profile -> ProfileTab()
        }
    }
}

@Composable
private fun ProfileTab() {
    val context = LocalContext.current
    var scrobbleEnabled by remember { mutableStateOf(ListenBrainz.isScrobbleEnabled()) }

    Column(modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
        Text(
            text = "Connected as ${ListenBrainz.userName ?: "?"}",
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Row(
            modifier = Modifier.fillMaxWidth().padding(vertical = 16.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "Scrobble listening history",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Switch(
                checked = scrobbleEnabled,
                onCheckedChange = {
                    scrobbleEnabled = it
                    ListenBrainz.setScrobbleEnabled(it)
                },
            )
        }
        val userName = ListenBrainz.userName
        if (userName != null) {
            Text(
                text = "Open your profile on listenbrainz.org",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .clickable {
                        context.startActivity(
                            Intent(Intent.ACTION_VIEW, Uri.parse("https://listenbrainz.org/user/$userName/")),
                        )
                    }
                    .padding(vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun RecentListensTab(result: Result<List<RecentListen>>?) {
    when {
        result == null -> Box(
            modifier = Modifier.fillMaxWidth().padding(32.dp),
            contentAlignment = Alignment.Center,
        ) {
            CircularProgressIndicator()
        }
        result.isFailure -> Text(
            text = result.exceptionOrNull()?.message ?: "Couldn't load recent listens.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
        )
        else -> {
            val listens = result.getOrDefault(emptyList())
            if (listens.isEmpty()) {
                Text(
                    text = "No listens yet.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
                )
            } else {
                LazyColumn(modifier = Modifier.fillMaxSize()) {
                    items(listens, key = { it.listenedAtMs }) { listen -> RecentListenRow(listen) }
                }
            }
        }
    }
}

@Composable
private fun RecentListenRow(listen: RecentListen) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = listen.trackName,
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            Text(
                text = listen.artistName,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
        Text(
            text = DateUtils.getRelativeTimeSpanString(listen.listenedAtMs).toString(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
