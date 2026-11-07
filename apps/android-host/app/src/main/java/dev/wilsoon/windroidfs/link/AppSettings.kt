package dev.wilsoon.windroidfs.link

import android.content.Context
import dev.wilsoon.windroidfs.ui.SettingsUiState
import dev.wilsoon.windroidfs.ui.StorageScope

/** The Settings screen's choices, kept across restarts. Read as the link goes, so a change applies at once. */
object AppSettings {
    private const val FILE = "settings"

    @Volatile private var cached: SettingsUiState? = null

    fun load(context: Context): SettingsUiState {
        cached?.let { return it }
        val prefs = prefs(context)
        val defaults = SettingsUiState()
        val exposed = prefs.getStringSet("exposed", null)
        val loaded = SettingsUiState(
            storageScope = prefs.getString("scope", null)
                ?.let { runCatching { StorageScope.valueOf(it) }.getOrNull() } ?: defaults.storageScope,
            exposedFolders = defaults.exposedFolders.mapValues { (folder, on) -> exposed?.contains(folder) ?: on },
            requireBiometrics = prefs.getBoolean("biometrics", defaults.requireBiometrics),
            keepScreenAwake = prefs.getBoolean("keep_awake", defaults.keepScreenAwake),
            tcpPort = prefs.getInt("port", defaults.tcpPort),
        )
        cached = loaded
        return loaded
    }

    fun save(context: Context, settings: SettingsUiState) {
        cached = settings
        prefs(context).edit()
            .putString("scope", settings.storageScope.name)
            .putStringSet("exposed", settings.exposedFolders.filterValues { it }.keys)
            .putBoolean("biometrics", settings.requireBiometrics)
            .putBoolean("keep_awake", settings.keepScreenAwake)
            .putInt("port", settings.tcpPort)
            .apply()
    }

    /** Top-level folders the PC may see, lowercase, or null for the whole storage. */
    fun exposedFolders(context: Context): Set<String>? {
        val settings = load(context)
        if (settings.storageScope == StorageScope.ENTIRE_STORAGE) return null
        return settings.exposedFolders.filterValues { it }.keys.map { it.lowercase() }.toSet()
    }

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
}
