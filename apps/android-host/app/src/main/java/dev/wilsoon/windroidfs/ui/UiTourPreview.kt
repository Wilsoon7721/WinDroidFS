package dev.wilsoon.windroidfs.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

// ---------------------------------------------------------------------------
// PREVIEW ONLY. Not used by the app.
// ---------------------------------------------------------------------------

private enum class TourScreen { ONBOARDING, DASHBOARD, SETTINGS }

private enum class FakeConnection { IDLE, CONNECTED, TRANSFERRING }

@Composable
private fun UiTour(dark: Boolean) {
    WinDroidTheme(darkTheme = dark) {
        var screen by remember { mutableStateOf(TourScreen.ONBOARDING) }
        var expanded by remember { mutableStateOf(ConnectionMethod.AOA) }
        var connection by remember { mutableStateOf(FakeConnection.IDLE) }
        var settings by remember { mutableStateOf(SettingsUiState()) }

        Box(modifier = Modifier.fillMaxSize()) {
            when (screen) {
                TourScreen.ONBOARDING -> OnboardingScreen(
                    onOpenAccessSettings = { screen = TourScreen.DASHBOARD },
                    onCheckAgain = { screen = TourScreen.DASHBOARD }
                )

                TourScreen.DASHBOARD -> MainScreen(
                    state = DashboardUiState(
                        expandedMethod = expanded,
                        localIpAddress = "192.168.1.23",
                        tcpPort = settings.tcpPort,
                        session = fakeSession(connection)
                    ),
                    onMethodSelected = { expanded = it },
                    onOpenSettings = { screen = TourScreen.SETTINGS },
                    onOpenDeveloperOptions = {},
                    onOpenTetheringSettings = {},
                    onTerminateConnection = { connection = FakeConnection.IDLE }
                )

                TourScreen.SETTINGS -> SettingsScreen(
                    state = settings,
                    onBack = { screen = TourScreen.DASHBOARD },
                    onStorageScopeChange = { settings = settings.copy(storageScope = it) },
                    onFolderToggle = { f, on ->
                        settings = settings.copy(exposedFolders = settings.exposedFolders + (f to on))
                    },
                    onRequireBiometricsChange = { settings = settings.copy(requireBiometrics = it) },
                    onKeepScreenAwakeChange = { settings = settings.copy(keepScreenAwake = it) },
                    onForegroundServiceChange = { settings = settings.copy(runAsForegroundService = it) },
                    onPortChange = { settings = settings.copy(tcpPort = it) }
                )
            }

            if (screen == TourScreen.DASHBOARD) {
                AssistChip(
                    onClick = {
                        connection = FakeConnection.entries[(connection.ordinal + 1) % FakeConnection.entries.size]
                    },
                    label = { Text("Demo: ${connection.name.lowercase()}") },
                    modifier = Modifier
                        .align(Alignment.BottomStart)
                        .navigationBarsPadding()
                        .padding(8.dp)
                )
            }
        }
    }
}

private fun fakeSession(connection: FakeConnection): ActiveSession? = when (connection) {
    FakeConnection.IDLE -> null
    FakeConnection.CONNECTED, FakeConnection.TRANSFERRING -> ActiveSession(
        method = ConnectionMethod.AOA,
        hostName = "DESKTOP-WILSOON",
        uptimeSeconds = 754,
        readBytesPerSecond = 88_300_000,
        writeBytesPerSecond = if (connection == FakeConnection.TRANSFERRING) 41_200_000 else 0,
        totalBytesTransferred = 6_420_000_000,
        activeTransfer = if (connection == FakeConnection.TRANSFERRING) {
            TransferInProgress("VID_20261014_183012.mp4", 2_900_000_000, 4_200_000_000)
        } else null
    )
}

@Preview(name = "UI Tour - Light", showSystemUi = true)
@Composable
private fun UiTourLightPreview() = UiTour(dark = false)

@Preview(name = "UI Tour - Dark", showSystemUi = true)
@Composable
private fun UiTourDarkPreview() = UiTour(dark = true)
