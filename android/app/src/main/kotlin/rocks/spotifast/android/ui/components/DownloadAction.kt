package rocks.spotifast.android.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.DownloadDone
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import rocks.spotifast.android.DownloadProgress

/**
 * The download state and its toggle, in an album's or playlist's header.
 *
 * One control for the whole collection rather than a per-row one: a download
 * covers every track of what is on screen, which is also all the app can
 * honestly promise. It reads like the "Save" row beside it — the icon and word
 * show the state, and tapping flips it — so "Downloaded" removes the download
 * when tapped, exactly as "Saved" unsaves.
 *
 * Hidden while [progress] is running for another collection: the in-flight
 * batch is reported where it belongs, not on a page it has nothing to do with.
 */
@Composable
fun DownloadAction(
    downloaded: Boolean,
    progress: DownloadProgress?,
    onDownload: () -> Unit,
    onRemove: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (progress != null) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = modifier) {
            CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(6.dp))
            Text(
                text = "Downloading ${progress.done} of ${progress.total}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier.clickable { if (downloaded) onRemove() else onDownload() },
    ) {
        Icon(
            imageVector = if (downloaded) Icons.Filled.DownloadDone else Icons.Filled.Download,
            contentDescription = if (downloaded) "Remove download" else "Download",
            tint = if (downloaded) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(16.dp),
        )
        Spacer(Modifier.width(6.dp))
        Text(
            text = if (downloaded) "Downloaded" else "Download",
            style = MaterialTheme.typography.bodySmall,
            color = if (downloaded) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
