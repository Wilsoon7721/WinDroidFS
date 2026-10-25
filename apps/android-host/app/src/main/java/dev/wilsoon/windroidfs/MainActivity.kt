package dev.wilsoon.windroidfs

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.core.content.ContextCompat
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.*
import dev.wilsoon.windroidfs.ui.*
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.ParcelFileDescriptor
import android.util.Log
import java.io.FileInputStream
import java.io.FileOutputStream
import dev.wilsoon.windroidfs.protocol.WinDroidProto
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.IntentFilter

enum class AppScreen {
    ONBOARDING, DASHBOARD, SETTINGS
}

private const val ACTION_USB_PERMISSION = "dev.wilsoon.windroidfs.USB_PERMISSION"

class MainActivity : ComponentActivity() {
    private var fileDescriptor: ParcelFileDescriptor? = null
    private var inputStream: FileInputStream? = null
    private var outputStream: FileOutputStream? = null
    private var isConnected = false

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleUsbIntent(intent)
    }

    private fun handleUsbIntent(intent: Intent) {
        if (intent.action == UsbManager.ACTION_USB_ACCESSORY_ATTACHED) {
            val usbManager = getSystemService(USB_SERVICE) as UsbManager
            val accessory: UsbAccessory? = intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)

            if (accessory != null) {
                try {
                    fileDescriptor = usbManager.openAccessory(accessory)
                    if (fileDescriptor != null) {
                        val fd = fileDescriptor!!.fileDescriptor
                        inputStream = FileInputStream(fd)
                        outputStream = FileOutputStream(fd)
                        isConnected = true

                        Thread {
                            listenForPackets()
                        }.start()

                        Log.d("WinDroid", "Successfully connected to Windows Daemon!")
                    }
                } catch (e: Exception) {
                    Log.e("WinDroid", "Error opening accessory: ${e.message}")
                }
            }
        }
    }

    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (ACTION_USB_PERMISSION == intent.action) {
                synchronized(this) {
                    val accessory: UsbAccessory? = intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
                    if (intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)) {
                        accessory?.let { openAccessory(it) }
                    } else {
                        Log.d("WinDroid", "Permission denied for accessory")
                    }
                }
            }
        }
    }

    private fun listenForPackets() {
        val buffer = ByteArray(16384)

        while (isConnected && inputStream != null) {
            try {
                val bytesRead = inputStream!!.read(buffer)

                if (bytesRead > 0) {
                    val packetData = buffer.copyOfRange(0, bytesRead)
                    val packet = WinDroidProto.Packet.parseFrom(packetData)

                    if (packet.hasPing()) {
                        val pingTimestamp = packet.ping.timestampMs
                        android.util.Log.d("WinDroid", "✅ RECEIVED PING! Timestamp: $pingTimestamp")

                        val pongMsg = WinDroidProto.Pong.newBuilder()
                            .setTimestampMs(pingTimestamp)
                            .build()

                        val responsePacket = WinDroidProto.Packet.newBuilder()
                            .setPong(pongMsg)
                            .build()

                        val responseBytes = responsePacket.toByteArray()
                        outputStream?.write(responseBytes)
                        outputStream?.flush()
                    }

                    if (packet.hasDirRequest()) {
                        val requestedPath = packet.dirRequest.path
                        android.util.Log.d("WinDroid", "PC requested directory: $requestedPath")

                        val rootStorage = Environment.getExternalStorageDirectory().absolutePath

                        val targetPath = if (requestedPath == "/") rootStorage else "$rootStorage$requestedPath"
                        val targetDir = java.io.File(targetPath)

                        val responseBuilder = WinDroidProto.DirectoryResponse.newBuilder()

                        if (targetDir.exists() && targetDir.isDirectory) {
                            responseBuilder.success = true

                            val files = targetDir.listFiles()
                            if (files != null) {
                                for (file in files) {
                                    val fileInfo = WinDroidProto.FileInfo.newBuilder()
                                        .setName(file.name)
                                        .setSizeBytes(if (file.isDirectory) 0 else file.length())
                                        .setIsDirectory(file.isDirectory)
                                        .setLastModifiedMs(file.lastModified())
                                        .build()

                                    responseBuilder.addFiles(fileInfo)
                                }
                            }
                        } else {
                            responseBuilder.success = false
                            responseBuilder.errorMessage = "Path does not exist or is not a directory"
                        }

                        val responsePacket = WinDroidProto.Packet.newBuilder().setDirResponse(responseBuilder).build()

                        val responseBytes = responsePacket.toByteArray()
                        outputStream?.write(responseBytes)
                        outputStream?.flush()
                    }

                    if (packet.hasFileReq()) {
                        val req = packet.fileReq

                        val rootStorage = Environment.getExternalStorageDirectory().absolutePath
                        val targetPath = if (req.path == "/") rootStorage else "$rootStorage${req.path}"
                        val targetFile = java.io.File(targetPath)

                        val responseBuilder = WinDroidProto.FileReadResponse.newBuilder()

                        if (targetFile.exists() && targetFile.isFile) {
                            try {
                                val raf = java.io.RandomAccessFile(targetFile, "r")
                                raf.seek(req.offset)

                                val chunk = ByteArray(req.maxBytes)
                                val bytesRead = raf.read(chunk)

                                responseBuilder.success = true
                                responseBuilder.totalFileSize = targetFile.length()

                                if (bytesRead > 0) {
                                    responseBuilder.data = com.google.protobuf.ByteString.copyFrom(chunk, 0, bytesRead)
                                    responseBuilder.eof = (req.offset + bytesRead) >= targetFile.length()
                                } else {
                                    responseBuilder.data = com.google.protobuf.ByteString.EMPTY
                                    responseBuilder.eof = true
                                }
                                raf.close()

                            } catch (e: Exception) {
                                responseBuilder.success = false
                                responseBuilder.errorMessage = e.message ?: "Unknown read error"
                            }
                        } else {
                            responseBuilder.success = false
                            responseBuilder.errorMessage = "File not found or is a directory"
                        }

                        val responsePacket = WinDroidProto.Packet.newBuilder()
                            .setFileRes(responseBuilder)
                            .build()

                        outputStream?.write(responsePacket.toByteArray())
                        outputStream?.flush()
                    }
                }
            } catch (e: Exception) {
                android.util.Log.e("WinDroid", "Stream read error or disconnected: ${e.message}")
                isConnected = false
                break
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        val filter = IntentFilter(ACTION_USB_PERMISSION)
        ContextCompat.registerReceiver(this, usbReceiver, filter, ContextCompat.RECEIVER_NOT_EXPORTED)

        setContent {
            WinDroidTheme {
                WinDroidApp()
            }
        }

        handleUsbIntent(intent)
        checkForExistingAccessory()
    }

    override fun onDestroy() {
        super.onDestroy()
        isConnected = false
        fileDescriptor?.close()
        unregisterReceiver(usbReceiver)
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

    private fun checkForExistingAccessory() {
        if (isConnected) return

        val usbManager = getSystemService(Context.USB_SERVICE) as UsbManager
        val accessoryList = usbManager.accessoryList

        if (accessoryList != null && accessoryList.isNotEmpty()) {
            val acc = accessoryList.find {
                it.manufacturer == "Wilson Oon" && it.model == "WinDroidFS"
            }

            acc?.let { accessory ->
                if (usbManager.hasPermission(accessory)) {
                    openAccessory(accessory)
                } else {
                    val intent = Intent(ACTION_USB_PERMISSION).apply {
                        setPackage(packageName)
                    }
                    val permissionIntent = PendingIntent.getBroadcast(
                        this,
                        0,
                        intent,
                        PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
                    )
                    usbManager.requestPermission(accessory, permissionIntent)
                }
            }
        }
    }

    private fun openAccessory(accessory: UsbAccessory) {
        try {
            val usbManager = getSystemService(Context.USB_SERVICE) as UsbManager
            fileDescriptor = usbManager.openAccessory(accessory)
            if (fileDescriptor != null) {
                val fd = fileDescriptor!!.fileDescriptor
                inputStream = FileInputStream(fd)
                outputStream = FileOutputStream(fd)
                isConnected = true

                Thread { listenForPackets() }.start()
                Log.d("WinDroid", "Successfully connected to Windows Daemon via Manual Scan!")
            }
        } catch (e: Exception) {
            Log.e("WinDroid", "Error opening accessory manually: ${e.message}")
        }
    }
}