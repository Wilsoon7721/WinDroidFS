package dev.wilsoon.windroidfs

import android.Manifest
import android.app.KeyguardManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.CancellationSignal
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.core.content.ContextCompat
import dev.wilsoon.windroidfs.link.Adb
import dev.wilsoon.windroidfs.link.AppSettings
import dev.wilsoon.windroidfs.link.CodePairing
import dev.wilsoon.windroidfs.link.WifiState
import dev.wilsoon.windroidfs.link.LinkService
import dev.wilsoon.windroidfs.link.LinkState
import dev.wilsoon.windroidfs.link.LinkTransport
import dev.wilsoon.windroidfs.link.Tethering
import dev.wilsoon.windroidfs.link.Wifi
import dev.wilsoon.windroidfs.link.LinkStatus
import dev.wilsoon.windroidfs.protocol.WinDroidProto.UsbSpeed
import dev.wilsoon.windroidfs.ui.*
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

enum class AppScreen {
    ONBOARDING, DASHBOARD, SETTINGS, WELCOME
}

private const val ACTION_USB_PERMISSION = "dev.wilsoon.windroidfs.USB_PERMISSION"

class MainActivity : ComponentActivity() {

    private val handler = Handler(Looper.getMainLooper())

    private val accessoryCheck = object : Runnable {
        override fun run() {
            checkForExistingAccessory()
            handler.postDelayed(this, 1000)
        }
    }

    private var permissionRequested = false
    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    private val confirmCredential =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
            unlocking = false
            if (it.resultCode == RESULT_OK) LinkService.unlock(this)
        }
    private var unlocking = false

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleUsbIntent(intent)
    }

    private fun handleUsbIntent(intent: Intent) {
        if (intent.action == UsbManager.ACTION_USB_ACCESSORY_ATTACHED) {
            LinkState.endedByUser = false
            val accessory: UsbAccessory? = intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
            accessory?.let { connect(it) }
        }
    }

    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (ACTION_USB_PERMISSION == intent.action) {
                val accessory: UsbAccessory? = intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
                if (intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)) {
                    accessory?.let { connect(it) }
                } else {
                    Log.d("WinDroid", "Permission denied for accessory")
                }
            }
        }
    }

    private fun connect(accessory: UsbAccessory) {
        LinkService.connect(this, accessory)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        val filter = IntentFilter(ACTION_USB_PERMISSION)
        ContextCompat.registerReceiver(this, usbReceiver, filter, ContextCompat.RECEIVER_NOT_EXPORTED)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }

        setContent {
            WinDroidTheme {
                WinDroidApp()
            }
        }

        handleUsbIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        handler.post(accessoryCheck)
        if (LinkState.status.value?.locked == true) askToUnlock()
    }

    /** Settings > Require biometrics: the PC sees nothing until this succeeds. */
    private fun askToUnlock() {
        if (unlocking) return
        val host = LinkState.status.value?.hostName?.ifEmpty { null } ?: "Your PC"
        unlocking = true
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val builder = BiometricPrompt.Builder(this)
                .setTitle("Unlock WinDroidFS")
                .setSubtitle("$host wants to open your files")
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                builder.setAllowedAuthenticators(
                    BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL,
                )
            } else {
                builder.setNegativeButton("Not now", mainExecutor) { _, _ -> unlocking = false }
            }
            builder.build().authenticate(CancellationSignal(), mainExecutor, object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    unlocking = false
                    LinkService.unlock(this@MainActivity)
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    unlocking = false
                    Log.i("WinDroid", "Unlock didn't happen: $errString")
                }
            })
        } else {
            @Suppress("DEPRECATION")
            val intent = getSystemService(KeyguardManager::class.java)
                .createConfirmDeviceCredentialIntent("Unlock WinDroidFS", "$host wants to open your files")
            if (intent == null) {
                unlocking = false
                LinkService.unlock(this)
            } else {
                confirmCredential.launch(intent)
            }
        }
    }

    override fun onPause() {
        handler.removeCallbacks(accessoryCheck)
        super.onPause()
    }

    override fun onDestroy() {
        super.onDestroy()
        unregisterReceiver(usbReceiver)
    }

    @Composable
    private fun WinDroidApp() {
        var currentScreen by remember {
            mutableStateOf(if (hasAllFilesPermission()) AppScreen.DASHBOARD else AppScreen.ONBOARDING)
        }

        var dashboardState by remember { mutableStateOf(DashboardUiState()) }
        var settingsState by remember { mutableStateOf(AppSettings.load(this)) }
        val changeSettings = { changed: SettingsUiState ->
            settingsState = changed
            AppSettings.save(this, changed)
        }
        val linkStatus by LinkState.status.collectAsState()
        val wifi by WifiState.info.collectAsState()
        val pairing by CodePairing.state.collectAsState()

        val linked = linkStatus?.waitingForPc == false
        val keepAwake = settingsState.keepScreenAwake && linked
        LaunchedEffect(keepAwake) {
            if (keepAwake) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        val locked = linkStatus?.locked == true
        LaunchedEffect(locked) { if (locked) askToUnlock() }
        var welcomeSeen by remember { mutableStateOf(AppSettings.welcomeSeen(this)) }
        val markWelcomeSeen = {
            welcomeSeen = true
            AppSettings.markWelcomeSeen(this)
        }

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
                    state = dashboardState.copy(
                        session = linkStatus?.takeUnless { it.waitingForPc }?.toSession(),
                        localIpAddress = wifi.address,
                        tcpPort = settingsState.tcpPort,
                        pairedPcs = wifi.pairedPcs,
                        pairing = pairing,
                    ),
                    onMethodSelected = { dashboardState = dashboardState.copy(expandedMethod = it) },
                    onOpenSettings = { currentScreen = AppScreen.SETTINGS },
                    onOpenDeveloperOptions = { openDeveloperOptions() },
                    onOpenTetheringSettings = { openTetheringSettings() },
                    onTerminateConnection = { LinkService.disconnect(this) },
                    onPairNewPc = { CodePairing.start(this) },
                    onForgetPc = { id ->
                        Wifi.forget(this, id)
                        WifiState.refresh(this)
                    },
                    onPairingAnswer = CodePairing::answer,
                    onPairingDismiss = CodePairing::dismiss,
                    onUnlock = { askToUnlock() },
                    showWelcome = !welcomeSeen,
                    onOpenWelcome = {
                        markWelcomeSeen()
                        currentScreen = AppScreen.WELCOME
                    },
                    onDismissWelcome = { markWelcomeSeen() }
                )
            }

            AppScreen.WELCOME -> WelcomeScreen(onBack = { currentScreen = AppScreen.DASHBOARD })

            AppScreen.SETTINGS -> {
                SettingsScreen(
                    state = settingsState,
                    onBack = { currentScreen = AppScreen.DASHBOARD },
                    onStorageScopeChange = { changeSettings(settingsState.copy(storageScope = it)) },
                    onFolderToggle = { folder, isExposed ->
                        val updatedFolders = settingsState.exposedFolders.toMutableMap()
                        updatedFolders[folder] = isExposed
                        changeSettings(settingsState.copy(exposedFolders = updatedFolders))
                    },
                    onRequireBiometricsChange = { changeSettings(settingsState.copy(requireBiometrics = it)) },
                    onKeepScreenAwakeChange = { changeSettings(settingsState.copy(keepScreenAwake = it)) },
                    onPortChange = { changeSettings(settingsState.copy(tcpPort = it)) },
                    wifiPortTaken = LinkState.wifiPortTaken.collectAsState().value
                )
            }
        }
    }

    private fun LinkStatus.toSession() = ActiveSession(
        method = when (transport) {
            LinkTransport.ACCESSORY -> ConnectionMethod.AOA
            LinkTransport.TETHERING -> ConnectionMethod.TETHERING
            LinkTransport.ADB -> ConnectionMethod.ADB
            LinkTransport.WIFI -> ConnectionMethod.WIFI
        },
        hostName = hostName.ifEmpty { "your PC" },
        locked = locked,
        uptimeSeconds = uptimeSeconds,
        readBytesPerSecond = readBytesPerSecond,
        writeBytesPerSecond = writeBytesPerSecond,
        totalBytesTransferred = totalBytes,
        activeTransfer = currentWrite?.let {
            TransferInProgress(fileName = it.fileName, bytesDone = it.bytesDone, bytesTotal = it.bytesTotal)
        },
        usbLink = when (usbSpeed) {
            UsbSpeed.USB_SPEED_LOW -> UsbLink.USB_1_0
            UsbSpeed.USB_SPEED_FULL -> UsbLink.USB_1_1
            UsbSpeed.USB_SPEED_HIGH -> UsbLink.USB_2_0
            UsbSpeed.USB_SPEED_SUPER -> UsbLink.USB_3_0
            UsbSpeed.USB_SPEED_SUPER_PLUS -> UsbLink.USB_3_1
            else -> UsbLink.UNKNOWN
        }
    )

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
        WifiState.refresh(this)
        // Only skip while linked
        if (LinkState.status.value?.waitingForPc == false) return

        val usbManager = getSystemService(Context.USB_SERVICE) as UsbManager
        val accessoryList = usbManager.accessoryList

        val accessory = accessoryList?.find {
            it.manufacturer == "Wilson Oon" && it.model == "WinDroidFS"
        }
        val tethered = Tethering.address() != null
        if (accessory == null && !tethered) LinkState.endedByUser = false
        if (LinkState.endedByUser) return

        if (tethered) LinkService.listenOnTethering(this)
        if (Adb.available(this)) LinkService.listenOnAdb(this)
        if (Wifi.available(this)) LinkService.listenOnWifi(this)
        if (accessory == null) {
            permissionRequested = false
            return
        }

        if (usbManager.hasPermission(accessory)) {
            connect(accessory)
        } else if (!permissionRequested) {
            permissionRequested = true
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
