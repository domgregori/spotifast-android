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
import androidx.compose.material.icons.filled.Cast
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
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
import rocks.spotifast.android.DeviceItem
import rocks.spotifast.android.SpotifyCore

/** One of this phone's own audio outputs: its speaker, wired headphones, a
 *  Bluetooth headset, USB audio. [handle] is the platform's own route object,
 *  kept so selecting one needs no second lookup. */
data class AudioOutput(val id: String, val name: String, val selected: Boolean, val handle: Any)

/**
 * Everything playback can be sent to, in one place: this phone's own outputs
 * first, then the account's Spotify Connect devices.
 *
 * The two are different problems that look alike. A row under "This phone"
 * changes Android's media route, so the sound keeps coming from this device
 * and the running stream follows without being rebuilt. A row under "Spotify
 * Connect" moves playback to another device entirely, through librespot's
 * Connect receiver.
 *
 * Android 11 and newer answer the local list through `MediaRouter2`, whose
 * system routes are exactly these outputs, but some devices return nothing
 * there, so the first-generation router is used whenever that list is empty.
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

/** The one playback picker: this phone's outputs and the Connect devices. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PlaybackSheet(onDismiss: () -> Unit) {
    val context = LocalContext.current
    // Read when the sheet opens. A device plugged in while it is open shows up
    // the next time it is opened, which is enough for a picker this size.
    val available = remember { outputs(context) }
    val devices by SpotifyCore.devices.collectAsState()
    LaunchedEffect(Unit) { SpotifyCore.loadDevices() }
    // Whether playback is on this device at all. The output rows describe the
    // route this phone would use, which stays set even while playback is
    // happening on another device, so they must not claim "playing here" then.
    val active = devices.firstOrNull { it.isActive }
    val onThisPhone = active == null || active.name == SpotifyCore.localDeviceName

    /** This phone's own Connect device, so choosing a local output can bring
     *  playback back here first when it is currently somewhere else. */
    fun playHere(output: AudioOutput) {
        val own = SpotifyCore.devices.value.firstOrNull { it.name == SpotifyCore.localDeviceName }
        val active = SpotifyCore.devices.value.firstOrNull { it.isActive }
        if (own != null && active?.id != own.id) SpotifyCore.transferPlayback(own.id)
        select(context, output)
    }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = "Playback",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            Text(
                text = "This phone keeps playing here and changes its speaker or headphones. " +
                    "The devices below move playback to another Spotify device.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp),
            )
            Text(
                text = "This phone",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 24.dp, end = 24.dp, top = 16.dp, bottom = 4.dp),
            )
            if (available.isEmpty()) {
                Text(
                    text = "This phone reports no audio outputs.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
                )
            }
            available.forEach { output ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(enabled = !onThisPhone || !output.selected) {
                            playHere(output)
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
                        tint = if (output.selected && onThisPhone) {
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
                            text = when {
                                !onThisPhone -> "Move playback here"
                                output.selected -> "Playing here"
                                else -> "Switch to this output"
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = if (output.selected && onThisPhone) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
                }
            }
            Text(
                text = "Spotify Connect",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 24.dp, end = 24.dp, top = 20.dp, bottom = 4.dp),
            )
            if (devices.isEmpty()) {
                Text(
                    text = "No other devices found.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
                )
            }
            devices.forEach { device: DeviceItem ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(enabled = !device.isActive) {
                            SpotifyCore.transferPlayback(device.id)
                            onDismiss()
                        }
                        .padding(horizontal = 24.dp, vertical = 16.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        imageVector = Icons.Filled.Cast,
                        contentDescription = null,
                        tint = if (device.isActive) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                    Spacer(Modifier.width(20.dp))
                    Column {
                        Text(
                            text = device.name,
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                        Text(
                            text = if (device.isActive) "Playing on this device" else device.kind,
                            style = MaterialTheme.typography.bodySmall,
                            color = if (device.isActive) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
                }
                if (device.isActive && device.supportsVolume) {
                    DeviceVolumeSlider(
                        device = device,
                        modifier = Modifier.padding(horizontal = 24.dp),
                    )
                }
            }
        }
    }
}
