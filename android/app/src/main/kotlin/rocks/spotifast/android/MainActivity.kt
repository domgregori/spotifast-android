package rocks.spotifast.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import rocks.spotifast.android.ui.SpotifastApp
import rocks.spotifast.android.ui.SpotifastTheme
import rocks.spotifast.android.ui.ThemeChoice

class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        SpotifyCore.init(this)
        handleLink(intent)
        setContent {
            val appTheme by SpotifyCore.appTheme.collectAsState()
            val themeChoice = if (appTheme == AppTheme.Noir) ThemeChoice.Noir else ThemeChoice.Spotify
            SpotifastTheme(theme = themeChoice) {
                SpotifastApp()
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleLink(intent)
    }

    /**
     * The authorization callback and shared links both arrive as a VIEW
     * intent: the first is spotifast://callback?code=..., the rest go to
     * [SpotifyCore.receiveDeepLink] and open once the app is connected.
     */
    private fun handleLink(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme == DeepLink.SCHEME && uri.host == "callback") {
            uri.getQueryParameter("code")?.let { SpotifyCore.completeRedirect(it) }
            return
        }
        SpotifyCore.receiveDeepLink(uri.toString())
    }
}
