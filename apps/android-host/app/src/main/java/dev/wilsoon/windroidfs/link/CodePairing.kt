package dev.wilsoon.windroidfs.link

import android.content.Context
import android.util.Log
import com.google.protobuf.ByteString
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.PairCommit
import dev.wilsoon.windroidfs.protocol.WinDroidProto.PairResult
import dev.wilsoon.windroidfs.protocol.WinDroidProto.PairReveal
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Pong
import dev.wilsoon.windroidfs.ui.PairingUi
import java.io.IOException
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Pairing over Wi-Fi without a cable: both screens show a 6-digit code and both owners confirm it matches.
 * This side commits to its nonce before seeing the PC's, so nobody in the middle can make the two codes agree.
 */
object CodePairing {
    /** Must match PAIRING_SERVICE in pairing.rs. */
    private const val SERVICE_TYPE = "_windroidfs-pair._tcp"
    const val PORT = 9998
    /** For a PC to turn up, and then for the owner to compare codes. */
    private const val WAIT_MS = 120_000
    private const val TAG = "WinDroid"

    private val mutableState = MutableStateFlow<PairingUi?>(null)
    val state: StateFlow<PairingUi?> = mutableState.asStateFlow()

    private val answers = LinkedBlockingQueue<Boolean>(1)
    @Volatile private var server: SSLServerSocket? = null
    @Volatile private var advert: Advert? = null

    /** Wait in the background for one PC to pair. */
    fun start(context: Context) {
        if (server != null) return
        val app = context.applicationContext
        val address = Wifi.address() ?: run {
            mutableState.value = PairingUi.Finished(false, "Connect to Wi-Fi first.")
            return
        }
        val socket = try {
            Wifi.serverSocket(app, address, PORT, anyPc = true)
        } catch (e: Exception) {
            Log.e(TAG, "Couldn't listen for pairing", e)
            mutableState.value = PairingUi.Finished(false, "Couldn't start pairing: ${e.message}")
            return
        }
        socket.soTimeout = WAIT_MS
        server = socket
        answers.clear()
        val certificate = Wifi.certificate().encoded
        val name = Wifi.deviceName(app)
        advert = Advert(app, SERVICE_TYPE, name, PORT, mapOf("id" to Wifi.fingerprint(certificate), "name" to name))
        mutableState.value = PairingUi.Waiting
        Thread({ run(app, socket, certificate) }, "windroid-pairing").start()
    }

    /** The owner's verdict on the code. */
    fun answer(accepted: Boolean) {
        val shown = mutableState.value as? PairingUi.Confirm ?: return
        mutableState.value = shown.copy(answered = true)
        answers.offer(accepted)
    }

    /** Close the dialog, and stop waiting if it still is. */
    fun dismiss() {
        mutableState.value = null
        answers.offer(false)
        stopListening()
    }

    private fun stopListening() {
        advert?.stop()
        advert = null
        try {
            server?.close()
        } catch (_: IOException) {
        }
        server = null
    }

    /** Ignored once dismissed. */
    private fun finish(paired: Boolean, message: String) {
        if (mutableState.value != null) mutableState.value = PairingUi.Finished(paired, message)
    }

    private fun run(context: Context, socket: SSLServerSocket, phoneCert: ByteArray) {
        try {
            val client = socket.accept() as SSLSocket
            stopListening() // one PC at a time
            client.use { pair(context, it, phoneCert) }
        } catch (_: SocketTimeoutException) {
            finish(false, "No PC tried to pair in time.")
        } catch (e: IOException) {
            finish(false, "Pairing broke off: ${e.message}")
        } finally {
            stopListening()
        }
    }

    private fun pair(context: Context, client: SSLSocket, phoneCert: ByteArray) {
        client.soTimeout = WAIT_MS + 30_000
        client.startHandshake()
        val pcCert = client.session.peerCertificates.first().encoded
        val input = client.inputStream
        val output = client.outputStream
        val frames = FrameReader()
        val buffer = ByteArray(16 * 1024)

        fun next(): Packet {
            while (true) {
                frames.next()?.let { return Packet.parseFrom(it) }
                val n = input.read(buffer)
                if (n < 0) throw IOException("the PC hung up")
                frames.append(buffer, n)
            }
        }

        fun reply(request: Packet, build: (Packet.Builder) -> Unit) {
            val packet = Packet.newBuilder().setRequestId(request.requestId).also(build).build()
            output.write(Framing.encode(packet.toByteArray()))
            output.flush()
        }

        val ours = ByteArray(32).also { SecureRandom().nextBytes(it) }
        var host = "PC"
        while (true) {
            val request = next()
            when {
                // While its owner looks at the code, the PC checks this side is still there.
                request.hasPing() -> reply(request) {
                    it.setPong(Pong.newBuilder().setDeviceName(Wifi.deviceName(context)))
                }

                request.hasPairHello() -> {
                    host = request.pairHello.hostName.ifEmpty { "PC" }
                    reply(request) {
                        it.setPairCommit(
                            PairCommit.newBuilder()
                                .setCommitment(ByteString.copyFrom(commitment(phoneCert, pcCert, ours)))
                                .setDeviceName(Wifi.deviceName(context)),
                        )
                    }
                }

                request.hasPairNonce() -> {
                    val code = code(pcCert, phoneCert, request.pairNonce.nonce.toByteArray(), ours)
                    mutableState.value = PairingUi.Confirm(host, code)
                    reply(request) { it.setPairReveal(PairReveal.newBuilder().setNonce(ByteString.copyFrom(ours))) }
                }

                request.hasPairConfirm() -> {
                    val result = PairResult.newBuilder()
                    when {
                        !request.pairConfirm.accepted -> {
                            result.setSuccess(false).setErrorMessage("Cancelled on the PC")
                            finish(false, "Cancelled on $host.")
                        }
                        answers.poll(WAIT_MS.toLong(), TimeUnit.MILLISECONDS) == true -> {
                            Wifi.pair(context, host, pcCert)
                            WifiState.refresh(context)
                            result.setSuccess(true).setWifiPort(Wifi.port(context))
                            finish(true, "Paired with $host. It can connect over Wi-Fi from now on.")
                        }
                        else -> {
                            result.setSuccess(false).setErrorMessage("Declined on the phone")
                            finish(false, "Not paired.")
                        }
                    }
                    reply(request) { it.setPairResult(result) }
                    return
                }

                else -> reply(request) {}
            }
        }
    }

    /** SHA-256 of [label], then each part as a 4-byte big-endian length and its bytes. Must match digest in pairing.rs. */
    private fun digest(label: String, vararg parts: ByteArray): ByteArray {
        val sha = MessageDigest.getInstance("SHA-256")
        sha.update(label.toByteArray())
        for (part in parts) {
            sha.update(ByteBuffer.allocate(4).putInt(part.size).array())
            sha.update(part)
        }
        return sha.digest()
    }

    internal fun commitment(phoneCert: ByteArray, pcCert: ByteArray, phoneNonce: ByteArray) =
        digest("windroidfs-commit", phoneCert, pcCert, phoneNonce)

    /** Six digits as "123 456". */
    internal fun code(pcCert: ByteArray, phoneCert: ByteArray, pcNonce: ByteArray, phoneNonce: ByteArray): String {
        val hash = digest("windroidfs-code", pcCert, phoneCert, pcNonce, phoneNonce)
        val n = (ByteBuffer.wrap(hash).int.toLong() and 0xffffffffL) % 1_000_000
        return "%03d %03d".format(n / 1000, n % 1000)
    }
}
