package dev.wilsoon.windroidfs.link

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.provider.Settings
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Log
import dev.wilsoon.windroidfs.ui.PairedPcItem
import java.io.File
import java.math.BigInteger
import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.Socket
import java.net.SocketException
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.MessageDigest
import java.security.Principal
import java.security.PrivateKey
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.security.spec.ECGenParameterSpec
import java.util.Date
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLEngine
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.X509ExtendedKeyManager
import javax.net.ssl.X509TrustManager
import javax.security.auth.x500.X500Principal
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** A PC paired over USB. Only it gets past the TLS handshake on Wi-Fi. */
class PairedPc(val name: String, val certificate: ByteArray)

object Wifi {
    /** Settings > TCP port. Tethering keeps 9999: a private cable link the PC can't discover a port on. */
    fun port(context: Context) = AppSettings.load(context).tcpPort

    /** Advertised while waiting on Wi-Fi, so the PC finds this phone wherever the router puts it. Must match PHONE_SERVICE in wifi.rs. */
    const val SERVICE_TYPE = "_windroidfs._tcp"

    private const val KEY_ALIAS = "windroidfs-wifi"
    private const val PAIRED_FILE = "paired-pcs.txt"

    /** This phone on Wi-Fi, or null when it isn't on Wi-Fi. */
    fun address(): InetAddress? = try {
        NetworkInterface.getNetworkInterfaces()?.asSequence()
            ?.filter { it.isUp && it.name.startsWith("wlan") }
            ?.flatMap { it.inetAddresses.asSequence() }
            ?.firstOrNull { it is Inet4Address && !it.isLoopbackAddress && !it.isLinkLocalAddress }
    } catch (_: SocketException) {
        null
    }

    /** Short and stable, for telling certificates apart. Must match fingerprint in wifi.rs. */
    fun fingerprint(certificate: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(certificate).take(8).joinToString("") { "%02x".format(it) }

    /** The name set in Settings > About phone, else the model. */
    fun deviceName(context: Context): String {
        val named = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N_MR1) {
            Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
        } else {
            null
        }
        return named?.takeIf { it.isNotBlank() } ?: Build.MODEL.orEmpty()
    }

    /** On Wi-Fi with a PC to wait for. */
    fun available(context: Context) = address() != null && pairedPcs(context).isNotEmpty()

    private fun keyStore(): KeyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    /** This phone's certificate. Its key is made in the Keystore and never leaves it. */
    fun certificate(): X509Certificate {
        val keyStore = keyStore()
        if (!keyStore.containsAlias(KEY_ALIAS)) {
            val spec = KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_SIGN)
                .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
                .setDigests(KeyProperties.DIGEST_SHA256, KeyProperties.DIGEST_NONE)
                .setCertificateSubject(X500Principal("CN=WinDroidFS"))
                .setCertificateSerialNumber(BigInteger.ONE)
                .setCertificateNotBefore(Date(0))
                .setCertificateNotAfter(Date(4_102_444_800_000)) // 2100: pinned, so expiry means nothing
                .build()
            KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, "AndroidKeyStore")
                .apply { initialize(spec) }
                .generateKeyPair()
        }
        return keyStore.getCertificate(KEY_ALIAS) as X509Certificate
    }

    fun pairedPcs(context: Context): List<PairedPc> =
        pairedFile(context).takeIf { it.exists() }?.readLines()?.mapNotNull { line ->
            val (name, hex) = line.split('\t').takeIf { it.size == 2 } ?: return@mapNotNull null
            PairedPc(name, hex.chunked(2).map { it.toInt(16).toByte() }.toByteArray())
        } ?: emptyList()

    /** Replaces an earlier pairing with the same certificate. */
    @Synchronized
    fun pair(context: Context, name: String, certificate: ByteArray) {
        val others = pairedPcs(context).filterNot { it.certificate.contentEquals(certificate) }
        write(context, others + PairedPc(name.replace(Regex("[\t\r\n]"), " "), certificate))
    }

    /** [id] is the PC's [fingerprint]. */
    @Synchronized
    fun forget(context: Context, id: String) {
        write(context, pairedPcs(context).filterNot { fingerprint(it.certificate) == id })
    }

    private fun write(context: Context, pcs: List<PairedPc>) {
        pairedFile(context).writeText(pcs.joinToString("") { pc ->
            pc.name + "\t" + pc.certificate.joinToString("") { "%02x".format(it) } + "\n"
        })
    }

    private fun pairedFile(context: Context) = File(context.filesDir, PAIRED_FILE)

    /** TLS 1.3 on [address], and the PC must show a paired certificate. [anyPc] only for pairing by code, where comparing codes is the check. */
    fun serverSocket(context: Context, address: InetAddress, port: Int = port(context), anyPc: Boolean = false): SSLServerSocket {
        val chain = arrayOf(certificate())
        val key = keyStore().getKey(KEY_ALIAS, null) as PrivateKey
        val trust = if (anyPc) AnyPc else PairedTrust(context.applicationContext)
        val tls = SSLContext.getInstance("TLS")
        tls.init(arrayOf(IdentityKeys(key, chain)), arrayOf(trust), null)
        return (tls.serverSocketFactory.createServerSocket(port, 1, address) as SSLServerSocket).apply {
            needClientAuth = true
            enabledProtocols = arrayOf("TLSv1.3")
        }
    }

    private class IdentityKeys(
        private val key: PrivateKey,
        private val chain: Array<X509Certificate>,
    ) : X509ExtendedKeyManager() {
        private fun ec(keyType: String?) = if (keyType == null || keyType.startsWith("EC")) KEY_ALIAS else null

        override fun chooseServerAlias(keyType: String?, issuers: Array<out Principal>?, socket: Socket?) = ec(keyType)
        override fun chooseEngineServerAlias(keyType: String?, issuers: Array<out Principal>?, engine: SSLEngine?) =
            ec(keyType)
        override fun getServerAliases(keyType: String?, issuers: Array<out Principal>?) = ec(keyType)?.let { arrayOf(it) }
        override fun getCertificateChain(alias: String?) = chain
        override fun getPrivateKey(alias: String?) = key
        override fun getClientAliases(keyType: String?, issuers: Array<out Principal>?): Array<String>? = null
        override fun chooseClientAlias(keyType: Array<out String>?, issuers: Array<out Principal>?, socket: Socket?): String? =
            null
    }

    /** Read on every handshake, so forgetting a PC takes effect at once. */
    private class PairedTrust(private val context: Context) : X509TrustManager {
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            val presented = chain?.firstOrNull()?.encoded ?: throw CertificateException("No certificate")
            if (pairedPcs(context).none { it.certificate.contentEquals(presented) }) {
                throw CertificateException("Not a paired PC")
            }
        }

        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) =
            throw CertificateException("This side only checks PCs")

        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    private object AnyPc : X509TrustManager {
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            if (chain.isNullOrEmpty()) throw CertificateException("No certificate")
        }

        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) =
            throw CertificateException("This side only checks PCs")

        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }
}

/** Announces a service on the local network (DNS-SD) until [stop]. */
class Advert(context: Context, type: String, name: String, port: Int, attributes: Map<String, String>) {
    private val nsd = context.getSystemService(NsdManager::class.java)
    private val listener = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(info: NsdServiceInfo) {}
        override fun onRegistrationFailed(info: NsdServiceInfo, code: Int) {
            Log.w("WinDroid", "Couldn't advertise $type (error $code)")
        }
        override fun onServiceUnregistered(info: NsdServiceInfo) {}
        override fun onUnregistrationFailed(info: NsdServiceInfo, code: Int) {}
    }

    init {
        val info = NsdServiceInfo().apply {
            serviceName = name
            serviceType = type
            this.port = port
            attributes.forEach { (key, value) -> setAttribute(key, value) }
        }
        nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
    }

    fun stop() {
        try {
            nsd.unregisterService(listener)
        } catch (_: IllegalArgumentException) {
        }
    }
}

/** What the Wi-Fi card shows. */
data class WifiInfo(val address: String? = null, val pairedPcs: List<PairedPcItem> = emptyList())

object WifiState {
    private val mutableInfo = MutableStateFlow(WifiInfo())
    val info: StateFlow<WifiInfo> = mutableInfo.asStateFlow()

    fun refresh(context: Context) {
        mutableInfo.value = WifiInfo(
            address = Wifi.address()?.hostAddress,
            pairedPcs = Wifi.pairedPcs(context).map { PairedPcItem(it.name, Wifi.fingerprint(it.certificate)) },
        )
    }
}
