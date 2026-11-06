package rocks.spotifast.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import rocks.spotifast.android.GeneratedTrack
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.TrackItem
import rocks.spotifast.android.ui.components.PlaylistNameDialog

/** The one results page every ListenBrainz-generated/fetched playlist
 *  funnels into — Weekly playlists, the artist context menu's "LB Radio"
 *  entry, and the Generator tab. A full page rather than a bottom sheet —
 *  a 50-track list with a progress line and an action row genuinely needs
 *  the room, and sheets made the action row hard to reach. Each track's
 *  recording MBID gets matched back to a Spotify track id lazily, in the
 *  background, via [ListenBrainz.matchToSpotify] — "Play" and "Save to
 *  Spotify" stay disabled until that finishes; "Save to ListenBrainz"
 *  doesn't need it at all, since ListenBrainz playlists are MBID-native. */
@Composable
fun GeneratedPlaylistScreen() {
    val open by ListenBrainz.generatedPlaylistOpen.collectAsState()
    if (!open) return
    val result by ListenBrainz.generatedPlaylistResult.collectAsState()
    var matchedTracks by remember(result) { mutableStateOf<List<GeneratedTrack>?>(null) }
    var matchProgress by remember(result) { mutableStateOf(0 to 0) }
    var namingForSpotify by remember { mutableStateOf(false) }
    var namingForListenBrainz by remember { mutableStateOf(false) }
    var saveStatus by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(result) {
        val playlist = result?.getOrNull() ?: return@LaunchedEffect
        ListenBrainz.matchToSpotify(
            playlist.tracks,
            onProgress = { matched, total -> matchProgress = matched to total },
            onResult = { matched -> matchedTracks = matched },
        )
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        Row(modifier = Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = { ListenBrainz.closeGeneratedPlaylist() }) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
            Text(
                text = "Playlist",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
            )
        }
        when {
            result == null -> Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
            result?.isFailure == true -> Text(
                text = result?.exceptionOrNull()?.message ?: "Couldn't load this playlist.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp),
            )
            else -> {
                val playlist = result!!.getOrThrow()
                val displayTracks = matchedTracks ?: playlist.tracks
                LazyColumn(modifier = Modifier.fillMaxSize()) {
                    item {
                        Text(
                            text = playlist.title,
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.onSurface,
                            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
                        )
                    }
                    item {
                        Text(
                            text = matchProgress.let { (matched, total) ->
                                if (matchedTracks != null) {
                                    "${playlist.tracks.size} tracks — from ListenBrainz"
                                } else {
                                    "Matching $matched/$total tracks to Spotify…"
                                }
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 24.dp, end = 24.dp, bottom = 8.dp),
                        )
                    }
                    items(displayTracks.size, key = { it }) { index ->
                        GeneratedTrackRow(track = displayTracks[index], resolved = matchedTracks != null)
                    }
                    item {
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                            horizontalArrangement = Arrangement.SpaceEvenly,
                        ) {
                            TextButton(
                                enabled = matchedTracks != null,
                                onClick = {
                                    val tracks = matchedTracks.orEmpty().mapNotNull { track ->
                                        val uri = track.spotifyUri ?: return@mapNotNull null
                                        TrackItem(
                                            uri = uri,
                                            name = track.title,
                                            artist = track.artist,
                                            durationMs = track.durationMs,
                                            artUrl = null,
                                        )
                                    }
                                    if (tracks.isNotEmpty()) {
                                        SpotifyCore.playTracks(tracks, 0)
                                        ListenBrainz.closeGeneratedPlaylist()
                                    }
                                },
                            ) {
                                Text("Play")
                            }
                            TextButton(enabled = matchedTracks != null, onClick = { namingForSpotify = true }) {
                                Text("Save to Spotify")
                            }
                            TextButton(onClick = { namingForListenBrainz = true }) {
                                Text("Save to ListenBrainz")
                            }
                        }
                    }
                    val status = saveStatus
                    if (status != null) {
                        item {
                            Text(
                                text = status,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
                            )
                        }
                    }
                }
                if (namingForSpotify) {
                    PlaylistNameDialog(
                        initialName = playlist.title,
                        confirmLabel = "Save",
                        onConfirm = { name ->
                            val uris = matchedTracks.orEmpty().mapNotNull { it.spotifyUri }
                            SpotifyCore.createPlaylist(name, uris)
                            namingForSpotify = false
                        },
                        onDismiss = { namingForSpotify = false },
                    )
                }
                if (namingForListenBrainz) {
                    PlaylistNameDialog(
                        initialName = playlist.title,
                        confirmLabel = "Save",
                        onConfirm = { name ->
                            ListenBrainz.savePlaylistToListenBrainz(playlist, name) { result ->
                                saveStatus = result.fold(
                                    onSuccess = { "Saved \"$name\" to ListenBrainz" },
                                    onFailure = { "Couldn't save to ListenBrainz: ${it.message}" },
                                )
                            }
                            namingForListenBrainz = false
                        },
                        onDismiss = { namingForListenBrainz = false },
                    )
                }
            }
        }
    }
}

@Composable
private fun GeneratedTrackRow(track: GeneratedTrack, resolved: Boolean) {
    val unmatched = resolved && track.spotifyUri == null
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            imageVector = if (unmatched) Icons.Filled.Block else Icons.Filled.MusicNote,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.width(16.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = track.title,
                style = MaterialTheme.typography.bodyLarge,
                color = if (unmatched) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            Text(
                text = track.artist,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
        if (unmatched) {
            Text(
                text = "not on Spotify",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
