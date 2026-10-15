package dev.wilsoon.windroidfs

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.*
import dev.wilsoon.windroidfs.ui.*
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

enum class AppScreen {
    ONBOARDING, DASHBOARD, SETTINGS
}

class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        enableEdgeToEdge()
        setContent {
            WinDroidTheme {
                WinDroidApp()
            }
        }
    }

    @Composable
    private fun WinDroidApp() {
        var currentScreen by remember {
            mutableStateOf(if (hasAllFilesPermission()) AppScreen.DASHBOARD else AppScreen.ONBOARDING)
        }

        var dashboardState by remember { mutableStateOf(DashboardUiState()) }
        var settingsState by remember { mutableStateOf(SettingsUiState()) }

        when (currentScreen) {
            AppScreen.ONBOARDING -> {
                OnboardingScreen(
                    onOpenAccessSettings = { openAllFilesSettings() },
                    onCheckAgain = {
                        if (hasAllFilesPermission()) {
                            currentScreen = AppScreen.DASHBOARD
                        }
                    }
                )
            }
            AppScreen.DASHBOARD -> {
                MainScreen(
                    state = dashboardState,
                    onMethodSelected = { dashboardState = dashboardState.copy(expandedMethod = it) },
                    onOpenSettings = { currentScreen = AppScreen.SETTINGS },
                    onOpenDeveloperOptions = { openDeveloperOptions() },
                    onOpenTetheringSettings = { openTetheringSettings() },
                    onTerminateConnection = {
                        // For now, just drop the fake session
                        dashboardState = dashboardState.copy(session = null)
                    }
                )
            }
            AppScreen.SETTINGS -> {
                SettingsScreen(
                    state = settingsState,
                    onBack = { currentScreen = AppScreen.DASHBOARD },
                    onStorageScopeChange = { settingsState = settingsState.copy(storageScope = it) },
                    onFolderToggle = { folder, isExposed ->
                        val updatedFolders = settingsState.exposedFolders.toMutableMap()
                        updatedFolders[folder] = isExposed
                        settingsState = settingsState.copy(exposedFolders = updatedFolders)
                    },
                    onRequireBiometricsChange = { settingsState = settingsState.copy(requireBiometrics = it) },
                    onKeepScreenAwakeChange = { settingsState = settingsState.copy(keepScreenAwake = it) },
                    onForegroundServiceChange = { settingsState = settingsState.copy(runAsForegroundService = it) },
                    onPortChange = { settingsState = settingsState.copy(tcpPort = it) }
                )
            }
        }
    }

    private fun hasAllFilesPermission(): Boolean {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            // TODO Android 10 and below use standard read/write permissions. This fixture is temporary.
            // Return true for now to test the UI on older devices.
            true
        }
    }

    private fun openAllFilesSettings() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            try {
                val intent = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION)
                intent.data = Uri.parse("package:$packageName")
                startActivity(intent)
            } catch (e: Exception) {
                // Fallback if package intent fails
                val intent = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
                startActivity(intent)
            }
        }
    }

    private fun openDeveloperOptions() {
        try {
            val intent = Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS)
            startActivity(intent)
        } catch (e: Exception) {
            // Fallback to main settings if Developer Options isn't enabled yet
            startActivity(Intent(Settings.ACTION_SETTINGS))
        }
    }

    private fun openTetheringSettings() {
        try {
            val intent = Intent(Settings.ACTION_WIRELESS_SETTINGS)
            startActivity(intent)
        } catch (e: Exception) {
            startActivity(Intent(Settings.ACTION_SETTINGS))
        }
    }
}