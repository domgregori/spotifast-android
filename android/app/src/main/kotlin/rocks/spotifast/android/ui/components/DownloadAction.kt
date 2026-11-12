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
 * honestly promise. It reads like the "Save" row beside it, and tapping flips
 * what it shows: "Downloaded" removes the download, while "Downloaded · 2 new"
 * brings it up to date instead, since that is the thing still to do. Once it
 * is up to date, the same tap removes it.
 *
 * Hidden while [progress] is running for another collection: the in-flight
 * batch is reported where it belongs, not on a page it has nothing to do with.
 */
@Composable
fun DownloadAction(
    downloaded: Boolean,
    newCount: Int,
    goneCount: Int,
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
    val stale = downloaded && (newCount > 0 || goneCount > 0)
    val label = when {
        !downloaded -> "Download"
        !stale -> "Downloaded"
        else -> {
            val changes = buildList {
                if (newCount > 0) add("$newCount new")
                if (goneCount > 0) add("$goneCount removed")
            }
            "Downloaded · " + changes.joinToString(", ")
        }
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier.clickable { if (stale || !downloaded) onDownload() else onRemove() },
    ) {
        Icon(
            imageVector = if (downloaded) Icons.Filled.DownloadDone else Icons.Filled.Download,
            contentDescription = if (stale || !downloaded) "Download" else "Remove download",
            tint = if (downloaded) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(16.dp),
        )
        Spacer(Modifier.width(6.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.bodySmall,
            color = if (downloaded) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
