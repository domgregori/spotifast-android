package rocks.spotifast.android.ui.components

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
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.DeviceItem
import rocks.spotifast.android.SpotifyCore

/** The "Connect to a device" sheet: every Spotify Connect device on the
 *  account, tapping one transfers playback to it (librespot's own receiver
 *  is what lets this device show up on everyone else's list too). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesSheet(onDismiss: () -> Unit) {
    val devices by SpotifyCore.devices.collectAsState()
    LaunchedEffect(Unit) { SpotifyCore.loadDevices() }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = "Connect to a device",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
            )
            if (devices.isEmpty()) {
                Text(
                    text = "No devices found.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp),
                )
            }
            devices.forEach { device ->
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
                        tint = if (device.isActive) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.width(20.dp))
                    Column {
                        Text(
                            text = device.name,
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                        Text(
                            text = if (device.isActive) "Active device" else device.kind,
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
                    DeviceVolumeSlider(device = device, modifier = Modifier.padding(horizontal = 24.dp))
                }
            }
        }
    }
}

/** The active Connect device's volume — the phone's own volume
 *  controls/slider only ever reach local playback, so a remote device
 *  needs this to be adjustable from here at all. Dragging updates the
 *  displayed value immediately; the actual `set_device_volume` call only
 *  fires on release, so a drag doesn't flood Spotify's API with one
 *  request per frame. */
@Composable
private fun DeviceVolumeSlider(device: DeviceItem, modifier: Modifier = Modifier) {
    var dragging by remember(device.id) { mutableFloatStateOf(-1f) }
    val value = if (dragging >= 0f) dragging else (device.volumePercent ?: 0).toFloat()
    Row(modifier = modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Icon(
            imageVector = Icons.AutoMirrored.Filled.VolumeUp,
            contentDescription = "Volume",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.width(12.dp))
        Slider(
            value = value,
            onValueChange = { dragging = it },
            onValueChangeFinished = {
                SpotifyCore.setDeviceVolume(device.id, dragging.toInt())
                dragging = -1f
            },
            valueRange = 0f..100f,
            modifier = Modifier.weight(1f),
        )
    }
}
