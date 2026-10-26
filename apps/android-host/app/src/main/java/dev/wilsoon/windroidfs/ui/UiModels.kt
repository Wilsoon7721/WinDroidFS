package dev.wilsoon.windroidfs.ui

enum class ConnectionMethod {
    NONE, AOA, ADB, TETHERING, WIFI
}

data class DashboardUiState(
    // Which setup card is open while idle
    val expandedMethod: ConnectionMethod = ConnectionMethod.AOA,
    // Shown in the Wi-Fi / Tethering cards, e.g. "192.168.1.23"
    val localIpAddress: String? = null,
    val tcpPort: Int = 9999,
    // null = idle (show the setup cards), non-null = a PC is connected (show live dashboard)
    val session: ActiveSession? = null
)

// A live connection to the PC
data class ActiveSession(
    val method: ConnectionMethod,
    val hostName: String,
    val uptimeSeconds: Long,
    val readBytesPerSecond: Long,
    val writeBytesPerSecond: Long,
    val totalBytesTransferred: Long,
    // null = nothing being written right now (safe to terminate without a warning).
    val activeTransfer: TransferInProgress? = null,
    val usbLink: UsbLink = UsbLink.UNKNOWN
)

// [maxSpeed] is set for links slower than USB 3. Same words as speed_advice in session.rs.
enum class UsbLink(val label: String, val maxSpeed: String?) {
    UNKNOWN("Unknown", null),
    USB_1_0("USB 1.0", "0.2 MB/s"),
    USB_1_1("USB 1.1", "1.5 MB/s"),
    USB_2_0("USB 2.0", "60 MB/s"),
    USB_3_0("USB 3.0", null),
    USB_3_1("USB 3.1+", null);

    val isSlow: Boolean get() = maxSpeed != null

    val advice: String
        get() = if (maxSpeed != null) {
            "Maxes out at $maxSpeed, use a USB 3.0+ port or supported cable to increase this"
        } else {
            "Fast enough for full-speed transfers"
        }
}

data class TransferInProgress(
    val fileName: String,
    val bytesDone: Long,
    val bytesTotal: Long
)

enum class StorageScope {
    ENTIRE_STORAGE, SELECTED_FOLDERS
}

data class SettingsUiState(
    val storageScope: StorageScope = StorageScope.SELECTED_FOLDERS,
    // Folder name -> is it exposed to the PC? Only used when scope = SELECTED_FOLDERS
    val exposedFolders: Map<String, Boolean> = linkedMapOf(
        "DCIM" to true,
        "Download" to true,
        "Documents" to false,
        "Pictures" to false,
        "Movies" to false,
        "Music" to false
    ),
    val requireBiometrics: Boolean = false,
    val keepScreenAwake: Boolean = false,
    val runAsForegroundService: Boolean = true,
    val tcpPort: Int = 9999
)

fun ConnectionMethod.displayName(): String = when (this) {
    ConnectionMethod.AOA -> "Direct USB"
    ConnectionMethod.ADB -> "ADB"
    ConnectionMethod.TETHERING -> "USB Tethering"
    ConnectionMethod.WIFI -> "Wi-Fi"
    ConnectionMethod.NONE -> "None"
}

fun formatBytes(bytes: Long): String {
    val units = listOf("B", "KB", "MB", "GB", "TB")
    var value = bytes.toDouble()
    var unit = 0
    while (value >= 1024 && unit < units.lastIndex) {
        value /= 1024
        unit++
    }
    return if (unit == 0) "$bytes B" else "%.1f %s".format(value, units[unit])
}

fun formatSpeed(bytesPerSecond: Long): String =
    "%.1f".format(bytesPerSecond / (1024.0 * 1024.0))

fun formatDuration(seconds: Long): String {
    val h = seconds / 3600
    val m = (seconds % 3600) / 60
    val s = seconds % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%02d:%02d".format(m, s)
}
