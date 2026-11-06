package rocks.spotifast.android.ui.components

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Sort
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import rocks.spotifast.android.LibrarySort

/** The sort-order control shown on Library and Home: a button that opens a
 *  menu of [LibrarySort] options, the active one checked. */
@Composable
fun SortMenuButton(current: LibrarySort, onSelect: (LibrarySort) -> Unit, modifier: Modifier = Modifier) {
    var expanded by remember { mutableStateOf(false) }
    IconButton(onClick = { expanded = true }, modifier = modifier) {
        Icon(
            imageVector = Icons.Filled.Sort,
            contentDescription = "Sort",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
        LibrarySort.entries.forEach { option ->
            DropdownMenuItem(
                text = { Text(option.label) },
                leadingIcon = if (option == current) {
                    { Icon(imageVector = Icons.Filled.Check, contentDescription = null) }
                } else {
                    null
                },
                onClick = {
                    onSelect(option)
                    expanded = false
                },
            )
        }
    }
}
