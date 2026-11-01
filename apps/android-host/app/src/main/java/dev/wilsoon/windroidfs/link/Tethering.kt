package dev.wilsoon.windroidfs.link

import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.SocketException

/** How the PC is linked to this phone. */
enum class LinkTransport { ACCESSORY, TETHERING }

object Tethering {
    /** Where the app listens on the tethering link. Must match PORT in tether.rs. */
    const val PORT = 9999

    // rndis0 on most phones, ncm0 on newer ones, usb0 on a few.
    private val USB_INTERFACE_PREFIXES = listOf("rndis", "ncm", "usb")

    /**
     * This phone's address on the USB tethering link, or null when it isn't sharing its connection over USB.
     */
    fun address(): InetAddress? = try {
        NetworkInterface.getNetworkInterfaces()?.asSequence()?.filter { nic -> nic.isUp && USB_INTERFACE_PREFIXES.any { nic.name.startsWith(it) } }?.flatMap { it.inetAddresses.asSequence() }?.firstOrNull { it is Inet4Address && !it.isLoopbackAddress }
    } catch (_: SocketException) {
        null
    }
}
