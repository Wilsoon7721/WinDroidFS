package dev.wilsoon.windroidfs.link

import dev.wilsoon.windroidfs.protocol.WinDroidProto.CreateRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.DirectoryRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.ErrorCode
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.RenameRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.StatRequest
import java.io.File
import java.nio.file.Files
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** Settings > Storage scope, as the PC meets it. */
class FileServerScopeTest {
    private lateinit var root: File
    private var exposed: Set<String>? = setOf("dcim", "download")
    private lateinit var server: FileServer

    @Before
    fun setUp() {
        root = Files.createTempDirectory("scope").toFile()
        File(root, "DCIM/Camera").mkdirs()
        File(root, "Documents").mkdirs()
        File(root, "Documents/secret.txt").writeText("no")
        File(root, "root-file.txt").writeText("no")
        server = FileServer(root, exposedFolders = { exposed })
    }

    @After
    fun tearDown() {
        root.deleteRecursively()
    }

    private fun send(build: Packet.Builder.() -> Unit) = server.handle(Packet.newBuilder().setRequestId(1).apply(build).build())

    private fun list(path: String) = send { setDirRequest(DirectoryRequest.newBuilder().setPath(path)) }.dirResponse

    private fun stat(path: String) = send { setStatRequest(StatRequest.newBuilder().setPath(path)) }.statResponse

    @Test
    fun theRootListsOnlyExposedFolders() {
        assertEquals(listOf("DCIM"), list("/").filesList.map { it.name })
        assertTrue(list("/DCIM").success)
    }

    @Test
    fun hiddenFoldersAreNotFound() {
        assertEquals(ErrorCode.NOT_FOUND, list("/Documents").errorCode)
        assertEquals(ErrorCode.NOT_FOUND, stat("/Documents/secret.txt").errorCode)
        assertEquals(ErrorCode.NOT_FOUND, stat("/documents/SECRET.txt").errorCode) // case doesn't help
        assertEquals(ErrorCode.NOT_FOUND, stat("/root-file.txt").errorCode)
        assertEquals(ErrorCode.NOT_FOUND, stat("/DCIM/../Documents/secret.txt").errorCode)
    }

    @Test
    fun nothingMovesInOrOutOfAHiddenFolder() {
        val create = send { setCreateRequest(CreateRequest.newBuilder().setPath("/Documents/new.txt")) }.opResponse
        assertFalse(create.success)
        val out = send {
            setRenameRequest(RenameRequest.newBuilder().setFromPath("/Documents/secret.txt").setToPath("/DCIM/got.txt"))
        }.opResponse
        assertFalse(out.success)
        assertTrue(File(root, "Documents/secret.txt").exists())
    }

    @Test
    fun theWholeStorageShowsEverything() {
        exposed = null
        assertEquals(setOf("DCIM", "Documents", "root-file.txt"), list("/").filesList.map { it.name }.toSet())
    }
}
