package rocks.spotifast.android

/**
 * A link that opens one thing in the app.
 *
 * Three spellings reach the same place: the app's own
 * `spotifast://album/<id>`, Spotify's `spotify:album:<id>`, and the web
 * address `https://open.spotify.com/album/<id>` (including the `/intl-xx/`
 * form Spotify uses for localised links, and the older
 * `/user/<user>/playlist/<id>` shape).
 *
 * Parsing is plain Kotlin over the text, not `android.net.Uri`, so it is
 * covered by JVM unit tests.
 */
sealed interface DeepLink {
    data class Playlist(val id: String) : DeepLink

    data class Album(val id: String) : DeepLink

    data class Artist(val id: String) : DeepLink

    /** Played straight away, the way a shared track link should. */
    data class Track(val id: String) : DeepLink {
        val uri: String get() = "spotify:track:$id"
    }

    companion object {
        /** The kinds the app can open; an episode or show link is left alone. */
        private val kinds = setOf("track", "album", "playlist", "artist")

        private val idPattern = Regex("[A-Za-z0-9]{1,64}")

        /** The host the app answers on its own scheme. */
        const val SCHEME = "spotifast"

        private const val WEB_HOST = "open.spotify.com"

        fun parse(raw: String?): DeepLink? {
            val text = raw?.trim().orEmpty()
            if (text.isEmpty()) return null
            val uri = runCatching { java.net.URI(text) }.getOrNull() ?: return null
            return when (uri.scheme?.lowercase()) {
                SCHEME -> fromOwnScheme(uri)
                "spotify" -> fromSpotifyUri(uri.schemeSpecificPart)
                "https", "http" -> fromWeb(uri)
                else -> null
            }
        }

        /** `spotifast://<kind>/<id>`. */
        private fun fromOwnScheme(uri: java.net.URI): DeepLink? {
            val kind = uri.host?.lowercase() ?: return null
            val id = uri.path?.trim('/')?.substringBefore('/') ?: return null
            return build(kind, id)
        }

        /** `spotify:<kind>:<id>`. */
        private fun fromSpotifyUri(specific: String?): DeepLink? {
            val parts = specific?.split(':') ?: return null
            val kind = parts.getOrNull(0)?.lowercase() ?: return null
            val id = parts.getOrNull(1) ?: return null
            return build(kind, id)
        }

        /**
         * `https://open.spotify.com/<kind>/<id>`, with any `/intl-xx/` prefix
         * and the legacy `/user/<user>/playlist/<id>` shape.
         */
        private fun fromWeb(uri: java.net.URI): DeepLink? {
            if (!uri.host.equals(WEB_HOST, ignoreCase = true)) return null
            val segments = uri.path.orEmpty().split('/').filter { it.isNotEmpty() }
            val parts = segments.dropWhile { it.startsWith("intl-") }
            val playlistIndex = parts.indexOf("playlist")
            if (playlistIndex > 0) {
                // .../user/<user>/playlist/<id>
                return build("playlist", parts.getOrNull(playlistIndex + 1))
            }
            return build(parts.getOrNull(0)?.lowercase(), parts.getOrNull(1))
        }

        private fun build(kind: String?, id: String?): DeepLink? {
            if (kind == null || id == null || !kinds.contains(kind)) return null
            if (!idPattern.matches(id)) return null
            return when (kind) {
                "track" -> Track(id)
                "album" -> Album(id)
                "playlist" -> Playlist(id)
                "artist" -> Artist(id)
                else -> null
            }
        }
    }
}
