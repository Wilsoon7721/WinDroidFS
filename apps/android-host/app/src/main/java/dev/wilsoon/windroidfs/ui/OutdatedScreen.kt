package dev.wilsoon.windroidfs.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

/**
 * The PC found this app out of date (it decides, from the version the app reports). [required]: the PC won't connect
 * until it's updated; otherwise it still works and updating is a suggestion.
 */
@Composable
fun OutdatedScreen(
    required: Boolean,
    currentVersion: String,
    latestVersion: String,
    pcName: String,
    onUpdate: () -> Unit,
    onClose: () -> Unit,
) {
    val pc = pcName.ifEmpty { "Your PC" }
    val latest = latestVersion.ifEmpty { "a newer version" }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(WinDroidTheme.colors.background)
            .systemBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Spacer(modifier = Modifier.height(32.dp))
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(88.dp)
                .clip(CircleShape)
                .background(WinDroidTheme.colors.surface)
        ) {
            Icon(
                imageVector = if (required) Icons.Filled.Warning else Icons.Filled.Refresh,
                contentDescription = null,
                tint = WinDroidTheme.colors.brand,
                modifier = Modifier.size(40.dp)
            )
        }
        Spacer(modifier = Modifier.height(24.dp))
        Text(
            text = if (required) "Update required" else "Update available",
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 26.sp,
            fontWeight = FontWeight.Bold,
            textAlign = TextAlign.Center
        )
        Spacer(modifier = Modifier.height(12.dp))
        Text(
            text = if (required) {
                "$pc needs WinDroidFS $latest or later on this phone, and won't connect until it's updated."
            } else {
                "WinDroidFS $latest is out, with fixes and improvements. $pc still works with this version meanwhile."
            },
            color = WinDroidTheme.colors.textSecondary,
            fontSize = 15.sp,
            textAlign = TextAlign.Center,
            lineHeight = 22.sp
        )
        Spacer(modifier = Modifier.height(32.dp))
        WinDroidCard {
            VersionRow("On this phone", currentVersion)
            Spacer(modifier = Modifier.height(12.dp))
            VersionRow("Latest", latestVersion.ifEmpty { "Newer" })
        }
        Spacer(modifier = Modifier.height(16.dp))
        Box(
            modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp))
                .background(WinDroidTheme.colors.surfaceVariant).padding(16.dp)
        ) {
            Text(
                text = "Update it from the app store you installed it with, such as F-Droid or IzzyOnDroid. If you installed it from GitHub, the latest APK is on its releases page.",
                color = WinDroidTheme.colors.textSecondary,
                fontSize = 13.sp,
                lineHeight = 19.sp
            )
        }
        Spacer(modifier = Modifier.height(32.dp))
        Button(
            onClick = onUpdate,
            shape = RoundedCornerShape(14.dp),
            contentPadding = PaddingValues(vertical = 16.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = WinDroidTheme.colors.brand,
                contentColor = WinDroidTheme.colors.onAccent
            ),
            modifier = Modifier.fillMaxWidth()
        ) {
            Text(text = "Update", fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        }
        Spacer(modifier = Modifier.height(8.dp))
        TextButton(onClick = onClose, modifier = Modifier.fillMaxWidth()) {
            Text(
                text = if (required) "Close" else "Not now",
                color = WinDroidTheme.colors.textSecondary
            )
        }
    }
}

@Composable
private fun VersionRow(label: String, version: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(text = label, color = WinDroidTheme.colors.textSecondary, fontSize = 15.sp)
        Text(
            text = version,
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold
        )
    }
}

@Preview(name = "Outdated - Light", showSystemUi = true)
@Composable
private fun OutdatedLightPreview() {
    WinDroidTheme(darkTheme = false) {
        OutdatedScreen(false, "1.1.0", "1.2.0", "WILSON-PC", onUpdate = {}, onClose = {})
    }
}

@Preview(name = "Outdated, required - Dark", showSystemUi = true)
@Composable
private fun OutdatedRequiredDarkPreview() {
    WinDroidTheme(darkTheme = true) {
        OutdatedScreen(true, "1.1.0", "1.3.0", "WILSON-PC", onUpdate = {}, onClose = {})
    }
}
