package rocks.spotifast.android.service

import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import rocks.spotifast.android.AppTheme
import rocks.spotifast.android.SpotifyCore
import rocks.spotifast.android.ui.DreamNowPlayingScreen
import rocks.spotifast.android.ui.SpotifastTheme
import rocks.spotifast.android.ui.ThemeChoice

/**
 * The system's "Screen saver" (Settings' name for what the platform API
 * still calls a Daydream) — a landscape ambient now-playing display,
 * offered while the device is docked/charging and idle. Purely a view
 * onto [SpotifyCore]'s already-running state; this does not itself start
 * or own playback the way [PlaybackService] does, so it's safe for the
 * system to tear down and recreate freely.
 */
class PlaybackDreamService : DreamServiceCompat() {

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        SpotifyCore.init(applicationContext)
        isInteractive = false
        isFullscreen = true
        isScreenBright = true
        setContent {
            val appTheme by SpotifyCore.appTheme.collectAsState()
            val themeChoice = if (appTheme == AppTheme.Noir) ThemeChoice.Noir else ThemeChoice.Spotify
            SpotifastTheme(theme = themeChoice) {
                DreamNowPlayingScreen()
            }
        }
    }
}
