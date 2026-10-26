package dev.wilsoon.windroidfs.link

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Mirrors frame.rs's tests. `knownBytes` pins the same bytes as the Rust test of that name. */
class FramingTest {

    private fun bytes(vararg values: Int) = ByteArray(values.size) { values[it].toByte() }

    private fun drainAll(reader: FrameReader): List<ByteArray> {
        val out = mutableListOf<ByteArray>()
        while (true) out.add(reader.next() ?: return out)
    }

    private fun FrameReader.feed(data: ByteArray) = append(data, data.size)

    @Test
    fun knownBytes() {
        assertArrayEquals(bytes(0x57, 0x44, 0, 0, 0, 0), Framing.encode(ByteArray(0)))
        assertArrayEquals(
            bytes(0x57, 0x44, 5, 0, 0, 0, 'h'.code, 'e'.code, 'l'.code, 'l'.code, 'o'.code),
            Framing.encode("hello".toByteArray()),
        )
        // 6 + 506 = 512 would end on a packet boundary, so one zero pad byte is appended.
        val padded = Framing.encode(ByteArray(506) { 1 })
        assertEquals(513, padded.size)
        assertEquals(0, padded.last().toInt())
    }

    @Test
    fun roundTrip() {
        val reader = FrameReader()
        reader.feed(Framing.encode("hello".toByteArray()))
        assertArrayEquals("hello".toByteArray(), reader.next())
        assertNull(reader.next())
    }

    @Test
    fun frameSplitAcrossReads() {
        val frame = Framing.encode(ByteArray(3000) { 7 })
        val reader = FrameReader()
        for (start in frame.indices step 700) {
            assertNull(reader.next())
            reader.feed(frame.copyOfRange(start, minOf(start + 700, frame.size)))
        }
        assertArrayEquals(ByteArray(3000) { 7 }, reader.next())
    }

    @Test
    fun twoFramesInOneRead() {
        val reader = FrameReader()
        reader.feed(Framing.encode("first".toByteArray()) + Framing.encode("second".toByteArray()))
        val frames = drainAll(reader)
        assertEquals(2, frames.size)
        assertArrayEquals("first".toByteArray(), frames[0])
        assertArrayEquals("second".toByteArray(), frames[1])
    }

    @Test
    fun noFrameEverEndsOnAPacketBoundary() {
        for (len in 0 until 5000) {
            val total = Framing.encode(ByteArray(len)).size
            assertTrue("length $len produced a $total-byte frame", total % 512 != 0)
        }
    }

    @Test
    fun paddedFrameLeavesNothingBehind() {
        val reader = FrameReader()
        reader.feed(Framing.encode(ByteArray(506) { 9 }) + Framing.encode("next".toByteArray()))
        val frames = drainAll(reader)
        assertEquals(2, frames.size)
        assertArrayEquals(ByteArray(506) { 9 }, frames[0])
        assertArrayEquals("next".toByteArray(), frames[1])
    }

    @Test
    fun resyncsAfterLeadingGarbage() {
        val reader = FrameReader()
        reader.feed(bytes(1, 2, 3, 4) + Framing.encode("after".toByteArray()))
        assertArrayEquals("after".toByteArray(), reader.next())
        assertEquals(4L, reader.skippedBytes)
    }

    @Test
    fun falseMagicWithAbsurdLengthIsSkipped() {
        val reader = FrameReader()
        reader.feed(bytes(0x57, 0x44, 0xFF, 0xFF, 0xFF, 0xFF) + Framing.encode("real".toByteArray()))
        assertArrayEquals("real".toByteArray(), reader.next())
    }

    @Test
    fun magicSplitAcrossReadsIsNotThrownAway() {
        val frame = Framing.encode("x".toByteArray())
        val reader = FrameReader()
        reader.feed(bytes(0xAA, frame[0].toInt()))
        assertNull(reader.next())
        reader.feed(frame.copyOfRange(1, frame.size))
        assertArrayEquals("x".toByteArray(), reader.next())
        assertEquals(1L, reader.skippedBytes)
    }

    @Test
    fun aCompleteFrameIsNotCountedAsSkipped() {
        val reader = FrameReader()
        reader.feed(Framing.encode("whole".toByteArray()))
        reader.next()
        assertEquals(0L, reader.skippedBytes)
    }
}
