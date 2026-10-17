package rocks.spotifast.android

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import uniffi.spotifast.CoreHandle
import uniffi.spotifast.PlaybackStateListener

/** The sign-in state shown by the UI, mirrored from the core's auth events. */
sealed interface AuthState {
    data object SignedOut : AuthState
    data class WaitingForBrowser(val url: String) : AuthState
    data object Connecting : AuthState
    data class Connected(val username: String) : AuthState
    data class Failed(val message: String) : AuthState
}

/** One playlist in the library list. */
data class LibraryItem(val id: String, val name: String)

/**
 * Process-wide holder for the single `CoreHandle`. The Activity binds to this
 * (Phase C moves it into a foreground `Service`); Compose collects the state
 * flows.
 */
object SpotifyCore {

    private val _authState = MutableStateFlow<AuthState>(AuthState.SignedOut)
    val authState: StateFlow<AuthState> = _authState.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private val _library = MutableStateFlow<List<LibraryItem>>(emptyList())
    val library: StateFlow<List<LibraryItem>> = _library.asStateFlow()

    private var handle: CoreHandle? = null

    private val listener = object : PlaybackStateListener {
        override fun onAuthStatus(status: String, detail: String) {
            _authState.value = when (status) {
                "Starting", "SignedOut" -> AuthState.SignedOut
                "WaitingForBrowser" -> AuthState.WaitingForBrowser(detail)
                "Connecting" -> AuthState.Connecting
                "Connected" -> AuthState.Connected(detail)
                "Failed" -> AuthState.Failed(detail)
                else -> AuthState.SignedOut
            }
        }

        override fun onError(message: String) {
            _error.value = message
        }

        override fun onLibrary(playlists: String) {
            _library.value = runCatching {
                val array = JSONArray(playlists)
                (0 until array.length()).map { index ->
                    val item = array.getJSONObject(index)
                    LibraryItem(item.getString("id"), item.getString("name"))
                }
            }.getOrDefault(emptyList())
        }
    }

    @Synchronized
    fun init(context: Context) {
        if (handle == null) {
            handle = CoreHandle()
            handle?.setCredentialStore(EncryptedCredentialStore(context.applicationContext))
            handle?.setListener(listener)
        }
    }

    fun signIn() = handle?.signIn()

    fun completeRedirect(code: String) = handle?.completeRedirect(code)

    fun signOut() = handle?.signOut()

    fun library() = handle?.library()
}
