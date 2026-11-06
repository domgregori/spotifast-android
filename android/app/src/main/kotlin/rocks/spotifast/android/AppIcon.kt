package rocks.spotifast.android

import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager

/** The launcher icon choices declared as `activity-alias` entries in the
 *  manifest — Android has no API to change one activity's own icon at
 *  runtime, only to toggle which of several statically-declared aliases is
 *  active, so a choice here is really "enable this alias, disable the
 *  others," not a direct icon swap. */
enum class AppIcon(val label: String, val componentName: String) {
    Indigo("Indigo", "rocks.spotifast.android.IconIndigo"),
    Comic("Comic", "rocks.spotifast.android.IconComic"),
}

/** Which alias is currently enabled — queried from the system rather than
 *  a separate preference, so there's nothing to fall out of sync with the
 *  actual launcher icon. */
fun currentAppIcon(context: Context): AppIcon {
    val packageManager = context.packageManager
    return AppIcon.entries.firstOrNull { icon ->
        val state = packageManager.getComponentEnabledSetting(ComponentName(context, icon.componentName))
        val enabledInManifest = icon == AppIcon.Indigo // the alias enabled="true" by default
        state == PackageManager.COMPONENT_ENABLED_STATE_ENABLED ||
            (state == PackageManager.COMPONENT_ENABLED_STATE_DEFAULT && enabledInManifest)
    } ?: AppIcon.Indigo
}

/** Enables `icon`'s alias and disables every other one. `DONT_KILL_APP`
 *  keeps this session running; the launcher picks up the new icon on its
 *  own, sometimes only after it next redraws (returning to the home
 *  screen, or a relaunch). */
fun setAppIcon(context: Context, icon: AppIcon) {
    val packageManager = context.packageManager
    AppIcon.entries.forEach { candidate ->
        val state = if (candidate == icon) {
            PackageManager.COMPONENT_ENABLED_STATE_ENABLED
        } else {
            PackageManager.COMPONENT_ENABLED_STATE_DISABLED
        }
        packageManager.setComponentEnabledSetting(
            ComponentName(context, candidate.componentName),
            state,
            PackageManager.DONT_KILL_APP,
        )
    }
}
