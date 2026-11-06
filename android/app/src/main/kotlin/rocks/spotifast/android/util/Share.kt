package rocks.spotifast.android.util

import android.content.Context
import android.content.Intent

/** Shares a `spotify:kind:id` uri as its web link
 *  (`https://open.spotify.com/kind/id`), via Android's own share sheet —
 *  the same link format Spotify's own apps share. */
fun shareUri(context: Context, uri: String, label: String) {
    val parts = uri.removePrefix("spotify:").split(":")
    if (parts.size < 2) return
    val kind = parts[0]
    val id = parts[1]
    val link = "https://open.spotify.com/$kind/$id"
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, link)
    }
    context.startActivity(Intent.createChooser(intent, "Share $label"))
}
