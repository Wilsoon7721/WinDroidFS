package dev.wilsoon.windroidfs.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color

data class WinDroidColors(
    val background: Color,
    val surface: Color,
    /** Slightly raised surface: metric tiles, step bubbles, input backgrounds. */
    val surfaceVariant: Color,
    /** Hairlines and dividers. */
    val border: Color,
    val textPrimary: Color,
    val textSecondary: Color,
    val brand: Color,
    /** Text/icons drawn on top of [brand], [error], etc. */
    val onAccent: Color,
    val success: Color,
    val warning: Color,
    val error: Color,
    val isDark: Boolean
)

val LightColors = WinDroidColors(
    background = Color(0xFFF3F4F6),
    surface = Color(0xFFFFFFFF),
    surfaceVariant = Color(0xFFF3F4F6),
    border = Color(0xFFE5E7EB),
    textPrimary = Color(0xFF111827),
    textSecondary = Color(0xFF6B7280),
    brand = Color(0xFF2563EB),
    onAccent = Color(0xFFFFFFFF),
    success = Color(0xFF059669),
    warning = Color(0xFFD97706),
    error = Color(0xFFDC2626),
    isDark = false
)

// Accents are one step lighter in dark mode so they keep contrast against the dark surfaces.
val DarkColors = WinDroidColors(
    background = Color(0xFF0B1120),
    surface = Color(0xFF1F2937),
    surfaceVariant = Color(0xFF111827),
    border = Color(0xFF374151),
    textPrimary = Color(0xFFF9FAFB),
    textSecondary = Color(0xFF9CA3AF),
    brand = Color(0xFF60A5FA),
    onAccent = Color(0xFF0B1120),
    success = Color(0xFF34D399),
    warning = Color(0xFFFBBF24),
    error = Color(0xFFF87171),
    isDark = true
)

val LocalWinDroidColors = staticCompositionLocalOf<WinDroidColors> {
    error("No WinDroidColors provided! Did you forget to wrap your app in WinDroidTheme?")
}

/**
 * Material3 components (Switch, AlertDialog, OutlinedTextField, Button...) read from
 * MaterialTheme, not from WinDroidColors. This maps our palette onto theirs so those
 * components match the rest of the app instead of using Material's default purple.
 */
private fun WinDroidColors.toMaterialColorScheme(): ColorScheme {
    val base = if (isDark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = brand,
        onPrimary = onAccent,
        secondary = brand,
        onSecondary = onAccent,
        background = background,
        onBackground = textPrimary,
        surface = surface,
        onSurface = textPrimary,
        surfaceVariant = surfaceVariant,
        onSurfaceVariant = textSecondary,
        surfaceContainerHigh = surface,   // AlertDialog background
        surfaceContainerHighest = border, // Switch track when off
        outline = textSecondary,
        outlineVariant = border,
        error = error,
        onError = onAccent
    )
}

@Composable
fun WinDroidTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit
) {
    val currentColors = if (darkTheme) DarkColors else LightColors

    CompositionLocalProvider(LocalWinDroidColors provides currentColors) {
        MaterialTheme(
            colorScheme = currentColors.toMaterialColorScheme(),
            content = content
        )
    }
}

object WinDroidTheme {
    val colors: WinDroidColors
        @Composable
        get() = LocalWinDroidColors.current
}
