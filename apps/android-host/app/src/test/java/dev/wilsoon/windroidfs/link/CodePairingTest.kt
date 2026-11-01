package dev.wilsoon.windroidfs.link

import org.junit.Assert.assertEquals
import org.junit.Test

/** Same inputs and answers as matches_the_phone in pairing.rs, so the two sides can't drift apart. */
class CodePairingTest {

    @Test
    fun matchesThePc() {
        val pcCert = "pc certificate".toByteArray()
        val phoneCert = "phone certificate".toByteArray()
        val pcNonce = ByteArray(32) { 1 }
        val phoneNonce = ByteArray(32) { 2 }

        val commitment = CodePairing.commitment(phoneCert, pcCert, phoneNonce).joinToString("") { "%02x".format(it) }
        assertEquals("dcc2a9b7ac2a4e9a5f0d7f98823ae76179282c59a868ed402d94fe00d029555f", commitment)
        assertEquals("565 043", CodePairing.code(pcCert, phoneCert, pcNonce, phoneNonce))
    }
}
