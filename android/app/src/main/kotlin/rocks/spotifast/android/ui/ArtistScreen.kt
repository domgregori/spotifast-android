package rocks.spotifast.android.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.PlayContext
import rocks.spotifast.android.SimilarArtist
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.components.AlbumRow
import rocks.spotifast.android.ui.components.ArtistArtContextMenu
import rocks.spotifast.android.ui.components.ArtistInfoSheet
import rocks.spotifast.android.ui.components.ArtworkPlaceholder
import rocks.spotifast.android.ui.components.SimilarArtistsSheet
import rocks.spotifast.android.ui.components.TrackRow
import rocks.spotifast.android.util.shareUri

/**
 * An artist's profile: a round header image and name, their most popular
 * tracks, then their albums and singles.
 */
@Composable
fun ArtistScreen(
    artistId: String,
    onBack: () -> Unit,
    onOpenAlbum: (String) -> Unit,
) {
    val context = LocalContext.current
    val artists by SpotifyCore.artists.collectAsState()
    val artistDetails by SpotifyCore.artistDetails.collectAsState()
    val artistTracks by SpotifyCore.artistTracks.collectAsState()
    val artistAlbums by SpotifyCore.artistAlbums.collectAsState()
    val likedTracks by SpotifyCore.likedTracks.collectAsState()
    val artist = artists[artistId]
    val tracks = artistTracks[artistId] ?: emptyList()
    val albums = artistAlbums[artistId] ?: emptyList()
    val artistUri = "spotify:artist:$artistId"
    val following = likedTracks[artistUri] == true
    var infoOpen by remember { mutableStateOf(false) }
    var menuOpen by remember { mutableStateOf(false) }
    var similarArtistsOpen by remember { mutableStateOf(false) }
    var similarArtistsResult by remember { mutableStateOf<Result<List<SimilarArtist>>?>(null) }
    var resolvingArtistMbid by remember { mutableStateOf<String?>(null) }
    var unresolvableArtistMbids by remember { mutableStateOf<Set<String>>(emptySet()) }

    LaunchedEffect(tracks) { SpotifyCore.checkLikedBatch(tracks.map { it.uri }) }
    LaunchedEffect(artistId) { SpotifyCore.checkLiked(artistUri) }

    LazyColumn(modifier = Modifier.fillMaxSize()) {
        item {
            ArtistHeader(
                artistId = artistId,
                name = artist?.name ?: "Artist",
                artUrl = artist?.artUrl,
                following = following,
                onBack = onBack,
                onOpenMenu = { menuOpen = true },
                onToggleFollow = { SpotifyCore.setLiked(artistUri, !following) },
            )
        }
        if (tracks.isNotEmpty()) {
            item {
                SectionTitle("Popular")
            }
        }
        itemsIndexed(tracks, key = { _, track -> "t:${track.uri}" }) { index, track ->
            TrackRow(
                track = track,
                onClick = {
                    SpotifyCore.playTracks(
                        tracks,
                        index,
                        PlayContext.Artist(artistId, artist?.name ?: "Artist", artist?.artUrl),
                    )
                },
                onLongClick = { SpotifyCore.openTrackMenu(track) },
            )
        }
        if (albums.isNotEmpty()) {
            item {
                SectionTitle("Albums")
            }
        }
        itemsIndexed(albums, key = { _, album -> "a:${album.id}" }) { _, album ->
            AlbumRow(album = album, onClick = { onOpenAlbum(album.id) })
        }
    }
    if (infoOpen && artist != null) {
        ArtistInfoSheet(
            artist = artist,
            details = artistDetails[artistId],
            onDismiss = { infoOpen = false },
        )
    }
    if (menuOpen) {
        ArtistArtContextMenu(
            name = artist?.name ?: "Artist",
            following = following,
            listenBrainzConnected = ListenBrainz.isConnected,
            onDismiss = { menuOpen = false },
            onViewInfo = { infoOpen = true },
            onToggleFollow = { SpotifyCore.setLiked(artistUri, !following) },
            onGoToRadio = { SpotifyCore.goToRadio(artistUri, "${artist?.name ?: "Artist"} Radio") },
            onSimilarArtists = {
                similarArtistsOpen = true
                similarArtistsResult = null
                ListenBrainz.similarArtists(artistId) { result -> similarArtistsResult = result }
            },
            onLbRadio = {
                ListenBrainz.mbidForSpotifyArtistAsync(artistId) { mbid ->
                    ListenBrainz.openGeneratedPlaylist { onResult ->
                        if (mbid != null) {
                            ListenBrainz.lbRadio("artist:($mbid)", mode = "medium", onResult = onResult)
                        } else {
                            onResult(
                                Result.failure(
                                    IllegalStateException(
                                        "This artist isn't linked on MusicBrainz, so LB Radio has nothing to generate from.",
                                    ),
                                ),
                            )
                        }
                    }
                }
            },
            onShare = { shareUri(context, artistUri, artist?.name ?: "Artist") },
        )
    }
    if (similarArtistsOpen) {
        SimilarArtistsSheet(
            artistName = artist?.name ?: "Artist",
            result = similarArtistsResult,
            resolving = resolvingArtistMbid,
            unresolvable = unresolvableArtistMbids,
            onSelect = { similar ->
                resolvingArtistMbid = similar.mbid
                ListenBrainz.resolveSpotifyArtistId(similar.mbid) { spotifyId ->
                    resolvingArtistMbid = null
                    if (spotifyId != null) {
                        similarArtistsOpen = false
                        SpotifyCore.openArtist(spotifyId)
                    } else {
                        unresolvableArtistMbids = unresolvableArtistMbids + similar.mbid
                    }
                }
            },
            onDismiss = { similarArtistsOpen = false },
        )
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.titleMedium,
        fontWeight = FontWeight.Bold,
        color = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
    )
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ArtistHeader(
    artistId: String,
    name: String,
    artUrl: String?,
    following: Boolean,
    onBack: () -> Unit,
    onOpenMenu: () -> Unit,
    onToggleFollow: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceContainer),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(4.dp)) {
            IconButton(onClick = onBack) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 24.dp, vertical = 8.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            Column {
                // Tapping no longer opens the info sheet — that, plus
                // follow and radio, live in the long-press menu now;
                // tapping the picture itself does nothing.
                ArtworkPlaceholder(
                    key = artistId,
                    label = name,
                    modifier = Modifier.size(160.dp).combinedClickable(onClick = {}, onLongClick = onOpenMenu),
                    corner = 80.dp,
                    artUrl = artUrl,
                )
                Spacer(Modifier.height(16.dp))
                Text(
                    text = name,
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.ExtraBold,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                Spacer(Modifier.height(12.dp))
                OutlinedButton(onClick = onToggleFollow) {
                    Text(if (following) "Following" else "Follow")
                }
                Spacer(Modifier.height(4.dp))
            }
        }
    }
}
