package rocks.spotifast.android.ui.components

import android.graphics.Bitmap
import coil3.size.Size
import coil3.transform.Transformation

/**
 * The comic concept's "Ink Outline" cover-art effect, ported from its SVG
 * filter (posterize the fill, erode the grayscale version and subtract it
 * from itself to get an edge mask, draw that mask in black over the
 * posterized color) to a plain per-pixel Bitmap pass, since Compose has no
 * SVG-filter equivalent to reach for.
 */
class InkOutlineTransformation : Transformation() {
    override val cacheKey: String = "InkOutlineTransformation"

    override suspend fun transform(input: Bitmap, size: Size): Bitmap {
        val width = input.width
        val height = input.height
        val pixelCount = width * height
        val src = IntArray(pixelCount)
        input.getPixels(src, 0, width, 0, 0, width, height)

        val gray = IntArray(pixelCount)
        for (i in 0 until pixelCount) {
            val p = src[i]
            val r = (p shr 16) and 0xFF
            val g = (p shr 8) and 0xFF
            val b = p and 0xFF
            gray[i] = (0.299f * r + 0.587f * g + 0.114f * b).toInt()
        }

        // 3x3 erode (min filter): the edge mask is the gap between a pixel
        // and its eroded neighborhood — near-zero in flat regions, large
        // right at a boundary.
        val eroded = IntArray(pixelCount)
        for (y in 0 until height) {
            for (x in 0 until width) {
                var min = 255
                for (dy in -1..1) {
                    val ny = (y + dy).coerceIn(0, height - 1)
                    for (dx in -1..1) {
                        val nx = (x + dx).coerceIn(0, width - 1)
                        val v = gray[ny * width + nx]
                        if (v < min) min = v
                    }
                }
                eroded[y * width + x] = min
            }
        }

        val out = IntArray(pixelCount)
        for (i in 0 until pixelCount) {
            val edge = gray[i] - eroded[i]
            out[i] = if (edge > 14) {
                0xFF000000.toInt()
            } else {
                val p = src[i]
                val a = (p ushr 24) and 0xFF
                val r = posterize((p shr 16) and 0xFF)
                val g = posterize((p shr 8) and 0xFF)
                val b = posterize(p and 0xFF)
                (a shl 24) or (r shl 16) or (g shl 8) or b
            }
        }

        val result = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        result.setPixels(out, 0, width, 0, 0, width, height)
        return result
    }

    private fun posterize(value: Int): Int {
        val step = 255 / (LEVELS - 1)
        return ((value / step) * step).coerceIn(0, 255)
    }

    override fun equals(other: Any?) = other is InkOutlineTransformation

    override fun hashCode() = javaClass.hashCode()

    private companion object {
        const val LEVELS = 4
    }
}
