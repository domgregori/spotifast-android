package rocks.spotifast.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import coil3.request.ImageRequest
import coil3.request.transformations
import kotlin.math.abs
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.LocalIsNoir

/**
 * A playlist/track's artwork tile. When [artUrl] is available (the Web
 * API's own cover art, or librespot's for the currently-playing track) it's
 * loaded over a gradient base so a slow/failed load still shows a filled
 * tile rather than empty space. Without one, the gradient — deterministic
 * per [key], so the same item always renders the same color — carries the
 * item's initial letter instead, the same fallback Spotify's own app uses
 * before art loads.
 */
@Composable
fun ArtworkPlaceholder(
    key: String,
    label: String,
    modifier: Modifier = Modifier,
    corner: Dp = 8.dp,
    artUrl: String? = null,
    /** Suppressed when a caller draws its own border around this art plus
     *  something else below it (the Noir grid tile's art+label card) — two
     *  borders meeting at that seam would double up instead of reading as
     *  one connected card. */
    drawBorder: Boolean = true,
) {
    val isLikedSongs = key == SpotifyCore.LIKED_SONGS_ID
    val brush = if (isLikedSongs) likedSongsGradient() else rememberArtworkGradient(key)
    val noir = LocalIsNoir.current
    val inkOutlineEnabled by SpotifyCore.inkOutlineEnabled.collectAsState()
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(corner))
            .background(brush)
            .let {
                if (noir && drawBorder) {
                    it.border(2.5.dp, MaterialTheme.colorScheme.onSurface, RoundedCornerShape(corner))
                } else {
                    it
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        if (artUrl != null) {
            val context = LocalContext.current
            val model = if (noir && inkOutlineEnabled) {
                ImageRequest.Builder(context)
                    .data(artUrl)
                    .transformations(listOf(InkOutlineTransformation()))
                    .build()
            } else {
                artUrl
            }
            AsyncImage(
                model = model,
                contentDescription = null,
                modifier = Modifier.matchParentSize(),
                contentScale = ContentScale.Crop,
            )
        } else if (isLikedSongs) {
            // Spotify's own Liked Songs icon is a fixed gradient-plus-heart
            // glyph, not a per-item placeholder — recognizable at a glance
            // rather than looking like any other unloaded cover.
            Icon(
                imageVector = Icons.Filled.Favorite,
                contentDescription = null,
                tint = Color.White.copy(alpha = 0.95f),
                modifier = Modifier.fillMaxSize(0.5f),
            )
        } else {
            val initial = label.trim().firstOrNull()?.uppercaseChar()?.toString() ?: "?"
            Text(
                text = initial,
                color = Color.White.copy(alpha = 0.92f),
                style = MaterialTheme.typography.titleLarge,
                fontSize = 22.sp,
            )
        }
    }
}

/** The fixed two-tone gradient behind the Liked Songs heart — the app's own
 *  accent-to-tertiary roles, so it reads as "this app's brand" in both
 *  Spotify and Noir mode rather than carrying one hardcoded color pair that
 *  would clash with Noir's palette. */
@Composable
private fun likedSongsGradient(): Brush {
    val start = MaterialTheme.colorScheme.primary
    val end = MaterialTheme.colorScheme.tertiary
    return Brush.linearGradient(
        colors = listOf(start, end),
        start = Offset(0f, 0f),
        end = Offset(1f, 1f),
    )
}

/** A deterministic two-tone diagonal gradient, hashed from [key]. */
@Composable
fun rememberArtworkGradient(key: String): Brush = remember(key) {
    val hue = (abs(key.hashCode()) % 360).toFloat()
    val start = Color.hsv(hue, 0.55f, 0.55f)
    val end = Color.hsv((hue + 48f) % 360f, 0.65f, 0.30f)
    Brush.linearGradient(
        colors = listOf(start, end),
        start = Offset(0f, 0f),
        end = Offset(1f, 1f),
    )
}
