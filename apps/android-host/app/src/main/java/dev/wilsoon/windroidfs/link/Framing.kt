package dev.wilsoon.windroidfs.link

/**
 * Framing for messages sent over the USB accessory link. Must match `frame.rs` on the PC.
 *
 *     [magic "WD": 2 bytes][payload length: u32 little-endian][payload][pad: 0 or 1 byte]
 */
object Framing {
    const val HEADER_LEN = 6
    const val MAX_PAYLOAD = 16 * 1024 * 1024
    private const val PACKET_ALIGN = 512

    internal const val MAGIC_0: Byte = 'W'.code.toByte()
    internal const val MAGIC_1: Byte = 'D'.code.toByte()

    /** How many padding bytes follow a payload of [payloadLen]. */
    fun padLen(payloadLen: Int): Int =
        if ((HEADER_LEN + payloadLen) % PACKET_ALIGN == 0) 1 else 0

    /** Wrap [payload] in a frame, ready to send in a single write. */
    fun encode(payload: ByteArray): ByteArray {
        val len = payload.size
        val out = ByteArray(HEADER_LEN + len + padLen(len))
        out[0] = MAGIC_0
        out[1] = MAGIC_1
        out[2] = len.toByte()
        out[3] = (len ushr 8).toByte()
        out[4] = (len ushr 16).toByte()
        out[5] = (len ushr 24).toByte()
        payload.copyInto(out, HEADER_LEN)
        return out
    }
}

/** Reassembles frames from the accessory stream: [append] each read, then [next] until null. */
class FrameReader {
    private var buf = ByteArray(64 * 1024)
    private var size = 0

    /** Garbage bytes discarded while resyncing. */
    var skippedBytes: Long = 0

    fun append(data: ByteArray, count: Int) {
        if (size + count > buf.size) {
            buf = buf.copyOf(maxOf(buf.size * 2, size + count))
        }
        System.arraycopy(data, 0, buf, size, count)
        size += count
    }

    /** The next complete payload, or null if more bytes are needed. */
    fun next(): ByteArray? {
        while (true) {
            if (size < 2) return null

            if (buf[0] != Framing.MAGIC_0 || buf[1] != Framing.MAGIC_1) {
                resync()
                continue
            }
            if (size < Framing.HEADER_LEN) return null

            val len = (buf[2].toInt() and 0xFF) or
                ((buf[3].toInt() and 0xFF) shl 8) or
                ((buf[4].toInt() and 0xFF) shl 16) or
                ((buf[5].toInt() and 0xFF) shl 24)
            // Over the limit (or overflowed into negative): not a real frame start.
            if (len < 0 || len > Framing.MAX_PAYLOAD) {
                skip(1)
                continue
            }

            val total = Framing.HEADER_LEN + len + Framing.padLen(len)
            if (size < total) return null

            val payload = buf.copyOfRange(Framing.HEADER_LEN, Framing.HEADER_LEN + len)
            consume(total)
            return payload
        }
    }

    /** Drop bytes up to the next possible magic. Always makes progress. */
    private fun resync() {
        var i = 1
        while (i < size - 1) {
            if (buf[i] == Framing.MAGIC_0 && buf[i + 1] == Framing.MAGIC_1) {
                skip(i)
                return
            }
            i++
        }
        // Keep the last byte if it could be the first half of a magic.
        skip(if (buf[size - 1] == Framing.MAGIC_0) size - 1 else size)
    }

    private fun skip(n: Int) {
        skippedBytes += n
        consume(n)
    }

    private fun consume(n: Int) {
        if (n <= 0) return
        System.arraycopy(buf, n, buf, 0, size - n)
        size -= n
    }
}
