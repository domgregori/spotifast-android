package rocks.spotifast.android.service

import android.net.Uri
import android.os.Looper
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.Player
import androidx.media3.common.SimpleBasePlayer
import androidx.media3.common.util.UnstableApi
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import rocks.spotifast.android.PlaybackPhase
import rocks.spotifast.android.PlayingTrack
import rocks.spotifast.android.SpotifyCore

/**
 * Mirrors [SpotifyCore]'s playback state into a [media3][androidx.media3] player so
 * [PlaybackService] can expose it as a [androidx.media3.session.MediaSession] — lock-screen/
 * Bluetooth controls and the foreground-service notification, both handled by media3 once this
 * reports an active track. The queue itself lives in librespot, not here: [getState] always
 * reports a single-item "playlist" (whatever [SpotifyCore.localPlayback] currently points at),
 * and skip-next/previous forward straight to the engine rather than walking a local index.
 */
@UnstableApi
class SpotifastPlayer(looper: Looper = Looper.getMainLooper()) : SimpleBasePlayer(looper) {

    private var track: PlayingTrack? = null
    private var phase: PlaybackPhase = PlaybackPhase.Stopped
    private var positionMs: Long = 0

    /** Called on the main thread by [PlaybackService] whenever [SpotifyCore.localPlayback] changes. */
    fun update(track: PlayingTrack?, phase: PlaybackPhase, positionMs: Long) {
        this.track = track
        this.phase = phase
        this.positionMs = positionMs
        invalidateState()
    }

    override fun getState(): State {
        val currentTrack = track
        val commands = Player.Commands.Builder()
            .addAll(
                Player.COMMAND_PLAY_PAUSE,
                Player.COMMAND_GET_CURRENT_MEDIA_ITEM,
                Player.COMMAND_GET_METADATA,
                Player.COMMAND_GET_TIMELINE,
            )
            .apply {
                if (currentTrack != null) {
                    add(Player.COMMAND_SEEK_TO_NEXT)
                    add(Player.COMMAND_SEEK_TO_PREVIOUS)
                }
            }
            .build()

        // The engine can legitimately report Loading before it knows which
        // track it's loading (core/src/player.rs's apply_event sets
        // `playback = Loading` without touching `track` on a first-ever
        // load) — media3 requires an empty playlist's state to be IDLE or
        // ENDED, so a track-less state is always IDLE regardless of phase.
        val playbackState = if (currentTrack == null) {
            Player.STATE_IDLE
        } else {
            when (phase) {
                PlaybackPhase.Stopped -> Player.STATE_IDLE
                PlaybackPhase.Loading -> Player.STATE_BUFFERING
                PlaybackPhase.Playing, PlaybackPhase.Paused -> Player.STATE_READY
            }
        }

        return State.Builder()
            .setAvailableCommands(commands)
            .setPlaybackState(playbackState)
            .setPlayWhenReady(phase == PlaybackPhase.Playing, Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST)
            .setPlaylist(currentTrack?.let { listOf(mediaItemData(it)) } ?: emptyList())
            .setCurrentMediaItemIndex(0)
            .setContentPositionMs(positionMs)
            .build()
    }

    override fun handleSetPlayWhenReady(playWhenReady: Boolean): ListenableFuture<*> {
        if (playWhenReady != (phase == PlaybackPhase.Playing)) {
            SpotifyCore.togglePlayback()
        }
        return Futures.immediateVoidFuture()
    }

    override fun handleSeek(mediaItemIndex: Int, positionMs: Long, seekCommand: Int): ListenableFuture<*> {
        when (seekCommand) {
            Player.COMMAND_SEEK_TO_NEXT -> SpotifyCore.skipNext()
            Player.COMMAND_SEEK_TO_PREVIOUS -> SpotifyCore.skipPrevious()
        }
        return Futures.immediateVoidFuture()
    }

    private fun mediaItemData(track: PlayingTrack): MediaItemData {
        val metadata = MediaMetadata.Builder()
            .setTitle(track.title)
            .setArtist(track.artist)
            .setAlbumTitle(track.album)
            .apply { track.artUrl?.let { setArtworkUri(Uri.parse(it)) } }
            .build()
        val mediaItem = MediaItem.Builder()
            .setMediaId(track.uri)
            .setMediaMetadata(metadata)
            .build()
        return MediaItemData.Builder(track.uri)
            .setMediaItem(mediaItem)
            .setDurationUs(track.durationMs * 1000)
            .build()
    }
}
