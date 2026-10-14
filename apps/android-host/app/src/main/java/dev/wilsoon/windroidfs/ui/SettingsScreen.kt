package dev.wilsoon.windroidfs.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

// Settings screen
// Current sections: Storage Scope, Security, Service, Network

@Composable
fun SettingsScreen(
    state: SettingsUiState,
    onBack: () -> Unit,
    onStorageScopeChange: (StorageScope) -> Unit,
    onFolderToggle: (folder: String, exposed: Boolean) -> Unit,
    onRequireBiometricsChange: (Boolean) -> Unit,
    onKeepScreenAwakeChange: (Boolean) -> Unit,
    onForegroundServiceChange: (Boolean) -> Unit,
    // Only called with a port that passed validation (1024-65535)
    onPortChange: (Int) -> Unit
) {
    BackHandler(onBack = onBack)

    Column(modifier = Modifier.fillMaxSize().background(WinDroidTheme.colors.background).systemBarsPadding()) {
        SettingsTopBar(onBack)

        Column(
            modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())
                .padding(start = 24.dp, end = 24.dp, top = 8.dp, bottom = 32.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)
        ) {
            SectionLabel("Storage Scope")
            StorageScopeCard(state, onStorageScopeChange, onFolderToggle)

            SectionLabel("Security", modifier = Modifier.padding(top = 12.dp))
            WinDroidCard {
                SettingsSwitchRow(
                    title = "Require biometrics for PC connection",
                    subtitle = "Even when a PC connects, the drive will remain locked until you confirm with your fingerprint.",
                    checked = state.requireBiometrics,
                    onCheckedChange = onRequireBiometricsChange
                )
            }

            SectionLabel("Service", modifier = Modifier.padding(top = 12.dp))
            WinDroidCard {
                SettingsSwitchRow(
                    title = "Run as foreground service",
                    subtitle = "Pins a notification so Android doesn't kill the connection in the background.",
                    checked = state.runAsForegroundService,
                    onCheckedChange = onForegroundServiceChange
                )
                SettingsDivider()
                SettingsSwitchRow(
                    title = "Keep screen awake",
                    subtitle = "Some phones drop USB connections when the screen turns off.",
                    checked = state.keepScreenAwake,
                    onCheckedChange = onKeepScreenAwakeChange
                )
            }

            SectionLabel("Network", modifier = Modifier.padding(top = 12.dp))
            WinDroidCard {
                PortField(currentPort = state.tcpPort, onPortChange = onPortChange)
            }
        }
    }
}

@Composable
private fun SettingsTopBar(onBack: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().padding(start = 8.dp, end = 24.dp, top = 16.dp, bottom = 8.dp)
    ) {
        IconButton(onClick = onBack) {
            Icon(
                imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                contentDescription = "Back",
                tint = WinDroidTheme.colors.textPrimary
            )
        }
        Spacer(modifier = Modifier.width(4.dp))
        Text(
            text = "Settings",
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 26.sp,
            fontWeight = FontWeight.Bold
        )
    }
}

// Two radio options. "Selected folders" will reveal the folder checklist */
@Composable
private fun StorageScopeCard(
    state: SettingsUiState,
    onStorageScopeChange: (StorageScope) -> Unit,
    onFolderToggle: (String, Boolean) -> Unit
) {
    WinDroidCard {
        ScopeOption(
            title = "Selected folders only",
            subtitle = "Recommended. The PC only sees the folders you tick below.",
            selected = state.storageScope == StorageScope.SELECTED_FOLDERS,
            onClick = { onStorageScopeChange(StorageScope.SELECTED_FOLDERS) }
        )

        AnimatedVisibility(visible = state.storageScope == StorageScope.SELECTED_FOLDERS) {
            Column(
                modifier = Modifier.padding(start = 40.dp, top = 4.dp, bottom = 8.dp).fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp)).background(WinDroidTheme.colors.surfaceVariant)
                    .padding(vertical = 4.dp)
            ) {
                state.exposedFolders.forEach { (folder, exposed) ->
                    FolderCheckRow(folder = folder, checked = exposed, onCheckedChange = { onFolderToggle(folder, it) })
                }
            }
        }

        SettingsDivider()
        ScopeOption(
            title = "Entire internal storage",
            subtitle = "The PC can see every file, including other apps' folders.",
            selected = state.storageScope == StorageScope.ENTIRE_STORAGE,
            onClick = { onStorageScopeChange(StorageScope.ENTIRE_STORAGE) },
            isWarning = true
        )
    }
}

@Composable
private fun ScopeOption(
    title: String,
    subtitle: String,
    selected: Boolean,
    onClick: () -> Unit,
    isWarning: Boolean = false
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .clickable(onClick = onClick)
            .padding(vertical = 8.dp)
    ) {
        RadioButton(selected = selected, onClick = onClick)
        Spacer(modifier = Modifier.width(4.dp))
        Column {
            Text(
                text = title,
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 16.sp,
                fontWeight = FontWeight.Medium
            )
            Text(
                text = subtitle,
                color = if (isWarning && selected) WinDroidTheme.colors.warning else WinDroidTheme.colors.textSecondary,
                fontSize = 13.sp,
                lineHeight = 18.sp
            )
        }
    }
}

@Composable
private fun FolderCheckRow(folder: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().clickable { onCheckedChange(!checked) }.padding(horizontal = 4.dp)
    ) {
        Checkbox(checked = checked, onCheckedChange = onCheckedChange)
        Text(
            text = "/$folder",
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 14.sp,
            fontFamily = FontFamily.Monospace
        )
    }
}

// Title + explanation on the left with a switch on the right
@Composable
private fun SettingsSwitchRow(
    title: String,
    subtitle: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).clickable { onCheckedChange(!checked) }
            .padding(vertical = 4.dp)
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = title,
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 16.sp,
                fontWeight = FontWeight.Medium
            )
            Spacer(modifier = Modifier.height(2.dp))
            Text(
                text = subtitle,
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 13.sp,
                lineHeight = 18.sp
            )
        }
        Spacer(modifier = Modifier.width(16.dp))
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

// TCP port input
@Composable
private fun PortField(currentPort: Int, onPortChange: (Int) -> Unit) {
    var text by remember(currentPort) { mutableStateOf(currentPort.toString()) }
    val parsed = text.toIntOrNull()
    val isValid = parsed != null && parsed in 1024..65535

    Text(
        text = "TCP port (Wi-Fi & Tethering)",
        color = WinDroidTheme.colors.textPrimary,
        fontSize = 16.sp,
        fontWeight = FontWeight.Medium
    )
    Spacer(modifier = Modifier.height(12.dp))
    OutlinedTextField(
        value = text,
        onValueChange = { input ->
            // Digits only, max 5 characters.
            val cleaned = input.filter { it.isDigit() }.take(5)
            text = cleaned
            cleaned.toIntOrNull()?.takeIf { it in 1024..65535 }?.let(onPortChange)
        },
        singleLine = true,
        isError = !isValid,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        textStyle = LocalTextStyle.current.copy(fontFamily = FontFamily.Monospace),
        supportingText = {
            Text(text = if (isValid) "Default is 9999. Change only if another app uses it." else "Enter a port between 1024 and 65535.")
        },
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth()
    )
}

@Composable
private fun SettingsDivider() {
    HorizontalDivider(
        color = WinDroidTheme.colors.border,
        thickness = 1.dp,
        modifier = Modifier.padding(vertical = 8.dp)
    )
}

@Composable
private fun PreviewSettings(dark: Boolean) {
    WinDroidTheme(darkTheme = dark) {
        var state by remember { mutableStateOf(SettingsUiState()) }
        SettingsScreen(
            state = state,
            onBack = {},
            onStorageScopeChange = { state = state.copy(storageScope = it) },
            onFolderToggle = { folder, on ->
                state = state.copy(exposedFolders = state.exposedFolders + (folder to on))
            },
            onRequireBiometricsChange = { state = state.copy(requireBiometrics = it) },
            onKeepScreenAwakeChange = { state = state.copy(keepScreenAwake = it) },
            onForegroundServiceChange = { state = state.copy(runAsForegroundService = it) },
            onPortChange = { state = state.copy(tcpPort = it) }
        )
    }
}

@Preview(name = "Settings - Light", showSystemUi = true)
@Composable
private fun SettingsLightPreview() = PreviewSettings(dark = false)

@Preview(name = "Settings - Dark", showSystemUi = true)
@Composable
private fun SettingsDarkPreview() = PreviewSettings(dark = true)
