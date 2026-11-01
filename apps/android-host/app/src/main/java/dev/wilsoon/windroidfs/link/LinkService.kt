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
import android.media.MediaScannerConnection
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import android.provider.Settings
import android.util.Log
import androidx.core.content.ContextCompat
import com.google.protobuf.InvalidProtocolBufferException
import dev.wilsoon.windroidfs.MainActivity
import dev.wilsoon.windroidfs.R
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.UsbSpeed
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
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
}

/** Foreground service for USB link */
class LinkService : Service() {
    private var accessoryFd: ParcelFileDescriptor? = null
    @Volatile private var listener: ServerSocket? = null
    @Volatile private var socket: Socket? = null
    @Volatile private var transport = LinkTransport.ACCESSORY
    private var tetherAddress: InetAddress? = null
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

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }

        // Required promptly after startForegroundService, whatever happens next.
        if (!enterForeground()) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (running) return START_NOT_STICKY

        val opened = if (intent?.action == ACTION_TETHER) {
            listen()
        } else {
            intent?.accessoryExtra()?.let(::open) ?: false
        }
        if (!opened) stopSelf()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        try {
            accessoryFd?.close()
        } catch (_: IOException) {
        }
        accessoryFd = null
        // Unblocks accept() and read() on the link thread
        try {
            listener?.close()
            socket?.close()
        } catch (_: IOException) {
        }
        ticker?.shutdownNow()
        ticker = null
        try {
            unregisterReceiver(detachReceiver)
        } catch (_: IllegalArgumentException) {
        }
        wakeLock?.takeIf { it.isHeld }?.release()
        LinkState.publish(null)
        super.onDestroy()
    }

    private fun open(accessory: UsbAccessory): Boolean {
        val usbManager = getSystemService(USB_SERVICE) as UsbManager
        val fd = try {
            usbManager.openAccessory(accessory)
        } catch (e: Exception) {
            Log.e(TAG, "Couldn't open the accessory", e)
            null
        } ?: return false

        accessoryFd = fd
        ContextCompat.registerReceiver(
            this,
            detachReceiver,
            IntentFilter(UsbManager.ACTION_USB_ACCESSORY_DETACHED),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        begin(LinkTransport.ACCESSORY) {
            serve(FileInputStream(fd.fileDescriptor), FileOutputStream(fd.fileDescriptor))
        }
        return true
    }

    /** Wait for the PC on the USB tethering link. One PC at a time: the listener closes once it's in */
    private fun listen(): Boolean {
        val address = Tethering.address() ?: return false
        val server = try {
            ServerSocket(Tethering.PORT, 1, address)
        } catch (e: IOException) {
            Log.e(TAG, "Couldn't listen on $address", e)
            return false
        }
        listener = server
        tetherAddress = address
        begin(LinkTransport.TETHERING) {
            val client = try {
                server.accept()
            } catch (e: IOException) {
                Log.i(TAG, "Stopped listening: ${e.message}")
                mainHandler.post { stopSelf() }
                return@begin
            } finally {
                try {
                    server.close()
                } catch (_: IOException) {
                }
            }
            socket = client
            client.tcpNoDelay = true
            serve(client.getInputStream(), client.getOutputStream())
        }
        return true
    }

    private fun begin(how: LinkTransport, link: () -> Unit) {
        transport = how
        running = true
        pcSeen = false
        bytesRead.set(0)
        bytesWritten.set(0)
        Thread(link, "windroid-link").start()
        ticker = Executors.newSingleThreadScheduledExecutor().apply {
            scheduleWithFixedDelay(::publishStatus, 0, 1, TimeUnit.SECONDS)
        }
        Log.d(TAG, "Waiting for the PC over $how")
    }

    private fun serve(input: InputStream, output: OutputStream) {
        val frames = FrameReader()
        val fileServer = FileServer(deviceName = deviceName(), onFileChanged = { file ->
            MediaScannerConnection.scanFile(applicationContext, arrayOf(file.path), null, null)
        })
        server = fileServer

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
                    if (!pcSeen) pcFound()
                    if (request.hasPing()) {
                        if (request.ping.hostName.isNotEmpty()) hostName = request.ping.hostName
                        usbSpeed = request.ping.usbSpeed
                    }

                    // A bug in one request should not crash - reply with just the ID.
                    handlingRequest = true
                    val reply = try {
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

                    if (request.hasGoodbye()) {
                        Log.i(TAG, "The PC disconnected")
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
            fileServer.close()
        } catch (e: IOException) {
            Log.w(TAG, "Couldn't sync the file being written: ${e.message}")
        }
        server = null
        mainHandler.post { stopSelf() }
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
            if (transport == LinkTransport.TETHERING && Tethering.address() != tetherAddress) {
                Log.i(TAG, "USB tethering ended")
                running = false
                mainHandler.post { stopSelf() }
                return
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
            running = false
            mainHandler.post { stopSelf() }
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
            stopSelf()
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
        private const val ACTION_STOP = "dev.wilsoon.windroidfs.action.DISCONNECT"
        private const val ACTION_TETHER = "dev.wilsoon.windroidfs.action.TETHER"

        fun connect(context: Context, accessory: UsbAccessory) {
            val intent = Intent(context, LinkService::class.java).putExtra(UsbManager.EXTRA_ACCESSORY, accessory)
            ContextCompat.startForegroundService(context, intent)
        }

        /** Call from the visible activity, while `Tethering.address` isn't null. */
        fun listenOnTethering(context: Context) {
            val intent = Intent(context, LinkService::class.java).setAction(ACTION_TETHER)
            ContextCompat.startForegroundService(context, intent)
        }

        fun disconnect(context: Context) {
            context.stopService(Intent(context, LinkService::class.java))
        }
    }
}
