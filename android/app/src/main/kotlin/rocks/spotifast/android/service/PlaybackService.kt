package rocks.spotifast.android.service

import android.app.PendingIntent
import android.content.Intent
import android.os.Bundle
import androidx.media3.common.util.UnstableApi
import androidx.media3.session.CommandButton
import androidx.media3.session.MediaSession
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionResult
import androidx.media3.session.MediaSessionService
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import rocks.spotifast.android.MainActivity
import rocks.spotifast.android.PlaybackPhase
import rocks.spotifast.android.SpotifyCore

private const val ACTION_TOGGLE_LIKE = "rocks.spotifast.android.TOGGLE_LIKE"

/**
 * Keeps the engine alive and network-exempt in the background. Android's Doze/App Standby
 * network throttling targets ordinary background processes; a foreground service with a
 * [MediaSession] reporting active playback is the sanctioned way out of that, and `mediaPlayback`
 * + `connectedDevice` (declared in the manifest) are what let the system grant it. media3 handles
 * the actual `startForeground()` call and notification itself, driven entirely by
 * [SpotifastPlayer]'s reported state — there is no manual notification-building here.
 *
 * The one piece of notification UI this does build by hand is the "Save to Liked Songs" heart,
 * matching Spotify's own lock-screen widget: a custom session command the system renders as a
 * button next to play/pause, pushed fresh via both [MediaSession.setCustomLayout] and
 * [MediaSession.setMediaButtonPreferences] whenever the current track or its liked state
 * changes, since media3 only derives play/pause/skip from [SpotifastPlayer]'s `State` —
 * everything else is explicit.
 */
@UnstableApi
class PlaybackService : MediaSessionService() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private lateinit var player: SpotifastPlayer
    private var mediaSession: MediaSession? = null
    private val toggleLikeCommand = SessionCommand(ACTION_TOGGLE_LIKE, Bundle.EMPTY)

    override fun onCreate() {
        super.onCreate()
        SpotifyCore.init(applicationContext)
        player = SpotifastPlayer()

        val sessionActivity = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        mediaSession = MediaSession.Builder(this, player)
            .setSessionActivity(sessionActivity)
            .setCallback(object : MediaSession.Callback {
                override fun onConnect(
                    session: MediaSession,
                    controller: MediaSession.ControllerInfo,
                ): MediaSession.ConnectionResult {
                    val sessionCommands = MediaSession.ConnectionResult.DEFAULT_SESSION_COMMANDS
                        .buildUpon()
                        .add(toggleLikeCommand)
                        .build()
                    val buttons = listOf(likeButton(currentTrackLiked()))
                    return MediaSession.ConnectionResult.AcceptedResultBuilder(session)
                        .setAvailableSessionCommands(sessionCommands)
                        // Both: customLayout is what older controllers (and some
                        // Bluetooth/Auto surfaces) read; mediaButtonPreferences is
                        // Android 13+'s redesigned media player's own source for
                        // the same buttons. Setting only one left the lock-screen
                        // widget's heart stuck at its connect-time state.
                        .setCustomLayout(buttons)
                        .setMediaButtonPreferences(buttons)
                        .build()
                }

                override fun onCustomCommand(
                    session: MediaSession,
                    controller: MediaSession.ControllerInfo,
                    customCommand: SessionCommand,
                    args: Bundle,
                ): ListenableFuture<SessionResult> {
                    if (customCommand.customAction == ACTION_TOGGLE_LIKE) {
                        SpotifyCore.localPlayback.value?.track?.uri?.let { SpotifyCore.toggleLiked(it) }
                    }
                    return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
                }
            })
            .build()
        // Nothing ever binds a MediaController to this service — the app's UI
        // talks to SpotifyCore directly — so onGetSession() never runs and
        // never registers the session on its own. Without this, media3's
        // notification/foreground-promotion machinery (wired up inside
        // addSession) never activates: the service starts, but never
        // actually becomes a true foreground service.
        addSession(mediaSession!!)

        scope.launch {
            SpotifyCore.localPlayback.collect { playback ->
                player.update(playback?.track, playback?.phase ?: PlaybackPhase.Stopped, playback?.positionMs ?: 0)
            }
        }

        scope.launch {
            combine(SpotifyCore.localPlayback, SpotifyCore.likedTracks) { playback, liked ->
                val uri = playback?.track?.uri
                uri to (uri != null && liked[uri] == true)
            }
                .distinctUntilChanged()
                .collect { (uri, liked) ->
                    if (uri != null) {
                        SpotifyCore.checkLiked(uri)
                        val buttons = listOf(likeButton(liked))
                        mediaSession?.setCustomLayout(buttons)
                        mediaSession?.setMediaButtonPreferences(buttons)
                    }
                }
        }
    }

    private fun currentTrackLiked(): Boolean {
        val uri = SpotifyCore.localPlayback.value?.track?.uri ?: return false
        return SpotifyCore.likedTracks.value[uri] == true
    }

    private fun likeButton(liked: Boolean): CommandButton =
        CommandButton.Builder(if (liked) CommandButton.ICON_HEART_FILLED else CommandButton.ICON_HEART_UNFILLED)
            .setSessionCommand(toggleLikeCommand)
            .setDisplayName(if (liked) "Remove from Liked Songs" else "Save to Liked Songs")
            .setEnabled(true)
            .build()

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = mediaSession

    override fun onDestroy() {
        mediaSession?.let {
            player.release()
            it.release()
            mediaSession = null
        }
        super.onDestroy()
    }
}
