package dev.wilsoon.windroidfs.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

@Composable
fun MainScreen(
    state: DashboardUiState,
    onMethodSelected: (ConnectionMethod) -> Unit,
    onOpenSettings: () -> Unit,
    onOpenDeveloperOptions: () -> Unit,
    onOpenTetheringSettings: () -> Unit,
    onTerminateConnection: () -> Unit
) {
    var showTerminateDialog by remember { mutableStateOf(false) }

    Column(modifier = Modifier.fillMaxSize().background(WinDroidTheme.colors.background).systemBarsPadding()) {
        DashboardHeader(
            session = state.session,
            onOpenSettings = onOpenSettings,
            modifier = Modifier.padding(start = 24.dp, end = 12.dp, top = 24.dp, bottom = 8.dp)
        )

        // Cross-fades between idle and active. contentKey makes it animate only when switching idle <-> active, not every time a speed number ticks
        AnimatedContent(
            targetState = state.session,
            contentKey = { it != null },
            transitionSpec = { fadeIn() togetherWith fadeOut() },
            label = "dashboardMode"
        ) { session ->
            if (session == null) {
                IdleContent(
                    state = state,
                    onMethodSelected = onMethodSelected,
                    onOpenDeveloperOptions = onOpenDeveloperOptions,
                    onOpenTetheringSettings = onOpenTetheringSettings
                )
            } else {
                ActiveDashboard(
                    session = session,
                    onTerminateClick = {
                        // Only interrupt if something may get corrupted
                        if (session.activeTransfer != null) showTerminateDialog = true
                        else onTerminateConnection()
                    }
                )
            }
        }
    }

    val transfer = state.session?.activeTransfer
    if (showTerminateDialog && transfer != null) {
        TerminateConnectionDialog(
            fileName = transfer.fileName,
            onConfirm = {
                showTerminateDialog = false
                onTerminateConnection()
            },
            onDismiss = { showTerminateDialog = false }
        )
    }
}

@Composable
private fun DashboardHeader(session: ActiveSession?, onOpenSettings: () -> Unit, modifier: Modifier = Modifier) {
    Row(modifier = modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) {
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                text = "WinDroidFS",
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 32.sp,
                fontWeight = FontWeight.Bold
            )
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StatusDot(
                    color = if (session != null) WinDroidTheme.colors.success else WinDroidTheme.colors.warning,
                    size = 12.dp
                )
                Text(
                    text = if (session != null) "Connected to ${session.hostName}" else "Waiting for connection...",
                    color = WinDroidTheme.colors.textSecondary,
                    fontSize = 16.sp
                )
            }
        }
        IconButton(onClick = onOpenSettings) {
            Icon(
                imageVector = Icons.Filled.Settings,
                contentDescription = "Settings",
                tint = WinDroidTheme.colors.textSecondary
            )
        }
    }
}

@Composable
private fun IdleContent(
    state: DashboardUiState,
    onMethodSelected: (ConnectionMethod) -> Unit,
    onOpenDeveloperOptions: () -> Unit,
    onOpenTetheringSettings: () -> Unit
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 24.dp, end = 24.dp, top = 16.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp)
    ) {
        item { SectionLabel("Pick any of these methods:") }

        // Direct USB
        item {
            ExpandableMethodCard(
                title = "Direct USB",
                description = "Simply connect a USB cable to your device. No setup required.",
                isExpanded = state.expandedMethod == ConnectionMethod.AOA,
                onClick = { onMethodSelected(ConnectionMethod.AOA) }
            ) {
                AoaSetupContent()
            }
        }

        // ADB
        item {
            ExpandableMethodCard(
                title = "ADB Forwarding",
                description = "For developers. Requires Developer Options to be enabled.",
                isExpanded = state.expandedMethod == ConnectionMethod.ADB,
                onClick = { onMethodSelected(ConnectionMethod.ADB) }) {
                AdbSetupContent(onOpenDeveloperOptions)
            }
        }

        // USB Tethering
        item {
            ExpandableMethodCard(
                title = "USB Tethering",
                description = "Alternative if direct USB does not work. Windows will connect to your phone as a hotspot, and this may consume some mobile data.",
                isExpanded = state.expandedMethod == ConnectionMethod.TETHERING,
                onClick = { onMethodSelected(ConnectionMethod.TETHERING) }
            ) {
                TetheringSetupContent(state.tcpPort, onOpenTetheringSettings)
            }
        }

        // Local Wi-Fi
        item {
            ExpandableMethodCard(
                title = "Local Wi-Fi",
                description = "Wireless transfer via local router.",
                isExpanded = state.expandedMethod == ConnectionMethod.WIFI,
                onClick = { onMethodSelected(ConnectionMethod.WIFI) }) {
                WifiSetupContent(state.localIpAddress, state.tcpPort)
            }
        }
    }
}

@Composable
private fun AoaSetupContent() {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        SetupStep(1, "Plug your phone into the PC with a USB cable.")
        SetupStep(2, "Start WinDroidFS on Windows.")
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 4.dp)) {
            CircularProgressIndicator(
                modifier = Modifier.size(20.dp),
                color = WinDroidTheme.colors.brand,
                strokeWidth = 2.dp
            )
            Spacer(modifier = Modifier.width(12.dp))
            Text(
                text = "Waiting for WinDroidFS on Windows...",
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 14.sp
            )
        }
    }
}

@Composable
private fun AdbSetupContent(onOpenDeveloperOptions: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        SetupStep(1, "Enable Developer Options (Settings > About phone > tap Build number 7 times).")
        SetupStep(2, "Turn on USB Debugging.")
        SetupStep(3, "Plug in, then accept the \"Allow USB debugging?\" prompt.")
        SetupActionButton(text = "Open Developer Options", onClick = onOpenDeveloperOptions)
    }
}

@Composable
private fun TetheringSetupContent(port: Int, onOpenTetheringSettings: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        SetupStep(1, "Plug your phone into the PC with a USB cable.")
        SetupStep(2, "Turn on USB Tethering in Hotspot & Tethering settings.")
        SetupStep(3, "WinDroidFS will listen on port $port.")
        SetupActionButton(text = "Open Tethering Settings", onClick = onOpenTetheringSettings)
    }
}

@Composable
private fun WifiSetupContent(ipAddress: String?, port: Int) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        SetupStep(1, "Connect your phone and PC to the same Wi-Fi network.")
        SetupStep(2, "Your PC should discover this phone automatically.")
        // Shown so the user can type it in manually if mDNS discovery fails
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp))
                .background(WinDroidTheme.colors.surfaceVariant).padding(horizontal = 14.dp, vertical = 12.dp)
        ) {
            Text(
                text = "Address",
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 13.sp,
                modifier = Modifier.weight(1f)
            )
            Text(
                text = if (ipAddress != null) "$ipAddress:$port" else "Not on Wi-Fi",
                color = if (ipAddress != null) WinDroidTheme.colors.textPrimary else WinDroidTheme.colors.warning,
                fontSize = 14.sp,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.SemiBold
            )
        }
    }
}

// Used inside setup cards to jump into Android settings
@Composable
private fun SetupActionButton(text: String, onClick: () -> Unit) {
    OutlinedButton(
        onClick = onClick,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, WinDroidTheme.colors.brand),
        modifier = Modifier.fillMaxWidth().padding(top = 4.dp)
    ) {
        Text(text = text, color = WinDroidTheme.colors.brand, fontWeight = FontWeight.SemiBold)
    }
}

@Composable
private fun ActiveDashboard(session: ActiveSession, onTerminateClick: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())
            .padding(start = 24.dp, end = 24.dp, top = 16.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp)
    ) {
        ProtocolBanner(session)

        SectionLabel("Live Throughput", modifier = Modifier.padding(top = 8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            SpeedTile(
                label = "Read",
                bytesPerSecond = session.readBytesPerSecond,
                modifier = Modifier.weight(1f)
            )
            SpeedTile(
                label = "Write",
                bytesPerSecond = session.writeBytesPerSecond,
                modifier = Modifier.weight(1f)
            )
        }
        SessionTotalCard(session.totalBytesTransferred)

        session.activeTransfer?.let { CurrentTransferCard(it) }

        Spacer(modifier = Modifier.height(8.dp))
        TerminateButton(onClick = onTerminateClick)
    }
}

@Composable
private fun ProtocolBanner(session: ActiveSession) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(WinDroidTheme.colors.success.copy(alpha = 0.12f))
            .padding(20.dp)
    ) {
        StatusDot(color = WinDroidTheme.colors.success, size = 12.dp)
        Spacer(modifier = Modifier.width(14.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = "${session.method.displayName()} Active",
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold
            )
            Text(
                text = "Mounted on ${session.hostName}",
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 14.sp
            )
        }
        Text(
            text = formatDuration(session.uptimeSeconds),
            color = WinDroidTheme.colors.success,
            fontSize = 16.sp,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.SemiBold
        )
    }
}

// Used to show the read/write speed
@Composable
private fun SpeedTile(label: String, bytesPerSecond: Long, modifier: Modifier = Modifier) {
    WinDroidCard(modifier = modifier) {
        Text(text = label, color = WinDroidTheme.colors.textSecondary, fontSize = 14.sp)
        Spacer(modifier = Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.Bottom) {
            Text(
                text = formatSpeed(bytesPerSecond),
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 32.sp,
                fontWeight = FontWeight.Bold
            )
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = "MB/s",
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 14.sp,
                modifier = Modifier.padding(bottom = 6.dp)
            )
        }
    }
}

@Composable
private fun SessionTotalCard(totalBytes: Long) {
    WinDroidCard {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "Transferred this session",
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 14.sp,
                modifier = Modifier.weight(1f)
            )
            Text(
                text = formatBytes(totalBytes),
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 20.sp,
                fontWeight = FontWeight.Bold
            )
        }
    }
}

// File name + progress bar for whatever is being written right now.
@Composable
private fun CurrentTransferCard(transfer: TransferInProgress) {
    val progress = if (transfer.bytesTotal > 0) transfer.bytesDone.toFloat() / transfer.bytesTotal else 0f
    WinDroidCard {
        Text(text = "Writing", color = WinDroidTheme.colors.textSecondary, fontSize = 14.sp)
        Spacer(modifier = Modifier.height(4.dp))
        Text(
            text = transfer.fileName,
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 16.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1
        )
        Spacer(modifier = Modifier.height(12.dp))
        LinearProgressIndicator(
            progress = { progress },
            color = WinDroidTheme.colors.brand,
            trackColor = WinDroidTheme.colors.surfaceVariant,
            modifier = Modifier
                .fillMaxWidth()
                .height(6.dp)
                .clip(RoundedCornerShape(3.dp))
        )
        Spacer(modifier = Modifier.height(8.dp))
        Text(
            text = "${formatBytes(transfer.bytesDone)} of ${formatBytes(transfer.bytesTotal)}",
            color = WinDroidTheme.colors.textSecondary,
            fontSize = 13.sp
        )
    }
}

@Composable
private fun TerminateButton(onClick: () -> Unit) {
    Button(
        onClick = onClick,
        shape = RoundedCornerShape(14.dp),
        contentPadding = PaddingValues(vertical = 18.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = WinDroidTheme.colors.error,
            contentColor = WinDroidTheme.colors.onAccent
        ),
        modifier = Modifier.fillMaxWidth()
    ) {
        Text(text = "Terminate Connection", fontSize = 17.sp, fontWeight = FontWeight.Bold)
    }
}

private val previewIdleState = DashboardUiState(
    expandedMethod = ConnectionMethod.ADB,
    localIpAddress = "192.168.1.23"
)

private val previewActiveState = DashboardUiState(
    session = ActiveSession(
        method = ConnectionMethod.AOA,
        hostName = "DESKTOP-WILSOON",
        uptimeSeconds = 754,
        readBytesPerSecond = 88_300_000,
        writeBytesPerSecond = 41_200_000,
        totalBytesTransferred = 6_420_000_000,
        activeTransfer = TransferInProgress(
            fileName = "VID_20261014_183012.mp4",
            bytesDone = 2_900_000_000,
            bytesTotal = 4_200_000_000
        )
    )
)

@Composable
private fun PreviewMainScreen(state: DashboardUiState, dark: Boolean) {
    WinDroidTheme(darkTheme = dark) {
        var expanded by remember { mutableStateOf(state.expandedMethod) }
        MainScreen(
            state = state.copy(expandedMethod = expanded),
            onMethodSelected = { expanded = it },
            onOpenSettings = {},
            onOpenDeveloperOptions = {},
            onOpenTetheringSettings = {},
            onTerminateConnection = {}
        )
    }
}

@Preview(name = "Dashboard Idle - Light", showSystemUi = true)
@Composable
private fun DashboardIdleLightPreview() = PreviewMainScreen(previewIdleState, dark = false)

@Preview(name = "Dashboard Idle - Dark", showSystemUi = true)
@Composable
private fun DashboardIdleDarkPreview() = PreviewMainScreen(previewIdleState, dark = true)

@Preview(name = "Dashboard Active - Light", showSystemUi = true)
@Composable
private fun DashboardActiveLightPreview() = PreviewMainScreen(previewActiveState, dark = false)

@Preview(name = "Dashboard Active - Dark", showSystemUi = true)
@Composable
private fun DashboardActiveDarkPreview() = PreviewMainScreen(previewActiveState, dark = true)
