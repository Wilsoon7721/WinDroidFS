package dev.wilsoon.windroidfs.link

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.math.BigInteger
import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.Socket
import java.net.SocketException
import java.security.KeyPairGenerator
import java.security.KeyStore
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

/** A PC paired over USB. Only it gets past the TLS handshake on Wi-Fi. */
class PairedPc(val name: String, val certificate: ByteArray)

object Wifi {
    /** Tethering's port, on the Wi-Fi address instead. */
    const val PORT = Tethering.PORT

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

    @Synchronized
    fun forget(context: Context, certificate: ByteArray) {
        write(context, pairedPcs(context).filterNot { it.certificate.contentEquals(certificate) })
    }

    private fun write(context: Context, pcs: List<PairedPc>) {
        pairedFile(context).writeText(pcs.joinToString("") { pc ->
            pc.name + "\t" + pc.certificate.joinToString("") { "%02x".format(it) } + "\n"
        })
    }

    private fun pairedFile(context: Context) = File(context.filesDir, PAIRED_FILE)

    /** TLS 1.3 on [address], and the PC must show a paired certificate. */
    fun serverSocket(context: Context, address: InetAddress): SSLServerSocket {
        val chain = arrayOf(certificate())
        val key = keyStore().getKey(KEY_ALIAS, null) as PrivateKey
        val tls = SSLContext.getInstance("TLS")
        tls.init(arrayOf(IdentityKeys(key, chain)), arrayOf(PairedTrust(context.applicationContext)), null)
        return (tls.serverSocketFactory.createServerSocket(PORT, 1, address) as SSLServerSocket).apply {
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
}
