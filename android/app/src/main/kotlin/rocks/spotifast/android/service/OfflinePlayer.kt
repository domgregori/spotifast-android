package rocks.spotifast.android.service

import android.content.Context
import android.net.Uri
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import rocks.spotifast.android.TrackItem
import java.io.File

/** The decrypted copy of a downloaded track, if it has one.
 *
 *  Downloads write these beside the audio cache, named after the track id and
 *  carrying the extension of the file Spotify served (see the core's
 *  `download` module). */
fun offlineAudioFile(filesDir: File, uri: String): File? {
    val id = uri.substringAfterLast(':')
    if (id.isEmpty()) return null
    val dir = File(File(filesDir, "cache"), "offline")
    return dir.listFiles()?.firstOrNull { file ->
        file.isFile && file.name.substringBefore('.') == id
    }
}

/** Plays downloaded tracks from their decrypted copies.
 *
 *  librespot cannot: even a pinned file is ciphertext that only its own player
 *  unlocks, through a session that needs the network. So with no network,
 *  playback moves here: plain audio files through Media3, in the order the
 *  tapped collection had, skipping anything that is not downloaded.
 */
class OfflinePlayer(context: Context, private val onChanged: () -> Unit) {
    private val filesDir = context.applicationContext.filesDir

    private val player: ExoPlayer = ExoPlayer.Builder(context.applicationContext).build().apply {
        setAudioAttributes(
            AudioAttributes.Builder()
                .setContentType(C.AUDIO_CONTENT_TYPE_MUSIC)
                .setUsage(C.USAGE_MEDIA)
                .build(),
            true,
        )
        addListener(object : Player.Listener {
            override fun onIsPlayingChanged(isPlaying: Boolean) = onChanged()

            override fun onPlaybackStateChanged(state: Int) {
                if (state == Player.STATE_ENDED) advance()
            }
        })
    }

    private var entries: List<Pair<TrackItem, File>> = emptyList()

    /** Where in [entries] the player is. */
    var index: Int = 0
        private set

    val isActive: Boolean get() = entries.isNotEmpty()
    val track: TrackItem? get() = entries.getOrNull(index)?.first
    val isPlaying: Boolean get() = isActive && player.isPlaying
    val positionMs: Long get() = if (isActive) player.currentPosition else 0L

    /** Starts playing [tracks] from [startIndex], keeping only the ones that
     *  are downloaded. Returns false when the tapped track itself has no
     *  copy, so the caller can fall back to the session. */
    fun play(tracks: List<TrackItem>, startIndex: Int): Boolean {
        val tapped = tracks.getOrNull(startIndex)?.uri ?: return false
        val playable = tracks.mapNotNull { track ->
            offlineAudioFile(filesDir, track.uri)?.let { track to it }
        }
        val at = playable.indexOfFirst { it.first.uri == tapped }
        if (at < 0) return false
        entries = playable
        start(at)
        return true
    }

    fun toggle() {
        if (!isActive) return
        if (player.isPlaying) player.pause() else player.play()
        onChanged()
    }

    fun seek(positionMs: Long) {
        if (!isActive) return
        player.seekTo(positionMs)
        onChanged()
    }

    fun next() = advance()

    fun previous() {
        if (isActive && index > 0) start(index - 1)
    }

    fun stop() {
        if (!isActive) return
        entries = emptyList()
        index = 0
        player.stop()
        player.clearMediaItems()
        onChanged()
    }

    private fun advance() {
        if (!isActive) return
        if (index + 1 < entries.size) start(index + 1) else stop()
    }

    private fun start(at: Int) {
        index = at
        val (_, file) = entries[at]
        player.setMediaItem(MediaItem.fromUri(Uri.fromFile(file)))
        player.prepare()
        player.play()
        onChanged()
    }
}
