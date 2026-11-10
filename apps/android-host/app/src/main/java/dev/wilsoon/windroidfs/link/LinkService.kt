package dev.wilsoon.windroidfs.link

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.net.LocalServerSocket
import android.net.LocalSocket
import android.media.MediaScannerConnection
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.net.wifi.WifiManager
import android.os.PowerManager
import android.provider.Settings
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.content.pm.PackageInfoCompat
import com.google.protobuf.ByteString
import com.google.protobuf.InvalidProtocolBufferException
import dev.wilsoon.windroidfs.MainActivity
import dev.wilsoon.windroidfs.R
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Goodbye
import dev.wilsoon.windroidfs.protocol.WinDroidProto.OperationResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.PairResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.UsbSpeed
import java.io.Closeable
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import javax.net.ssl.SSLSocket
import java.io.InputStream
import java.io.OutputStream
import java.net.BindException
import java.net.InetAddress
import java.net.ServerSocket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

data class LinkStatus(
    val hostName: String,
    val transport: LinkTransport,
    val waitingForPc: Boolean,
    val uptimeSeconds: Long,
    /** Waiting for the owner's fingerprint (Settings > Require biometrics). */
    val locked: Boolean = false,
    /* From PC view */
    val readBytesPerSecond: Long,
    val writeBytesPerSecond: Long,
    val totalBytes: Long,
    val currentWrite: WriteProgress?,
    val usbSpeed: UsbSpeed,
)

/** The live link's status for the UI, null when no link */
object LinkState {
    private val mutableStatus = MutableStateFlow<LinkStatus?>(null)
    val status: StateFlow<LinkStatus?> = mutableStatus.asStateFlow()

    internal fun publish(status: LinkStatus?) {
        mutableStatus.value = status
    }

    private val mutableWifiPortTaken = MutableStateFlow<Int?>(null)
    /** Settings > TCP port: another app already listens on it, so Wi-Fi can't. */
    val wifiPortTaken: StateFlow<Int?> = mutableWifiPortTaken.asStateFlow()

    internal fun publishWifiPortTaken(port: Int?) {
        mutableWifiPortTaken.value = port
    }

    /** Disconnect was pressed: don't link again until the cable is replugged or tethering toggled. */
    @Volatile var endedByUser = false
        internal set

    private val mutableUpdateNeeded = MutableStateFlow<UpdateNeeded?>(null)
    /** A PC said this app needs updating (it decides, from the version in every pong). Null after it's closed. */
    val updateNeeded: StateFlow<UpdateNeeded?> = mutableUpdateNeeded.asStateFlow()

    internal fun publishUpdateNeeded(update: UpdateNeeded) {
        mutableUpdateNeeded.value = update
    }

    /** "Not now" or "Close" on the update screen. A PC says it again the next time it connects. */
    fun dismissUpdate() {
        mutableUpdateNeeded.value = null
    }
}

/** What a PC said about this app's version, for the update screen. */
data class UpdateNeeded(
    /** The PC won't connect until it's updated. */
    val required: Boolean,
    /** The newest version the PC knows of, e.g. "1.2.0". */
    val latestVersion: String,
    val pcName: String,
)

/** Foreground service for USB link */
class LinkService : Service() {
    /** Every route the PC might speak on. The first it does claims the link and the rest close. Closing one unblocks its thread. */
    private val routes = ConcurrentHashMap<LinkTransport, Closeable>()
    private val claimed = AtomicBoolean(false)
    @Volatile private var transport = LinkTransport.ACCESSORY
    @Volatile private var tetherAddress: InetAddress? = null
    @Volatile private var wifiAddress: InetAddress? = null
    @Volatile private var wifiAdvert: Advert? = null
    @Volatile private var wifiPort = 0
    @Volatile private var locked = false
    private var detachRegistered = false
    private var wakeLock: PowerManager.WakeLock? = null
    private val wifiLocks = mutableListOf<WifiManager.WifiLock>()
    private var ticker: ScheduledExecutorService? = null
    private val mainHandler = Handler(Looper.getMainLooper())

    // Written by the USB thread, read by the ticker
    @Volatile private var hostName = ""
    @Volatile private var usbSpeed = UsbSpeed.USB_SPEED_UNKNOWN
    @Volatile private var server: FileServer? = null
    private val bytesRead = AtomicLong()
    private val bytesWritten = AtomicLong()
    private var connectedAtMs = 0L

    @Volatile private var running = false

    // For the watchdog in publishStatus
    @Volatile private var lastActivityMs = 0L
    @Volatile private var handlingRequest = false
    @Volatile private var pcSeen = false
    /** Answer the PC's next request with a goodbye, then close. */
    @Volatile private var sayingGoodbye = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_UNLOCK) {
            locked = false
            if (!running) stopSelf()
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_STOP) {
            LinkState.endedByUser = true
            if (running && pcSeen) {
                // Over USB the PC only receives when it asks, so the goodbye waits for its next request (an idle ping within 3s).
                sayingGoodbye = true
                mainHandler.postDelayed({ stopSelf() }, GOODBYE_GRACE_MS)
            } else {
                stopSelf()
            }
            return START_NOT_STICKY
        }

        // Required promptly after startForegroundService, whatever happens next.
        if (!enterForeground()) {
            stopSelf()
            return START_NOT_STICKY
        }
        // Linked already: the PC chose, so new routes don't matter
        if (pcSeen) return START_NOT_STICKY

        when (intent?.action) {
            ACTION_TETHER -> offerTethering()
            ACTION_ADB -> offerAdb()
            ACTION_WIFI -> offerWifi()
            else -> intent?.accessoryExtra()?.let(::offerAccessory)
        }
        stopIfIdle()
        return START_NOT_STICKY
    }

    /**
     * Stop once there's nothing to wait on. While the cable is plugged in there still might be: tethering, Accessory Mode
     * or USB debugging can start at any moment, typically while the user is in Settings switching to one. An app in the
     * background can't start this service again to listen for them, so it has to still be running then.
     */
    private fun stopIfIdle() {
        if (routes.isEmpty() && !(running && cablePlugged())) stopSelf()
    }

    /** Whether a USB cable connects the phone to something, read from the system's sticky USB_STATE broadcast. */
    private fun cablePlugged(): Boolean =
        ContextCompat.registerReceiver(this, null, IntentFilter(ACTION_USB_STATE), ContextCompat.RECEIVER_NOT_EXPORTED)
            ?.getBooleanExtra("connected", false) == true

    override fun onDestroy() {
        running = false
        routes.values.forEach(::quietlyClose)
        routes.clear()
        wifiAdvert?.stop()
        wifiAdvert = null
        LinkState.publishWifiPortTaken(null)
        ticker?.shutdownNow()
        ticker = null
        if (detachRegistered) unregisterReceiver(detachReceiver)
        wakeLock?.takeIf { it.isHeld }?.release()
        releaseWifi()
        LinkState.publish(null)
        super.onDestroy()
    }

    private fun offerAccessory(accessory: UsbAccessory) {
        if (routes.containsKey(LinkTransport.ACCESSORY)) return
        val usbManager = getSystemService(USB_SERVICE) as UsbManager
        val fd = try {
            usbManager.openAccessory(accessory)
        } catch (e: Exception) {
            Log.e(TAG, "Couldn't open the accessory", e)
            null
        } ?: return

        if (!detachRegistered) {
            ContextCompat.registerReceiver(
                this,
                detachReceiver,
                IntentFilter(UsbManager.ACTION_USB_ACCESSORY_DETACHED),
                ContextCompat.RECEIVER_NOT_EXPORTED,
            )
            detachRegistered = true
        }
        offer(LinkTransport.ACCESSORY, fd) {
            FileInputStream(fd.fileDescriptor) to FileOutputStream(fd.fileDescriptor)
        }
    }

    /** Listen on the tethering link only, so nothing on Wi-Fi can reach it. */
    private fun offerTethering() {
        if (routes.containsKey(LinkTransport.TETHERING)) return
        val address = Tethering.address() ?: return
        val server = try {
            ServerSocket(Tethering.PORT, 1, address)
        } catch (e: IOException) {
            Log.e(TAG, "Couldn't listen on $address", e)
            return
        }
        tetherAddress = address
        offer(LinkTransport.TETHERING, server) {
            val client = server.accept()
            server.close()
            routes[LinkTransport.TETHERING] = client
            client.tcpNoDelay = true
            client.getInputStream() to client.getOutputStream()
        }
    }

    /** `adb forward` reaches this socket. Other apps can too, so only adbd is let in. */
    private fun offerAdb() {
        if (routes.containsKey(LinkTransport.ADB)) return
        val server = try {
            LocalServerSocket(Adb.SOCKET_NAME)
        } catch (e: IOException) {
            Log.e(TAG, "Couldn't listen on @${Adb.SOCKET_NAME}", e)
            return
        }
        // close() alone doesn't wake a blocked accept()
        offer(LinkTransport.ADB, Closeable { shutdown(server.fileDescriptor); server.close() }) {
            var client: LocalSocket
            while (true) {
                client = server.accept()
                val uid = client.peerCredentials.uid
                if (Adb.isAdbd(uid)) break
                Log.w(TAG, "Refused a connection from uid $uid")
                client.close()
            }
            shutdown(server.fileDescriptor)
            server.close()
            routes[LinkTransport.ADB] = Closeable { shutdown(client.fileDescriptor); client.close() }
            client.inputStream to client.outputStream
        }
    }

    /** TLS on the Wi-Fi address. Only a paired PC gets past the handshake; anyone else is dropped and the wait goes on. */
    private fun offerWifi() {
        if (routes.containsKey(LinkTransport.WIFI) || !Wifi.available(this)) return
        val address = Wifi.address() ?: return
        val port = Wifi.port(this)
        val server = try {
            Wifi.serverSocket(this, address, port)
        } catch (e: Exception) {
            // Tried again every second, so only say so once.
            val taken = (e as? BindException)?.let { port }
            if (LinkState.wifiPortTaken.value != taken || taken == null) {
                Log.e(TAG, "Couldn't listen on Wi-Fi at $address:$port", e)
            }
            LinkState.publishWifiPortTaken(taken)
            return
        }
        LinkState.publishWifiPortTaken(null)
        wifiAddress = address
        wifiPort = server.localPort
        val name = deviceName()
        wifiAdvert?.stop()
        wifiAdvert = Advert(this, Wifi.SERVICE_TYPE, name, wifiPort, mapOf(
            "id" to Wifi.fingerprint(Wifi.certificate().encoded),
            "name" to name,
        ))
        offer(LinkTransport.WIFI, server) {
            var client: SSLSocket
            while (true) {
                client = server.accept() as SSLSocket
                try {
                    // Nobody gets to hold the only seat by never finishing the handshake.
                    client.soTimeout = HANDSHAKE_TIMEOUT_MS
                    client.startHandshake()
                    client.soTimeout = 0
                    break
                } catch (e: IOException) {
                    Log.w(TAG, "Refused a Wi-Fi connection from ${client.inetAddress}: ${e.message}")
                    client.close()
                }
            }
            server.close()
            routes[LinkTransport.WIFI] = client
            client.tcpNoDelay = true
            client.inputStream to client.outputStream
        }
    }

    /** Wait on [route] in its own thread: [connect] blocks until the PC arrives, and closing [handle] gives up. */
    private fun offer(route: LinkTransport, handle: Closeable, connect: () -> Pair<InputStream, OutputStream>) {
        routes[route] = handle
        if (!running) {
            running = true
            pcSeen = false
            claimed.set(false)
            bytesRead.set(0)
            bytesWritten.set(0)
            ticker = Executors.newSingleThreadScheduledExecutor().apply {
                scheduleWithFixedDelay(::publishStatus, 0, 1, TimeUnit.SECONDS)
            }
        }
        Thread({
            val streams = try {
                connect()
            } catch (e: IOException) {
                Log.i(TAG, "Stopped waiting over $route: ${e.message}")
                null
            }
            if (streams != null) serve(route, streams.first, streams.second)
            routeEnded(route)
        }, "windroid-$route").start()
        Log.d(TAG, "Waiting for the PC over $route")
    }

    /** The PC spoke on [route] first: it's the link now. */
    private fun claim(route: LinkTransport): Boolean {
        if (!claimed.compareAndSet(false, true)) return false
        transport = route
        for ((other, handle) in routes) {
            if (other != route) {
                routes.remove(other)
                quietlyClose(handle)
            }
        }
        return true
    }

    private fun routeEnded(route: LinkTransport) {
        routes.remove(route)?.let(::quietlyClose)
        if (route == LinkTransport.WIFI) {
            wifiAdvert?.stop()
            wifiAdvert = null
        }
        if (claimed.get() && route == transport) {
            mainHandler.post(::linkEnded)
        } else if (!claimed.get()) {
            mainHandler.post(::stopIfIdle)
        }
    }

    /** The PC left. Unless Disconnect was pressed, wait on whatever routes are left, so the next PC finds the app without it being opened. */
    private fun linkEnded() {
        wakeLock?.takeIf { it.isHeld }?.release()
        wakeLock = null
        releaseWifi()
        pcSeen = false
        sayingGoodbye = false
        locked = false
        claimed.set(false)
        hostName = ""
        usbSpeed = UsbSpeed.USB_SPEED_UNKNOWN
        if (LinkState.endedByUser) {
            stopSelf()
            return
        }
        lastNotifiedHost = null
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(null))
        offerAvailableRoutes()
        stopIfIdle()
    }

    /** Offering a route that's already offered does nothing. */
    private fun offerAvailableRoutes() {
        if (!running || pcSeen) return
        val usbManager = getSystemService(USB_SERVICE) as UsbManager
        usbManager.accessoryList
            ?.find { it.manufacturer == "Wilson Oon" && it.model == "WinDroidFS" }
            ?.takeIf(usbManager::hasPermission)
            ?.let(::offerAccessory)
        offerTethering()
        if (Adb.available(this)) offerAdb()
        offerWifi()
    }

    /** Only over USB, which proves which PC this is. Over Wi-Fi anyone could ask. */
    private fun pair(route: LinkTransport, request: Packet): Packet {
        val response = PairResponse.newBuilder()
        if (route == LinkTransport.WIFI) {
            response.setSuccess(false).setErrorMessage("Pair over USB")
        } else try {
            val pc = request.pairRequest
            Wifi.pair(this, pc.hostName.ifEmpty { "PC" }, pc.certificate.toByteArray())
            response.setSuccess(true)
                .setCertificate(ByteString.copyFrom(Wifi.certificate().encoded))
                .setWifiPort(Wifi.port(this))
            Wifi.address()?.hostAddress?.let(response::addWifiAddresses)
            Log.i(TAG, "Paired with ${pc.hostName} for Wi-Fi")
        } catch (e: Exception) {
            Log.e(TAG, "Pairing failed", e)
            response.setSuccess(false).setErrorMessage(e.message ?: "Pairing failed")
        }
        return Packet.newBuilder().setRequestId(request.requestId).setPairResponse(response).build()
    }

    private fun serve(route: LinkTransport, input: InputStream, output: OutputStream) {
        val frames = FrameReader()
        var fileServer: FileServer? = null

        val buffer = ByteArray(16 * 1024)

        var pcSaidGoodbye = false
        while (running && !pcSaidGoodbye) {
            try {
                val bytesIn = input.read(buffer)
                if (bytesIn < 0) break // the PC closed the link
                if (bytesIn == 0) continue
                lastActivityMs = System.currentTimeMillis()
                frames.append(buffer, bytesIn)

                while (true) {
                    val payload = frames.next() ?: break
                    val request = try {
                        Packet.parseFrom(payload)
                    } catch (e: InvalidProtocolBufferException) {
                        Log.w(TAG, "Dropped an unreadable message: ${e.message}")
                        continue
                    }
                    if (fileServer == null) {
                        if (!claim(route)) return
                        val app = packageManager.getPackageInfo(packageName, 0)
                        fileServer = FileServer(
                            deviceName = deviceName(),
                            appVersionCode = PackageInfoCompat.getLongVersionCode(app).toInt(),
                            appVersionName = app.versionName.orEmpty(),
                            exposedFolders = { AppSettings.exposedFolders(this) },
                            locked = { locked },
                            onFileChanged = { file ->
                                MediaScannerConnection.scanFile(applicationContext, arrayOf(file.path), null, null)
                            },
                        )
                        server = fileServer
                        pcFound()
                    }
                    if (request.hasPing()) {
                        if (request.ping.hostName.isNotEmpty()) hostName = request.ping.hostName
                        usbSpeed = request.ping.usbSpeed
                    }

                    // A bug in one request should not crash - reply with just the ID.
                    handlingRequest = true
                    val goodbye = sayingGoodbye
                    val reply = if (goodbye) {
                        Packet.newBuilder()
                            .setRequestId(request.requestId)
                            .setGoodbye(Goodbye.getDefaultInstance())
                            .build()
                    } else if (request.hasPairRequest()) {
                        pair(route, request)
                    } else if (request.hasUpdateNotice()) {
                        updateNeeded(request)
                    } else try {
                        fileServer.handle(request)
                    } catch (e: Exception) {
                        Log.e(TAG, "Failed to handle request ${request.requestId}", e)
                        Packet.newBuilder().setRequestId(request.requestId).build()
                    }
                    if (request.hasWriteRequest()) bytesWritten.addAndGet(reply.opResponse.bytesWritten)
                    if (reply.hasFileRes()) bytesRead.addAndGet(reply.fileRes.data.size().toLong())

                    output.write(Framing.encode(reply.toByteArray()))
                    output.flush()
                    handlingRequest = false
                    lastActivityMs = System.currentTimeMillis()

                    if (goodbye || request.hasGoodbye()) {
                        Log.i(TAG, if (goodbye) "Told the PC goodbye" else "The PC disconnected")
                        pcSaidGoodbye = true
                        break
                    }
                }

                if (frames.skippedBytes > 0) {
                    Log.w(TAG, "Skipped ${frames.skippedBytes} corrupt bytes while resyncing")
                    frames.skippedBytes = 0
                }
            } catch (e: IOException) {
                Log.i(TAG, "Link ended: ${e.message}")
                break
            }
        }

        try {
            fileServer?.close()
        } catch (e: IOException) {
            Log.w(TAG, "Couldn't sync the file being written: ${e.message}")
        }
        server = null
    }

    /**
     * Wi-Fi power saving naps between packets, adding 100ms+ to every request, and Explorer sends them one after another.
     * Low latency only applies while the app is on screen; high performance covers the rest where Android still honours it.
     */
    @Suppress("DEPRECATION")
    private fun holdWifiAwake() {
        val wifi = applicationContext.getSystemService(WifiManager::class.java) ?: return
        val modes = buildList {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) add(WifiManager.WIFI_MODE_FULL_LOW_LATENCY)
            add(WifiManager.WIFI_MODE_FULL_HIGH_PERF)
        }
        modes.forEach { mode ->
            wifiLocks += wifi.createWifiLock(mode, "WinDroidFS:wifi").apply {
                setReferenceCounted(false)
                acquire()
            }
        }
    }

    private fun releaseWifi() {
        wifiLocks.forEach { if (it.isHeld) it.release() }
        wifiLocks.clear()
    }

    /** The accessory stays open after the PC leaves */
    private fun pcFound() {
        connectedAtMs = System.currentTimeMillis()
        lastActivityMs = connectedAtMs
        wakeLock = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "WinDroidFS:link").apply { acquire() }
        if (transport == LinkTransport.WIFI) holdWifiAwake()
        locked = AppSettings.load(this).requireBiometrics
        pcSeen = true
        Log.d(TAG, "The PC answered")
    }

    private var lastRead = 0L
    private var lastWritten = 0L
    private var lastNotifiedLocked = false
    private var lastNotifiedHost: String? = null // null while waiting for the PC

    /** Once a second: work out speeds, publish the status, and watch for a silent PC */
    private fun publishStatus() {
        if (!running) return
        if (!pcSeen) {
            if (routes.containsKey(LinkTransport.TETHERING) && Tethering.address() != tetherAddress) {
                Log.i(TAG, "USB tethering ended")
                routes[LinkTransport.TETHERING]?.let(::quietlyClose)
            }
            if (routes.containsKey(LinkTransport.ADB) && !Adb.available(this)) {
                Log.i(TAG, "USB debugging ended")
                routes[LinkTransport.ADB]?.let(::quietlyClose)
            }
            if (routes.containsKey(LinkTransport.WIFI) &&
                (Wifi.address() != wifiAddress || !Wifi.available(this) || Wifi.port(this) != wifiPort)
            ) {
                Log.i(TAG, "Wi-Fi changed")
                routes[LinkTransport.WIFI]?.let(::quietlyClose)
            }
            // Routes that turned up since, such as Wi-Fi once a PC is paired or tethering switched on. Then, if there's
            // still nothing and the cable's been unplugged, nothing more can turn up.
            mainHandler.post(::offerAvailableRoutes)
            mainHandler.post(::stopIfIdle)

            LinkState.publish(
                LinkStatus(
                    hostName = "",
                    transport = transport,
                    waitingForPc = true,
                    uptimeSeconds = 0,
                    readBytesPerSecond = 0,
                    writeBytesPerSecond = 0,
                    totalBytes = 0,
                    currentWrite = null,
                    usbSpeed = UsbSpeed.USB_SPEED_UNKNOWN,
                ),
            )
            return
        }

        val silentFor = System.currentTimeMillis() - lastActivityMs
        if (!handlingRequest && silentFor > PC_SILENCE_TIMEOUT_MS) {
            Log.i(TAG, "No word from the PC for ${silentFor / 1000}s; ending the link")
            // Its thread then ends and the routes reopen (see linkEnded)
            routes[transport]?.let(::quietlyClose)
            return
        }
        val read = bytesRead.get()
        val written = bytesWritten.get()
        LinkState.publish(
            LinkStatus(
                hostName = hostName,
                transport = transport,
                waitingForPc = false,
                uptimeSeconds = (System.currentTimeMillis() - connectedAtMs) / 1000,
                locked = locked,
                readBytesPerSecond = read - lastRead,
                writeBytesPerSecond = written - lastWritten,
                totalBytes = read + written,
                currentWrite = server?.currentWrite,
                usbSpeed = usbSpeed,
            ),
        )
        lastRead = read
        lastWritten = written

        val host = hostName
        if (host != lastNotifiedHost || locked != lastNotifiedLocked) {
            lastNotifiedHost = host
            lastNotifiedLocked = locked
            getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION_ID, notification(host, locked))
        }
    }

    private val detachReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            Log.i(TAG, "Accessory detached")
            routes[LinkTransport.ACCESSORY]?.let(::quietlyClose)
        }
    }

    private fun enterForeground(): Boolean = try {
        createChannel()
        val notification = notification(null)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        true
    } catch (e: Exception) {
        // Refused if started from the background (12+) or without the USB permission (14+)
        Log.e(TAG, "Couldn't start as a foreground service", e)
        false
    }

    /**
     * The PC found this app out of date. A service can't open a screen from the background, so post a notification
     * that opens it; if the app is already open, it switches to the update screen at once (see [LinkState.updateNeeded]).
     */
    private fun updateNeeded(request: Packet): Packet {
        val notice = request.updateNotice
        val pc = hostName.ifEmpty { "Your PC" }
        Log.i(TAG, "$pc says this app needs updating to ${notice.latestVersion} (required: ${notice.required})")
        LinkState.publishUpdateNeeded(UpdateNeeded(notice.required, notice.latestVersion, hostName))

        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(UPDATE_CHANNEL_ID, "App updates", NotificationManager.IMPORTANCE_DEFAULT)
                    .apply { description = "When a PC needs a newer version of this app" },
            )
        }
        val openApp = PendingIntent.getActivity(
            this,
            2,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, UPDATE_CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        val latest = notice.latestVersion.ifEmpty { "A newer version" }
        manager.notify(
            UPDATE_NOTIFICATION_ID,
            builder
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(if (notice.required) "Update WinDroidFS to connect" else "WinDroidFS $latest is out")
                .setContentText(
                    if (notice.required) "$pc needs WinDroidFS $latest or later" else "Tap to see how to update",
                )
                .setContentIntent(openApp)
                .setAutoCancel(true)
                .build(),
        )
        return Packet.newBuilder()
            .setRequestId(request.requestId)
            .setOpResponse(OperationResponse.newBuilder().setSuccess(true))
            .build()
    }

    private fun createChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Connection to your PC",
            NotificationManager.IMPORTANCE_LOW,
        ).apply { description = "Shown while WinDroidFS is connected to a PC" }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    /** Host is null while waiting for the PC */
    private fun notification(host: String?, locked: Boolean = false): Notification {
        val openApp = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val disconnect = PendingIntent.getService(
            this,
            1,
            Intent(this, LinkService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        return builder
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(
                when {
                    host == null -> "Waiting for your PC"
                    locked -> "${host.ifEmpty { "Your PC" }} wants your files"
                    host.isEmpty() -> "Connected to your PC"
                    else -> "Connected to $host"
                },
            )
            .setContentText(
                when {
                    host == null -> "Start WinDroidFS on the PC to mount your storage"
                    locked -> "Tap to unlock with your fingerprint"
                    else -> "Your phone's storage is mounted on the PC"
                },
            )
            .setContentIntent(openApp)
            .addAction(
                Notification.Action.Builder(null, "Disconnect", disconnect).build(),
            )
            .setOngoing(true)
            .build()
    }

    /** Routes get closed from several places, often twice. Nothing about closing one may take the app down. */
    private fun quietlyClose(handle: Closeable) {
        try {
            handle.close()
        } catch (e: Exception) {
            Log.w(TAG, "Closing a route failed: ${e.message}")
        }
    }

    /** Null once the socket is closed, which a second close finds. */
    private fun shutdown(fd: java.io.FileDescriptor?) {
        if (fd == null) return
        try {
            Os.shutdown(fd, OsConstants.SHUT_RDWR)
        } catch (_: ErrnoException) {
        }
    }

    /** The name set in Settings > About phone, else the model. Used for the drive label */
    private fun deviceName() = Wifi.deviceName(this)

    private fun Intent.accessoryExtra(): UsbAccessory? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            getParcelableExtra(UsbManager.EXTRA_ACCESSORY, UsbAccessory::class.java)
        } else {
            @Suppress("DEPRECATION")
            getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
        }

    companion object {
        private const val TAG = "WinDroid"
        private const val CHANNEL_ID = "link"
        private const val NOTIFICATION_ID = 1
        private const val UPDATE_CHANNEL_ID = "updates"
        private const val UPDATE_NOTIFICATION_ID = 2

        /** The PC pings every 3s when idle, 1 reply can take up to 15s */
        private const val PC_SILENCE_TIMEOUT_MS = 20_000L
        /** How long to wait for a PC request to carry the goodbye before closing anyway. */
        private const val GOODBYE_GRACE_MS = 5_000L
        /** The system's sticky broadcast of the USB connection state. Hidden from the SDK, but stable since Android 4. */
        private const val ACTION_USB_STATE = "android.hardware.usb.action.USB_STATE"
        private const val ACTION_STOP = "dev.wilsoon.windroidfs.action.DISCONNECT"
        private const val ACTION_TETHER = "dev.wilsoon.windroidfs.action.TETHER"
        private const val ACTION_UNLOCK = "dev.wilsoon.windroidfs.action.UNLOCK"
        private const val ACTION_ADB = "dev.wilsoon.windroidfs.action.ADB"
        private const val ACTION_WIFI = "dev.wilsoon.windroidfs.action.WIFI"
        private const val HANDSHAKE_TIMEOUT_MS = 10_000

        fun connect(context: Context, accessory: UsbAccessory) {
            val intent = Intent(context, LinkService::class.java).putExtra(UsbManager.EXTRA_ACCESSORY, accessory)
            ContextCompat.startForegroundService(context, intent)
        }

        /** Call from the visible activity, while `Tethering.address` isn't null. */
        fun listenOnTethering(context: Context) {
            val intent = Intent(context, LinkService::class.java).setAction(ACTION_TETHER)
            ContextCompat.startForegroundService(context, intent)
        }

        /** Call from the visible activity, while [Adb.available]. */
        /** Call from the visible activity, while [Wifi.available]. */
        fun listenOnWifi(context: Context) {
            val intent = Intent(context, LinkService::class.java).setAction(ACTION_WIFI)
            ContextCompat.startForegroundService(context, intent)
        }

        fun listenOnAdb(context: Context) {
            val intent = Intent(context, LinkService::class.java).setAction(ACTION_ADB)
            ContextCompat.startForegroundService(context, intent)
        }

        /** After the owner confirmed with a fingerprint. */
        fun unlock(context: Context) {
            context.startService(Intent(context, LinkService::class.java).setAction(ACTION_UNLOCK))
        }

        fun disconnect(context: Context) {
            context.startService(Intent(context, LinkService::class.java).setAction(ACTION_STOP))
        }
    }
}
