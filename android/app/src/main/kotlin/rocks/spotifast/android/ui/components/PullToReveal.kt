package rocks.spotifast.android.ui.components

import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlin.math.min

/**
 * A nested-scroll connection that calls [onReveal] once a downward pull from
 * the very top of [listState] passes [threshold]. Spotify's own library and
 * playlist screens keep search behind exactly this gesture instead of a
 * persistent field, so the common case — just browsing — stays uncluttered.
 * Attach via `Modifier.nestedScroll(...)` on the scrollable's parent; once
 * [revealed] is true this stops intercepting and lets the list scroll
 * normally again.
 */
@Composable
fun rememberPullToRevealConnection(
    listState: LazyListState,
    revealed: Boolean,
    onReveal: () -> Unit,
    threshold: Dp = 56.dp,
): NestedScrollConnection {
    val thresholdPx = with(LocalDensity.current) { threshold.toPx() }
    val pulled = remember { mutableFloatStateOf(0f) }
    return remember(listState, revealed, thresholdPx) {
        object : NestedScrollConnection {
            override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
                if (revealed) return Offset.Zero
                val atTop = listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset == 0
                if (!atTop || available.y <= 0f) {
                    pulled.floatValue = 0f
                    return Offset.Zero
                }
                pulled.floatValue = min(pulled.floatValue + available.y, thresholdPx)
                if (pulled.floatValue >= thresholdPx) {
                    pulled.floatValue = 0f
                    onReveal()
                }
                // Consumed ourselves, so the list doesn't also try to scroll
                // or overscroll-bounce while this gesture is driving the reveal.
                return available
            }
        }
    }
}
