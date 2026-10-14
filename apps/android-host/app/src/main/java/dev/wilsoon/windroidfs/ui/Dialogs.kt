package dev.wilsoon.windroidfs.ui

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

// Terminate connection dialog while file is being written
@Composable
fun TerminateConnectionDialog(fileName: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = {
            Icon(imageVector = Icons.Filled.Warning, contentDescription = null, tint = WinDroidTheme.colors.error)
        },
        title = { Text(text = "Interrupt Transfer?") },
        text = {
            Text(text = "\"$fileName\" is currently being written. Disconnecting now may corrupt the file. Are you sure?")
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(text = "Disconnect", color = WinDroidTheme.colors.error, fontWeight = FontWeight.SemiBold)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(text = "Keep Transferring")
            }
        },
        containerColor = WinDroidTheme.colors.surface,
        titleContentColor = WinDroidTheme.colors.textPrimary,
        textContentColor = WinDroidTheme.colors.textSecondary
    )
}

@Preview(name = "Terminate Connection Dialog - Light")
@Composable
private fun TerminateConnectionDialogLightPreview() {
    WinDroidTheme(darkTheme = false) {
        TerminateConnectionDialog(fileName = "VID_20261014_183012.mp4", onConfirm = {}, onDismiss = {})
    }
}

@Preview(name = "Terminate Connection Dialog - Dark")
@Composable
private fun TerminateConnectionDialogDarkPreview() {
    WinDroidTheme(darkTheme = true) {
        TerminateConnectionDialog(fileName = "VID_20261014_183012.mp4", onConfirm = {}, onDismiss = {})
    }
}
