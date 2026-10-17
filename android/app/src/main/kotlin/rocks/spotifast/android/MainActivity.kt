package rocks.spotifast.android

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        SpotifyCore.init(this)
        setContent {
            MaterialTheme {
                App()
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intent.data?.getQueryParameter("code")?.let { code ->
            SpotifyCore.completeRedirect(code)
        }
    }
}

@Composable
private fun App() {
    val authState by SpotifyCore.authState.collectAsState()

    when (val state = authState) {
        is AuthState.WaitingForBrowser -> BrowserRedirect(state.url)
        is AuthState.Connecting -> Connecting()
        is AuthState.Connected -> Library(state.username)
        else -> Login()
    }
}

/** Opens the Spotify authorize URL in a Custom Tab, then waits for the code. */
@Composable
private fun BrowserRedirect(url: String) {
    val context = LocalContext.current
    LaunchedEffect(url) {
        CustomTabsIntent.Builder().build().launchUrl(context, Uri.parse(url))
    }
    Centered { CircularProgressIndicator() }
}

@Composable
private fun Connecting() {
    Centered { CircularProgressIndicator() }
}

@Composable
private fun Login() {
    Centered {
        Text("Spotifast", style = MaterialTheme.typography.headlineMedium)
        Spacer(Modifier.height(16.dp))
        Button(onClick = { SpotifyCore.signIn() }) {
            Text("Sign in")
        }
    }
}

@Composable
private fun Library(username: String) {
    val library by SpotifyCore.library.collectAsState()
    LaunchedEffect(Unit) { SpotifyCore.library() }

    Scaffold { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(16.dp),
        ) {
            Text("Signed in as $username", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(16.dp))
            if (library.isEmpty()) {
                Text("Your library is empty.")
            } else {
                LazyColumn {
                    items(library, key = { it.id }) { playlist ->
                        Text(
                            text = playlist.name,
                            style = MaterialTheme.typography.bodyLarge,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 8.dp),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun Centered(content: @Composable () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        content()
    }
}
