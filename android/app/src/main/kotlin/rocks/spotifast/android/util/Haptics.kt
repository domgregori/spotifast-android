package rocks.spotifast.android.util

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager

/** A short, subtle tick — confirmation feedback for a quick toggle like
 *  liking/unliking a track, not an alert. Uses the system's own tuned
 *  "tick" effect where available (API 29+); a brief fixed-amplitude pulse
 *  on older devices, since [VibrationEffect.EFFECT_TICK] isn't defined
 *  there. No-ops quietly if the device has no vibrator at all. */
fun vibrateTick(context: Context) {
    val vibrator = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        val manager = context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager
        manager?.defaultVibrator
    } else {
        @Suppress("DEPRECATION")
        context.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
    } ?: return
    if (!vibrator.hasVibrator()) return
    val effect = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        VibrationEffect.createPredefined(VibrationEffect.EFFECT_TICK)
    } else {
        VibrationEffect.createOneShot(20, VibrationEffect.DEFAULT_AMPLITUDE)
    }
    vibrator.vibrate(effect)
}
