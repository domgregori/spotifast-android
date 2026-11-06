package rocks.spotifast.android.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import rocks.spotifast.android.R

/**
 * Spotify's own brand swatches. Spotify's Android app is dark-only, so
 * [SpotifastColorScheme] below is the single source of truth for the app's
 * look — there is no light counterpart, matching the real client.
 */
object SpotifyBrand {
    /** The signature "Spotify green," used for primary actions and accents. */
    val Green = Color(0xFF1ED760)

    /** The slightly deeper green used for pressed states and secondary emphasis. */
    val GreenPressed = Color(0xFF1DB954)

    val Black = Color(0xFF000000)
    val NearBlack = Color(0xFF090909)

    /** The app's base surface — Spotify's signature near-black backdrop. */
    val Surface = Color(0xFF121212)

    /** Card/row surface, one step up from [Surface] (playlist rows, chips). */
    val SurfaceRow = Color(0xFF181818)

    /** A further step up, used for pressed rows and elevated sheets. */
    val SurfaceElevated = Color(0xFF212121)

    /** The highest elevation step — hover/pressed chips, search field fill. */
    val SurfaceHighest = Color(0xFF282828)

    val TextPrimary = Color(0xFFFFFFFF)
    val TextSecondary = Color(0xFFB3B3B3)

    val Outline = Color(0xFF3A3A3A)
    val OutlineVariant = Color(0xFF2A2A2A)

    val Error = Color(0xFFF15E6C)
    val ErrorContainer = Color(0xFF4E0F17)
}

/**
 * The "Panel Player" comic/noir palette: light marks on a near-black page
 * instead of Spotify's near-black-on-near-black, with a hot-crimson accent
 * standing in for the signature green. Pulled straight from the Noir mode
 * of the comic-book UI concept explored separately — only the color roles
 * carry over here, not its display type or panel shapes.
 */
object NoirBrand {
    val Crimson = Color(0xFFFF3B57)
    val CrimsonDeep = Color(0xFFD81E3F)
    val Cyan = Color(0xFF4FC3F7)

    val Black = Color(0xFF000000)

    /** The app's base surface — darker than Spotify's, with a warm tint
     *  instead of true neutral black. */
    val Surface = Color(0xFF1A171C)
    val SurfaceRow = Color(0xFF221E23)
    val SurfaceElevated = Color(0xFF2A262C)
    val SurfaceHighest = Color(0xFF332E35)
    val PageBlack = Color(0xFF07070A)

    /** Off-white rather than pure white — ink on newsprint, not a screen glow. */
    val TextPrimary = Color(0xFFEFE9DC)
    val TextSecondary = Color(0xFF948B86)

    val Outline = Color(0xFF3A353B)
    val OutlineVariant = Color(0xFF2A262C)

    val Error = Color(0xFFFF3B57)
    val ErrorContainer = Color(0xFF4A0F1A)
}

/**
 * The app's Material 3 [androidx.compose.material3.ColorScheme], expressed
 * with Spotify's brand colors standing in for a dynamic, wallpaper-derived
 * palette. Every Material role is mapped deliberately rather than leaving
 * defaults, so components (buttons, fields, cards, nav bars) pick up the
 * Spotify look automatically instead of needing per-call `Color(...)` hacks.
 */
private val SpotifastColorScheme = darkColorScheme(
    primary = SpotifyBrand.Green,
    onPrimary = SpotifyBrand.Black,
    primaryContainer = SpotifyBrand.GreenPressed,
    onPrimaryContainer = SpotifyBrand.Black,
    inversePrimary = SpotifyBrand.GreenPressed,

    secondary = SpotifyBrand.TextSecondary,
    onSecondary = SpotifyBrand.Black,
    secondaryContainer = SpotifyBrand.SurfaceHighest,
    onSecondaryContainer = SpotifyBrand.TextPrimary,

    tertiary = SpotifyBrand.GreenPressed,
    onTertiary = SpotifyBrand.Black,
    tertiaryContainer = SpotifyBrand.SurfaceElevated,
    onTertiaryContainer = SpotifyBrand.TextPrimary,

    background = SpotifyBrand.Surface,
    onBackground = SpotifyBrand.TextPrimary,

    surface = SpotifyBrand.Surface,
    onSurface = SpotifyBrand.TextPrimary,
    surfaceVariant = SpotifyBrand.SurfaceElevated,
    onSurfaceVariant = SpotifyBrand.TextSecondary,
    surfaceTint = SpotifyBrand.Green,

    surfaceDim = SpotifyBrand.NearBlack,
    surfaceBright = SpotifyBrand.SurfaceHighest,
    surfaceContainerLowest = SpotifyBrand.Black,
    surfaceContainerLow = SpotifyBrand.NearBlack,
    surfaceContainer = SpotifyBrand.SurfaceRow,
    surfaceContainerHigh = SpotifyBrand.SurfaceElevated,
    surfaceContainerHighest = SpotifyBrand.SurfaceHighest,

    inverseSurface = SpotifyBrand.TextPrimary,
    inverseOnSurface = SpotifyBrand.Surface,

    error = SpotifyBrand.Error,
    onError = SpotifyBrand.Black,
    errorContainer = SpotifyBrand.ErrorContainer,
    onErrorContainer = SpotifyBrand.Error,

    outline = SpotifyBrand.Outline,
    outlineVariant = SpotifyBrand.OutlineVariant,
    scrim = SpotifyBrand.Black,
)

private val NoirColorScheme = darkColorScheme(
    primary = NoirBrand.Crimson,
    onPrimary = NoirBrand.Black,
    primaryContainer = NoirBrand.CrimsonDeep,
    onPrimaryContainer = NoirBrand.TextPrimary,
    inversePrimary = NoirBrand.CrimsonDeep,

    secondary = NoirBrand.TextSecondary,
    onSecondary = NoirBrand.Black,
    secondaryContainer = NoirBrand.SurfaceHighest,
    onSecondaryContainer = NoirBrand.TextPrimary,

    tertiary = NoirBrand.Cyan,
    onTertiary = NoirBrand.Black,
    tertiaryContainer = NoirBrand.SurfaceElevated,
    onTertiaryContainer = NoirBrand.TextPrimary,

    background = NoirBrand.Surface,
    onBackground = NoirBrand.TextPrimary,

    surface = NoirBrand.Surface,
    onSurface = NoirBrand.TextPrimary,
    surfaceVariant = NoirBrand.SurfaceElevated,
    onSurfaceVariant = NoirBrand.TextSecondary,
    surfaceTint = NoirBrand.Crimson,

    surfaceDim = NoirBrand.PageBlack,
    surfaceBright = NoirBrand.SurfaceHighest,
    surfaceContainerLowest = NoirBrand.Black,
    surfaceContainerLow = NoirBrand.PageBlack,
    surfaceContainer = NoirBrand.SurfaceRow,
    surfaceContainerHigh = NoirBrand.SurfaceElevated,
    surfaceContainerHighest = NoirBrand.SurfaceHighest,

    inverseSurface = NoirBrand.TextPrimary,
    inverseOnSurface = NoirBrand.Surface,

    error = NoirBrand.Error,
    onError = NoirBrand.Black,
    errorContainer = NoirBrand.ErrorContainer,
    onErrorContainer = NoirBrand.Error,

    outline = NoirBrand.Outline,
    outlineVariant = NoirBrand.OutlineVariant,
    scrim = NoirBrand.Black,
)

/** Bangers — the comic-shout display face from the Panel Player concept —
 *  for Noir mode's big headlines; Barlow Condensed for everything smaller,
 *  the same pairing the concept used for metadata and buttons. */
private val ComicDisplayFont = FontFamily(Font(R.font.bangers_regular, FontWeight.Normal))
private val ComicBodyFont = FontFamily(
    Font(R.font.barlow_condensed_medium, FontWeight.Medium),
    Font(R.font.barlow_condensed_semibold, FontWeight.SemiBold),
    Font(R.font.barlow_condensed_bold, FontWeight.Bold),
    Font(R.font.barlow_condensed_extrabold, FontWeight.ExtraBold),
)

/**
 * A type scale that keeps Material 3's default (system) font but leans into
 * Spotify's heavy, condensed display weights for headings — the
 * "Good evening" greeting, section titles, the sign-in splash — while
 * leaving body/label text at normal weight for legibility in dense lists.
 */
private val SpotifastTypography = Typography().let { base ->
    base.copy(
        displayLarge = base.displayLarge.copy(fontWeight = FontWeight.Black, letterSpacing = (-0.5).sp),
        displayMedium = base.displayMedium.copy(fontWeight = FontWeight.Black),
        displaySmall = base.displaySmall.copy(fontWeight = FontWeight.ExtraBold),
        headlineLarge = base.headlineLarge.copy(fontWeight = FontWeight.ExtraBold),
        headlineMedium = base.headlineMedium.copy(fontWeight = FontWeight.ExtraBold),
        headlineSmall = base.headlineSmall.copy(fontWeight = FontWeight.Bold),
        titleLarge = base.titleLarge.copy(fontWeight = FontWeight.Bold),
        titleMedium = base.titleMedium.copy(fontWeight = FontWeight.Bold),
        titleSmall = base.titleSmall.copy(fontWeight = FontWeight.SemiBold),
        labelLarge = base.labelLarge.copy(fontWeight = FontWeight.Bold, letterSpacing = 0.2.sp),
        labelMedium = base.labelMedium.copy(fontWeight = FontWeight.SemiBold),
        bodyLarge = base.bodyLarge.copy(fontWeight = FontWeight.Normal),
        bodyMedium = base.bodyMedium.copy(fontWeight = FontWeight.Normal),
        bodySmall = base.bodySmall.copy(fontWeight = FontWeight.Medium),
    )
}

/**
 * Shapes biased toward Spotify's fully-rounded "pill" buttons and search
 * fields ([extraLarge]), with softer, large-radius cards for rows and tiles
 * ([large]/[medium]) rather than Material's default small corner radii.
 */
/**
 * Noir mode's type scale: Bangers for the sizes that act like a comic
 * masthead or splash-page title (display/headline/the big titleLarge
 * section tags), Barlow Condensed everywhere smaller — dense lists and
 * buttons stay legible rather than trying to set body text in a shout font.
 */
private val NoirTypography = SpotifastTypography.copy(
    displayLarge = SpotifastTypography.displayLarge.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal),
    displayMedium = SpotifastTypography.displayMedium.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal),
    displaySmall = SpotifastTypography.displaySmall.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal),
    headlineLarge = SpotifastTypography.headlineLarge.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal),
    headlineMedium = SpotifastTypography.headlineMedium.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal),
    headlineSmall = SpotifastTypography.headlineSmall.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal),
    titleLarge = SpotifastTypography.titleLarge.copy(fontFamily = ComicDisplayFont, fontWeight = FontWeight.Normal, letterSpacing = 0.4.sp),
    titleMedium = SpotifastTypography.titleMedium.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.Bold),
    titleSmall = SpotifastTypography.titleSmall.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.Bold),
    labelLarge = SpotifastTypography.labelLarge.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.Bold),
    labelMedium = SpotifastTypography.labelMedium.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.SemiBold),
    labelSmall = SpotifastTypography.labelSmall.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.SemiBold),
    bodyLarge = SpotifastTypography.bodyLarge.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.Medium),
    bodyMedium = SpotifastTypography.bodyMedium.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.Medium),
    bodySmall = SpotifastTypography.bodySmall.copy(fontFamily = ComicBodyFont, fontWeight = FontWeight.SemiBold),
)

private val SpotifastShapes = Shapes(
    extraSmall = RoundedCornerShape(4.dp),
    small = RoundedCornerShape(8.dp),
    medium = RoundedCornerShape(12.dp),
    large = RoundedCornerShape(16.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

/** Noir mode's shapes: sharper, smaller corners than Spotify's rounded
 *  pills — closer to an inked comic panel than a soft Material card. */
private val NoirShapes = Shapes(
    extraSmall = RoundedCornerShape(4.dp),
    small = RoundedCornerShape(6.dp),
    medium = RoundedCornerShape(8.dp),
    large = RoundedCornerShape(10.dp),
    extraLarge = RoundedCornerShape(18.dp),
)

/** Whether Noir mode is active — read this instead of comparing
 *  [rocks.spotifast.android.AppTheme] directly so presentation code doesn't
 *  need to depend on [rocks.spotifast.android.SpotifyCore]. Components that
 *  draw an ink border or swap a glyph's tint check this. */
val LocalIsNoir = staticCompositionLocalOf { false }

/** The app's selectable color themes — see [rocks.spotifast.android.AppTheme]
 *  for the persisted setting this maps from. */
enum class ThemeChoice { Spotify, Noir }

/**
 * Wraps the app in its Material 3 theme: Spotify's dark, green-accented look
 * by default, or the full comic/noir treatment — palette, Bangers/Barlow
 * Condensed type, sharper panel shapes — when [theme] is [ThemeChoice.Noir].
 * Also provides [LocalIsNoir] so components that need to do more than swap
 * a `MaterialTheme.colorScheme` role (an ink border, the Ink Outline art
 * filter) know which mode is active. This is the single themed entry
 * point — replaces the bare `MaterialTheme { }` call that previously
 * wrapped [rocks.spotifast.android.MainActivity]'s content.
 */
@Composable
fun SpotifastTheme(theme: ThemeChoice = ThemeChoice.Spotify, content: @Composable () -> Unit) {
    val noir = theme == ThemeChoice.Noir
    CompositionLocalProvider(LocalIsNoir provides noir) {
        MaterialTheme(
            colorScheme = if (noir) NoirColorScheme else SpotifastColorScheme,
            typography = if (noir) NoirTypography else SpotifastTypography,
            shapes = if (noir) NoirShapes else SpotifastShapes,
            content = content,
        )
    }
}
