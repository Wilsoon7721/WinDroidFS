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
import android.os.PowerManager
import android.provider.Settings
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import androidx.core.content.ContextCompat
import com.google.protobuf.InvalidProtocolBufferException
import dev.wilsoon.windroidfs.MainActivity
import dev.wilsoon.windroidfs.R
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Goodbye
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.UsbSpeed
import java.io.Closeable
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
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

    /** Disconnect was pressed: don't link again until the cable is replugged or tethering toggled. */
    @Volatile var endedByUser = false
        internal set
}

/** Foreground service for USB link */
class LinkService : Service() {
    /** Every route the PC might speak on. The first it does claims the link and the rest close. Closing one unblocks its thread. */
    private val routes = ConcurrentHashMap<LinkTransport, Closeable>()
    private val claimed = AtomicBoolean(false)
    @Volatile private var transport = LinkTransport.ACCESSORY
    @Volatile private var tetherAddress: InetAddress? = null
    private var detachRegistered = false
    private var wakeLock: PowerManager.WakeLock? = null
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
            else -> intent?.accessoryExtra()?.let(::offerAccessory)
        }
        if (routes.isEmpty()) stopSelf()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        routes.values.forEach(::quietlyClose)
        routes.clear()
        ticker?.shutdownNow()
        ticker = null
        if (detachRegistered) unregisterReceiver(detachReceiver)
        wakeLock?.takeIf { it.isHeld }?.release()
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
        if (claimed.get() && route == transport) {
            mainHandler.post(::linkEnded)
        } else if (!claimed.get() && routes.isEmpty()) {
            mainHandler.post { stopSelf() }
        }
    }

    /** The PC left. Unless Disconnect was pressed, wait on whatever routes are left, so the next PC finds the app without it being opened. */
    private fun linkEnded() {
        wakeLock?.takeIf { it.isHeld }?.release()
        wakeLock = null
        pcSeen = false
        sayingGoodbye = false
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
        if (routes.isEmpty()) stopSelf()
    }

    private fun offerAvailableRoutes() {
        val usbManager = getSystemService(USB_SERVICE) as UsbManager
        usbManager.accessoryList
            ?.find { it.manufacturer == "Wilson Oon" && it.model == "WinDroidFS" }
            ?.takeIf(usbManager::hasPermission)
            ?.let(::offerAccessory)
        offerTethering()
        if (Adb.available(this)) offerAdb()
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
                        fileServer = FileServer(deviceName = deviceName(), onFileChanged = { file ->
                            MediaScannerConnection.scanFile(applicationContext, arrayOf(file.path), null, null)
                        })
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

    /** The accessory stays open after the PC leaves */
    private fun pcFound() {
        connectedAtMs = System.currentTimeMillis()
        lastActivityMs = connectedAtMs
        wakeLock = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "WinDroidFS:link").apply { acquire() }
        pcSeen = true
        Log.d(TAG, "The PC answered")
    }

    private var lastRead = 0L
    private var lastWritten = 0L
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
        if (host != lastNotifiedHost) {
            lastNotifiedHost = host
            getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION_ID, notification(host))
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
    private fun notification(host: String?): Notification {
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
                    host.isEmpty() -> "Connected to your PC"
                    else -> "Connected to $host"
                },
            )
            .setContentText(
                if (host == null) "Start WinDroidFS on the PC to mount your storage"
                else "Your phone's storage is mounted on the PC",
            )
            .setContentIntent(openApp)
            .addAction(
                Notification.Action.Builder(null, "Disconnect", disconnect).build(),
            )
            .setOngoing(true)
            .build()
    }

    private fun quietlyClose(handle: Closeable) {
        try {
            handle.close()
        } catch (_: IOException) {
        }
    }

    private fun shutdown(fd: java.io.FileDescriptor) {
        try {
            Os.shutdown(fd, OsConstants.SHUT_RDWR)
        } catch (_: ErrnoException) {
        }
    }

    /** The name set in Settings > About phone, else the model. Used for the drive label */
    private fun deviceName(): String {
        val named = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N_MR1) {
            Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME)
        } else {
            null
        }
        return named?.takeIf { it.isNotBlank() } ?: Build.MODEL.orEmpty()
    }

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

        /** The PC pings every 3s when idle, 1 reply can take up to 15s */
        private const val PC_SILENCE_TIMEOUT_MS = 20_000L
        /** How long to wait for a PC request to carry the goodbye before closing anyway. */
        private const val GOODBYE_GRACE_MS = 5_000L
        private const val ACTION_STOP = "dev.wilsoon.windroidfs.action.DISCONNECT"
        private const val ACTION_TETHER = "dev.wilsoon.windroidfs.action.TETHER"
        private const val ACTION_ADB = "dev.wilsoon.windroidfs.action.ADB"

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
        fun listenOnAdb(context: Context) {
            val intent = Intent(context, LinkService::class.java).setAction(ACTION_ADB)
            ContextCompat.startForegroundService(context, intent)
        }

        fun disconnect(context: Context) {
            context.startService(Intent(context, LinkService::class.java).setAction(ACTION_STOP))
        }
    }
}
