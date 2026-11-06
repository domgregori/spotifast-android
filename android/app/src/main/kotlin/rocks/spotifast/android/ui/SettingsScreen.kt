package rocks.spotifast.android.ui

import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.AppIcon
import rocks.spotifast.android.AppTheme
import rocks.spotifast.android.AudioQuality
import rocks.spotifast.android.ListenBrainz
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.currentAppIcon
import rocks.spotifast.android.setAppIcon
import rocks.spotifast.android.ui.components.SectionHeader

/** ListenBrainz tokens are `str(uuid.uuid4())` server-side (confirmed in
 *  listenbrainz-server's `db/user.py`), so the version/variant nibbles are
 *  fixed — matching them precisely avoids false positives on clipboard
 *  text that merely looks UUID-ish. */
private val LISTENBRAINZ_TOKEN_REGEX =
    Regex("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-4[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}")

/** Settings: theme and streaming quality. The theme switches instantly;
 *  librespot bakes the bitrate into the session at startup, so a quality
 *  change takes effect the next time the app is launched instead. */
@Composable
fun SettingsScreen(onClose: () -> Unit) {
    var selected by remember { mutableStateOf(SpotifyCore.audioQuality()) }
    val appTheme by SpotifyCore.appTheme.collectAsState()
    val inkOutlineEnabled by SpotifyCore.inkOutlineEnabled.collectAsState()
    val context = LocalContext.current
    var appIcon by remember { mutableStateOf(currentAppIcon(context)) }
    var lbConnected by remember { mutableStateOf(ListenBrainz.isConnected) }
    var lbUserName by remember { mutableStateOf(ListenBrainz.userName) }
    var lbScrobbleEnabled by remember { mutableStateOf(ListenBrainz.isScrobbleEnabled()) }
    var lbTokenInput by remember { mutableStateOf("") }
    var lbConnecting by remember { mutableStateOf(false) }
    var lbError by remember { mutableStateOf<String?>(null) }
    var justOpenedListenBrainz by remember { mutableStateOf(false) }
    var detectedToken by remember { mutableStateOf<String?>(null) }
    var confirmingLogout by remember { mutableStateOf(false) }

    fun attemptConnect(tokenText: String) {
        lbConnecting = true
        ListenBrainz.connect(tokenText) { result ->
            lbConnecting = false
            result.fold(
                onSuccess = { name ->
                    lbConnected = true
                    lbUserName = name
                    lbScrobbleEnabled = true
                    lbTokenInput = ""
                },
                onFailure = { lbError = it.message },
            )
        }
    }

    // Clipboard reads are only reliable once the window actually has input
    // focus — coming back from the browser, ON_RESUME fires a beat before
    // that, and a clipboard read right then silently returns null. Window
    // focus is the correct "we're really back" signal.
    val view = LocalView.current
    DisposableEffect(view) {
        val listener = android.view.ViewTreeObserver.OnWindowFocusChangeListener { hasFocus ->
            if (hasFocus && justOpenedListenBrainz) {
                justOpenedListenBrainz = false
                if (!lbConnected) {
                    val clipboard =
                        context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                    val clip = clipboard?.primaryClip
                    val text = if (clip != null && clip.itemCount > 0) {
                        clip.getItemAt(0).text?.toString()?.trim()
                    } else {
                        null
                    }
                    detectedToken = text?.let { LISTENBRAINZ_TOKEN_REGEX.find(it)?.value }
                }
            }
        }
        view.viewTreeObserver.addOnWindowFocusChangeListener(listener)
        onDispose { view.viewTreeObserver.removeOnWindowFocusChangeListener(listener) }
    }

    if (confirmingLogout) {
        AlertDialog(
            onDismissRequest = { confirmingLogout = false },
            title = { Text("Log out of Spotify?") },
            text = { Text("You'll need to sign in again through the browser.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmingLogout = false
                    SpotifyCore.signOut()
                }) {
                    Text("Log out")
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingLogout = false }) {
                    Text("Cancel")
                }
            },
        )
    }

    if (detectedToken != null) {
        AlertDialog(
            onDismissRequest = { detectedToken = null },
            title = { Text("ListenBrainz token detected") },
            text = { Text("Save it and connect to ListenBrainz?") },
            confirmButton = {
                TextButton(onClick = {
                    attemptConnect(detectedToken ?: "")
                    detectedToken = null
                }) {
                    Text("Connect")
                }
            },
            dismissButton = {
                TextButton(onClick = { detectedToken = null }) {
                    Text("Not now")
                }
            },
        )
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .verticalScroll(rememberScrollState())
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        Row(modifier = Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onClose) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
            Text(
                text = "Settings",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
            )
        }
        SectionHeader("Theme")
        AppTheme.entries.forEach { theme ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { SpotifyCore.setAppTheme(theme) }
                    .padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = appTheme == theme, onClick = { SpotifyCore.setAppTheme(theme) })
                Text(
                    text = theme.label,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
        }

        if (appTheme == AppTheme.Noir) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { SpotifyCore.setInkOutlineEnabled(!inkOutlineEnabled) }
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = "Ink Outline art",
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                    Text(
                        text = "Posterizes and traces cover art instead of showing it plain. Costs a little per image.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(checked = inkOutlineEnabled, onCheckedChange = { SpotifyCore.setInkOutlineEnabled(it) })
            }
        }

        SectionHeader("App icon")
        AppIcon.entries.forEach { icon ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable {
                        appIcon = icon
                        setAppIcon(context, icon)
                    }
                    .padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(
                    selected = appIcon == icon,
                    onClick = {
                        appIcon = icon
                        setAppIcon(context, icon)
                    },
                )
                Text(
                    text = icon.label,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
        Text(
            text = "The home screen icon may take a moment to update.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )

        SectionHeader("Streaming quality")
        AudioQuality.entries.forEach { quality ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable {
                        selected = quality
                        SpotifyCore.setAudioQuality(quality)
                    }
                    .padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = selected == quality, onClick = {
                    selected = quality
                    SpotifyCore.setAudioQuality(quality)
                })
                Column {
                    Text(
                        text = "${quality.label} (${quality.kbps} kbps)",
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
        }
        Text(
            text = "Takes effect the next time you open the app.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )

        SectionHeader("ListenBrainz")
        if (!lbConnected) {
            Text(
                text = "Scrobble what you play here to your ListenBrainz account. " +
                    "ListenBrainz has no sign-in for apps like this one — get a token from your " +
                    "settings page there and paste it below.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable {
                        justOpenedListenBrainz = true
                        ListenBrainz.openTokenPage(context)
                    }
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Open listenbrainz.org/settings/",
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                OutlinedTextField(
                    value = lbTokenInput,
                    onValueChange = { lbTokenInput = it; lbError = null },
                    placeholder = { Text("Paste your user token") },
                    singleLine = true,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(8.dp))
                Button(
                    onClick = { attemptConnect(lbTokenInput) },
                    enabled = lbTokenInput.isNotBlank() && !lbConnecting,
                ) {
                    Text(if (lbConnecting) "Connecting…" else "Connect")
                }
            }
            if (lbError != null) {
                Text(
                    text = lbError ?: "",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
        } else {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = "Connected as ${lbUserName ?: "?"}",
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                    Text(
                        text = "Scrobble listening history",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = lbScrobbleEnabled,
                    onCheckedChange = {
                        lbScrobbleEnabled = it
                        ListenBrainz.setScrobbleEnabled(it)
                    },
                )
            }
            Text(
                text = "Disconnect",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier
                    .clickable {
                        ListenBrainz.disconnect()
                        lbConnected = false
                        lbUserName = null
                        lbScrobbleEnabled = false
                    }
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }

        SectionHeader("Storage")
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable { SpotifyCore.clearCache() }
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column {
                Text(
                    text = "Clear Cache",
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                Text(
                    text = "Wipes cached playlist tracks, cover art, and downloaded audio, and retries any " +
                        "playlist that previously failed to load. Cached audio is also dropped automatically " +
                        "after 30 days unplayed.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        SectionHeader("Account")
        Text(
            text = "Log out",
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier
                .clickable { confirmingLogout = true }
                .padding(horizontal = 16.dp, vertical = 8.dp),
        )
    }
}
