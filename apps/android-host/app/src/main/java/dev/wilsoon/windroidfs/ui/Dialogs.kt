package dev.wilsoon.windroidfs.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
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

// Pairing a PC over Wi-Fi by comparing a code on both screens
@Composable
fun PairingDialog(pairing: PairingUi, onAnswer: (Boolean) -> Unit, onDismiss: () -> Unit) {
    when (pairing) {
        PairingUi.Waiting -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text(text = "Pair a new PC") },
            text = {
                Text(text = "On your PC, choose \"Pair a phone over Wi-Fi\" in WinDroidFS's tray menu. Both need to be on the same Wi-Fi.")
            },
            confirmButton = { TextButton(onClick = onDismiss) { Text(text = "Cancel") } },
            containerColor = WinDroidTheme.colors.surface,
            titleContentColor = WinDroidTheme.colors.textPrimary,
            textContentColor = WinDroidTheme.colors.textSecondary
        )

        is PairingUi.Confirm -> AlertDialog(
            onDismissRequest = {},
            title = { Text(text = "Pair with ${pairing.pcName}?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(
                        text = if (pairing.answered) "Waiting for the PC…"
                        else "Only pair if your PC shows this same code:"
                    )
                    Text(
                        text = pairing.code,
                        color = WinDroidTheme.colors.textPrimary,
                        fontSize = 34.sp,
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.Bold
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { onAnswer(true) }, enabled = !pairing.answered) {
                    Text(text = "Pair", color = WinDroidTheme.colors.brand, fontWeight = FontWeight.SemiBold)
                }
            },
            dismissButton = {
                TextButton(onClick = { onAnswer(false) }, enabled = !pairing.answered) {
                    Text(text = "Codes differ")
                }
            },
            containerColor = WinDroidTheme.colors.surface,
            titleContentColor = WinDroidTheme.colors.textPrimary,
            textContentColor = WinDroidTheme.colors.textSecondary
        )

        is PairingUi.Finished -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text(text = if (pairing.paired) "Paired" else "Not paired") },
            text = { Text(text = pairing.message) },
            confirmButton = { TextButton(onClick = onDismiss) { Text(text = "OK") } },
            containerColor = WinDroidTheme.colors.surface,
            titleContentColor = WinDroidTheme.colors.textPrimary,
            textContentColor = WinDroidTheme.colors.textSecondary
        )
    }
}

@Preview(name = "Pairing Dialog - Code")
@Composable
private fun PairingDialogPreview() {
    WinDroidTheme(darkTheme = false) {
        PairingDialog(pairing = PairingUi.Confirm("WILSON-PC", "565 043"), onAnswer = {}, onDismiss = {})
    }
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
