package rocks.spotifast.android.ui.components

import android.content.Context
import android.media.MediaRouter
import android.media.MediaRoute2Info
import android.media.MediaRouter2
import android.os.Build
import android.util.Log
import androidx.annotation.RequiresApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

/** One of this phone's own audio outputs: its speaker, wired headphones, a
 *  Bluetooth headset, USB audio. [handle] is the platform's own route object,
 *  kept so selecting one needs no second lookup. */
data class AudioOutput(val id: String, val name: String, val selected: Boolean, val handle: Any)

/**
 * Where this phone's sound comes out, and how to move it.
 *
 * This is deliberately separate from [DevicesSheet], which hands playback to
 * another Spotify device. Here playback stays on this phone and only its
 * output changes, which belongs to Android's media routing rather than
 * Spotify's: the app has always played to the system's default output, so the
 * picker just asks the system for a different one, and the running stream
 * follows without being rebuilt (verified on the Pixel 9 Pro by pulling the
 * Bluetooth earbuds mid-track).
 *
 * Android 11 and newer answer through `MediaRouter2`, whose system routes are
 * exactly these outputs. Older versions only expose the first-generation
 * router, which is used as a fallback.
 */
@RequiresApi(Build.VERSION_CODES.R)
private fun currentOutputs(context: Context): List<AudioOutput> {
    val router = MediaRouter2.getInstance(context)
    val controller = router.systemController
    val selected = controller.selectedRoutes.map { it.id }.toSet()
    Log.d("SpotifastOutputs", "v2 routes: " + router.routes.joinToString { "${it.name}/${it.type}/system=${it.isSystemRoute}" })
    return router.routes
        .filter { it.isSystemRoute }
        .distinctBy { it.id }
        .map { AudioOutput(it.id, it.name.toString(), it.id in selected, it) }
}

@Suppress("DEPRECATION")
private fun legacyOutputs(context: Context): List<AudioOutput> {
    val router =
        context.getSystemService(Context.MEDIA_ROUTER_SERVICE) as MediaRouter
    val current = router.getSelectedRoute(MediaRouter.ROUTE_TYPE_LIVE_AUDIO)
    Log.d("SpotifastOutputs", "v1 routes: " + (0 until router.routeCount).joinToString { router.getRouteAt(it)?.name.toString() })
    return (0 until router.routeCount)
        .mapNotNull { index ->
            val route = router.getRouteAt(index) ?: return@mapNotNull null
            val local = route.playbackType == MediaRouter.RouteInfo.PLAYBACK_TYPE_LOCAL
            if (!local || !route.isEnabled) return@mapNotNull null
            AudioOutput(route.toString(), route.name.toString(), route === current, route)
        }
        .distinctBy { it.id }
}

private fun outputs(context: Context): List<AudioOutput> {
    val legacy = legacyOutputs(context)
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return legacy
    // The modern list is preferred, but an empty one is not the truth on
    // every device: fall back to the older router rather than say there are
    // no outputs at all.
    val current = currentOutputs(context)
    return current.ifEmpty { legacy }
}

@Suppress("DEPRECATION")
private fun select(context: Context, output: AudioOutput) {
    when (val handle = output.handle) {
        is MediaRoute2Info ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                MediaRouter2.getInstance(context).systemController.selectRoute(handle)
            } else {
                Unit
            }
        // Deprecated on Android 11 and newer but still the only route object
        // on the versions below it.
        is MediaRouter.RouteInfo -> {
            val router =
                context.getSystemService(Context.MEDIA_ROUTER_SERVICE) as MediaRouter
            router.selectRoute(MediaRouter.ROUTE_TYPE_LIVE_AUDIO, handle)
        }
    }
}

/** The output picker, laid out like the Connect sheet it sits beside. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun OutputRouteSheet(onDismiss: () -> Unit) {
    val context = LocalContext.current
    // Read when the sheet opens. A device plugged in while it is open shows up
    // the next time it is opened, which is enough for a picker this size.
    val available = remember { outputs(context) }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = "Play on this phone",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            Text(
                text = "Choose this phone's speaker, headphones or a connected device. To move " +
                    "playback to another Spotify device, use Connect instead.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp),
            )
            if (available.isEmpty()) {
                Text(
                    text = "This phone reports no audio outputs.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp),
                )
            }
            available.forEach { output ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(enabled = !output.selected) {
                            select(context, output)
                            onDismiss()
                        }
                        .padding(horizontal = 24.dp, vertical = 16.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        imageVector = if (output.selected) {
                            Icons.Filled.Check
                        } else {
                            Icons.AutoMirrored.Filled.VolumeUp
                        },
                        contentDescription = null,
                        tint = if (output.selected) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                    Spacer(Modifier.width(20.dp))
                    Column {
                        Text(
                            text = output.name,
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                        Text(
                            text = if (output.selected) "Playing here" else "This phone",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (output.selected) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
                }
            }
        }
    }
}
