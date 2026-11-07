package dev.wilsoon.windroidfs.ui

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PortCheckTest {
    @Test
    fun refusesPortsThatCantWork() {
        for (port in listOf("", "0", "80", "443", "1023", "65536", "9998")) {
            assertFalse(port, checkPort(port).usable)
        }
        assertTrue(checkPort("80").message.contains("below 1024"))
        assertTrue(checkPort("9998").message.contains("pair"))
    }

    @Test
    fun acceptsTheRestButWarnsAboutAdb() {
        for (port in listOf("1024", "8080", "9999", "65535")) {
            val check = checkPort(port)
            assertTrue(port, check.usable)
            assertFalse(port, check.warning)
        }
        assertTrue(checkPort("5555").usable)
        assertTrue(checkPort("5555").warning)
    }
}
