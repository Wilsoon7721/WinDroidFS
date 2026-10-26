package dev.wilsoon.windroidfs.link

import com.google.protobuf.ByteString
import dev.wilsoon.windroidfs.protocol.WinDroidProto.CreateRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.DeleteRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.ErrorCode
import dev.wilsoon.windroidfs.protocol.WinDroidProto.FileReadRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.FlushRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.OperationResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.RenameRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.TruncateRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.WriteRequest
import java.io.File
import java.io.IOException
import java.nio.file.Files
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** Against a temporary folder standing in for shared storage. */
class FileServerTest {
    private lateinit var root: File
    private lateinit var server: FileServer
    private val changed = mutableListOf<String>()

    @Before
    fun setUp() {
        root = Files.createTempDirectory("fileserver").toFile()
        server = FileServer(root) { changed += it.name }
    }

    @After
    fun tearDown() {
        server.close()
        root.deleteRecursively()
    }

    private var nextId = 1

    private fun op(build: Packet.Builder.() -> Unit): OperationResponse =
        server.handle(Packet.newBuilder().setRequestId(nextId++).apply(build).build()).opResponse

    private fun create(path: String, dir: Boolean = false) =
        op { setCreateRequest(CreateRequest.newBuilder().setPath(path).setIsDirectory(dir)) }

    private fun write(path: String, offset: Long, data: ByteArray) = op {
        setWriteRequest(
            WriteRequest.newBuilder().setPath(path).setOffset(offset)
                .setData(ByteString.copyFrom(data)),
        )
    }

    private fun flush(path: String) = op { setFlushRequest(FlushRequest.newBuilder().setPath(path)) }

    private fun delete(path: String) = op { setDeleteRequest(DeleteRequest.newBuilder().setPath(path)) }

    private fun rename(from: String, to: String, replace: Boolean = false) = op {
        setRenameRequest(
            RenameRequest.newBuilder().setFromPath(from).setToPath(to).setReplaceExisting(replace),
        )
    }

    @Test
    fun createsWritesAndSyncsAFile() {
        assertTrue(create("/new.txt").success)
        val first = write("/new.txt", 0, "hello".toByteArray())
        assertTrue(first.success)
        assertEquals(5, first.bytesWritten)
        assertEquals(5, first.info.sizeBytes)

        // A second write past the end grows the file; the gap is zero-filled.
        assertTrue(write("/new.txt", 7, "!".toByteArray()).success)
        assertTrue(flush("/new.txt").success)

        assertArrayEquals("hello\u0000\u0000!".toByteArray(), File(root, "new.txt").readBytes())
        assertEquals(listOf("new.txt"), changed) // media library told once, on flush
    }

    @Test
    fun readsSeeDataNotYetFlushed() {
        create("/a.txt")
        write("/a.txt", 0, "abc".toByteArray())
        val read = server.handle(
            Packet.newBuilder().setRequestId(9).setFileReq(
                FileReadRequest.newBuilder().setPath("/a.txt").setOffset(0).setMaxBytes(10),
            ).build(),
        ).fileRes
        assertEquals("abc", read.data.toStringUtf8())
    }

    @Test
    fun switchingFilesKeepsBothIntact() {
        create("/a.txt")
        create("/b.txt")
        write("/a.txt", 0, "aaa".toByteArray())
        write("/b.txt", 0, "bbb".toByteArray())
        write("/a.txt", 3, "AAA".toByteArray())
        flush("/a.txt")
        flush("/b.txt")
        assertEquals("aaaAAA", File(root, "a.txt").readText())
        assertEquals("bbb", File(root, "b.txt").readText())
    }

    @Test
    fun truncates() {
        create("/t.txt")
        write("/t.txt", 0, "123456".toByteArray())
        val resp = op { setTruncateRequest(TruncateRequest.newBuilder().setPath("/t.txt").setSize(2)) }
        assertTrue(resp.success)
        assertEquals(2, resp.info.sizeBytes)
        flush("/t.txt")
        assertEquals("12", File(root, "t.txt").readText())
    }

    @Test
    fun refusesBadCreatesAndWrites() {
        File(root, "taken.txt").writeText("x")
        assertEquals(ErrorCode.ALREADY_EXISTS, create("/taken.txt").errorCode)
        assertEquals(ErrorCode.NOT_FOUND, create("/missing/file.txt").errorCode)
        assertEquals(ErrorCode.NOT_FOUND, write("/nope.txt", 0, byteArrayOf(1)).errorCode)
        assertEquals(ErrorCode.INVALID_REQUEST, write("/taken.txt", -1, byteArrayOf(1)).errorCode)
        assertEquals(
            ErrorCode.INVALID_REQUEST,
            write("/taken.txt", 0, ByteArray(FileServer.MAX_WRITE + 1)).errorCode,
        )
        // Writing never creates a file as a side effect.
        assertFalse(File(root, "nope.txt").exists())
    }

    @Test
    fun staysInsideTheRoot() {
        assertEquals(ErrorCode.ACCESS_DENIED, create("/../escaped.txt").errorCode)
        assertFalse(File(root.parentFile, "escaped.txt").exists())
        assertEquals(ErrorCode.ACCESS_DENIED, delete("/").errorCode)
    }

    @Test
    fun deletesFilesAndOnlyEmptyFolders() {
        assertTrue(create("/dir", dir = true).success)
        create("/dir/f.txt")
        write("/dir/f.txt", 0, "x".toByteArray()) // still open for writing when deleted
        assertEquals(ErrorCode.DIRECTORY_NOT_EMPTY, delete("/dir").errorCode)
        assertTrue(delete("/dir/f.txt").success)
        assertTrue(delete("/dir").success)
        assertFalse(File(root, "dir").exists())
        assertEquals(ErrorCode.NOT_FOUND, delete("/dir").errorCode)
    }

    @Test
    fun renames() {
        File(root, "a.txt").writeText("a")
        File(root, "b.txt").writeText("b")
        assertEquals(ErrorCode.ALREADY_EXISTS, rename("/a.txt", "/b.txt").errorCode)
        assertTrue(rename("/a.txt", "/b.txt", replace = true).success)
        assertEquals("a", File(root, "b.txt").readText())
        assertFalse(File(root, "a.txt").exists())

        File(root, "folder").mkdir()
        assertEquals(ErrorCode.ACCESS_DENIED, rename("/b.txt", "/folder", replace = true).errorCode)
        assertTrue(rename("/b.txt", "/folder/c.txt").success)
        assertEquals("a", File(root, "folder/c.txt").readText())
        assertTrue(rename("/folder", "/moved").success)
        assertTrue(File(root, "moved/c.txt").isFile)
    }

    @Test
    fun renameClosesTheOpenFileFirst() {
        create("/w.txt")
        write("/w.txt", 0, "data".toByteArray())
        assertTrue(rename("/w.txt", "/r.txt").success)
        assertEquals("data", File(root, "r.txt").readText())
        // A later write to the new name works, and doesn't recreate the old one.
        assertTrue(write("/r.txt", 4, "!".toByteArray()).success)
        assertFalse(File(root, "w.txt").exists())
    }

    @Test
    fun mapsIoErrorsToCodes() {
        assertEquals(
            ErrorCode.NO_SPACE,
            FileServer.errorCodeOf(IOException("write failed: ENOSPC (No space left on device)")),
        )
        assertEquals(
            ErrorCode.ACCESS_DENIED,
            FileServer.errorCodeOf(IOException("open failed: EACCES (Permission denied)")),
        )
        assertEquals(ErrorCode.IO_ERROR, FileServer.errorCodeOf(IOException("weird")))
    }
}
