package dev.wilsoon.windroidfs.link

import android.os.Build
import android.os.Environment
import android.os.StatFs
import com.google.protobuf.ByteString
import dev.wilsoon.windroidfs.protocol.WinDroidProto.CreateRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.DirectoryResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.ErrorCode
import dev.wilsoon.windroidfs.protocol.WinDroidProto.FileInfo
import dev.wilsoon.windroidfs.protocol.WinDroidProto.FileReadRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.FileReadResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Goodbye
import dev.wilsoon.windroidfs.protocol.WinDroidProto.OperationResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Packet
import dev.wilsoon.windroidfs.protocol.WinDroidProto.Pong
import dev.wilsoon.windroidfs.protocol.WinDroidProto.RenameRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.SetTimesRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.StatResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.TruncateRequest
import dev.wilsoon.windroidfs.protocol.WinDroidProto.VolumeInfoResponse
import dev.wilsoon.windroidfs.protocol.WinDroidProto.WriteRequest
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile

/** Progress of the file being written. [bytesTotal] is its current size, which Explorer sets up front. */
data class WriteProgress(val fileName: String, val bytesDone: Long, val bytesTotal: Long)

/**
 * Answers the PC's file requests against shared storage. Every path is checked to stay inside
 * [root]. Not thread-safe: requests arrive one at a time from the USB thread.
 *
 * @param deviceName sent in every pong, so the PC can name the drive.
 * @param onFileChanged called after a file is written, deleted or renamed, for the media library.
 */
class FileServer(
    root: File = Environment.getExternalStorageDirectory(),
    private val deviceName: String = "",
    private val onFileChanged: (File) -> Unit = {},
) {
    private val rootPath: String = root.canonicalPath

    /** The file being written, kept open between requests until the PC flushes it. */
    private var writer: OpenWriter? = null

    private class OpenWriter(val file: File, val raf: RandomAccessFile) {
        /** The furthest point written so far. */
        var end = 0L
    }

    /** For the dashboard. Read from another thread. */
    @Volatile
    var currentWrite: WriteProgress? = null
        private set

    /** Build the reply to [request]. Always echoes its `request_id`. */
    fun handle(request: Packet): Packet {
        val reply = Packet.newBuilder().setRequestId(request.requestId)
        when {
            request.hasPing() ->
                reply.setPong(
                    Pong.newBuilder().setTimestampMs(request.ping.timestampMs)
                        .setDeviceName(deviceName),
                )

            request.hasDirRequest() -> reply.setDirResponse(listDirectory(request.dirRequest.path))
            request.hasStatRequest() -> reply.setStatResponse(stat(request.statRequest.path))
            request.hasFileReq() -> reply.setFileRes(read(request.fileReq))
            request.hasVolumeRequest() -> reply.setVolumeResponse(volumeInfo())
            request.hasGoodbye() -> reply.setGoodbye(Goodbye.getDefaultInstance())
            request.hasCreateRequest() -> reply.setOpResponse(create(request.createRequest))
            request.hasWriteRequest() -> reply.setOpResponse(write(request.writeRequest))
            request.hasTruncateRequest() -> reply.setOpResponse(truncate(request.truncateRequest))
            request.hasDeleteRequest() -> reply.setOpResponse(delete(request.deleteRequest.path))
            request.hasRenameRequest() -> reply.setOpResponse(rename(request.renameRequest))
            request.hasSetTimesRequest() -> reply.setOpResponse(setTimes(request.setTimesRequest))
            request.hasFlushRequest() -> reply.setOpResponse(flush(request.flushRequest.path))
            // Unknown request: reply with just the id, so the PC fails fast.
            else -> Unit
        }
        return reply.build()
    }

    /** Close the file being written, if any. Call when the link ends. */
    fun close() {
        closeWriter()
    }

    /** The file at [path], or null if it would point outside shared storage. */
    private fun resolve(path: String): File? {
        val canonical = try {
            File(rootPath, path.trimStart('/')).canonicalPath
        } catch (e: IOException) {
            return null
        }
        // File.separator, so the check also works in JVM tests on Windows.
        return if (canonical == rootPath || canonical.startsWith(rootPath + File.separator)) {
            File(canonical)
        } else {
            null
        }
    }

    /** Android 11+ hides `Android/data` and `Android/obb` even with All Files Access. */
    private fun isRestricted(dir: File): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return false
        // Case-insensitive, as Windows is.
        val path = dir.path
        return path.equals(File(rootPath, "Android/data").path, ignoreCase = true) ||
                path.equals(File(rootPath, "Android/obb").path, ignoreCase = true)
    }

    private fun listDirectory(path: String): DirectoryResponse {
        val dir = resolve(path)
            ?: return dirError(ErrorCode.ACCESS_DENIED, "That path is outside shared storage")
        if (!dir.exists()) return dirError(ErrorCode.NOT_FOUND, "No such folder")
        if (!dir.isDirectory) return dirError(ErrorCode.NOT_A_DIRECTORY, "Not a folder")
        if (isRestricted(dir)) {
            return dirError(
                ErrorCode.ACCESS_DENIED,
                "Android doesn't let apps read Android/data or Android/obb",
            )
        }

        // null means Android refused to list it, which is not the same as empty.
        val children = dir.listFiles()
            ?: return dirError(ErrorCode.ACCESS_DENIED, "Android won't let this app list that folder")

        return DirectoryResponse.newBuilder()
            .setSuccess(true)
            .addAllFiles(children.map(::fileInfo))
            .build()
    }

    private fun stat(path: String): StatResponse {
        val file = resolve(path)
            ?: return statError(ErrorCode.ACCESS_DENIED, "That path is outside shared storage")
        if (!file.exists()) return statError(ErrorCode.NOT_FOUND, "No such file or folder")

        val info = fileInfo(file).toBuilder()
            .setName(if (file.path == rootPath) "" else file.name)
            .build()
        return StatResponse.newBuilder().setSuccess(true).setInfo(info).build()
    }

    private fun read(req: FileReadRequest): FileReadResponse {
        if (req.maxBytes <= 0 || req.maxBytes > MAX_READ) {
            return readError(ErrorCode.INVALID_REQUEST, "Read size must be 1 to $MAX_READ bytes")
        }
        if (req.offset < 0) return readError(ErrorCode.INVALID_REQUEST, "Negative offset")

        val file = resolve(req.path)
            ?: return readError(ErrorCode.ACCESS_DENIED, "That path is outside shared storage")
        if (!file.exists()) return readError(ErrorCode.NOT_FOUND, "No such file")
        if (file.isDirectory) return readError(ErrorCode.IS_A_DIRECTORY, "That's a folder")

        return try {
            RandomAccessFile(file, "r").use { raf ->
                val length = raf.length()
                raf.seek(req.offset)
                val chunk = ByteArray(req.maxBytes)
                // read() may return less than asked even mid-file.
                var got = 0
                while (got < chunk.size) {
                    val n = raf.read(chunk, got, chunk.size - got)
                    if (n < 0) break
                    got += n
                }
                FileReadResponse.newBuilder()
                    .setSuccess(true)
                    .setTotalFileSize(length)
                    .setData(ByteString.copyFrom(chunk, 0, got))
                    .setEof(req.offset + got >= length)
                    .build()
            }
        } catch (e: SecurityException) {
            readError(ErrorCode.ACCESS_DENIED, e.message ?: "Access denied")
        } catch (e: IOException) {
            readError(ErrorCode.IO_ERROR, e.message ?: "Read failed")
        }
    }

    private fun create(req: CreateRequest): OperationResponse {
        val file = resolve(req.path) ?: return outsideStorage()
        if (file.exists()) return opError(ErrorCode.ALREADY_EXISTS, "That name is taken")
        if (file.parentFile?.isDirectory != true) {
            return opError(ErrorCode.NOT_FOUND, "The folder it would go in doesn't exist")
        }
        return attempt("Create failed") {
            val made = if (req.isDirectory) file.mkdir() else file.createNewFile()
            if (!made) return opError(ErrorCode.ACCESS_DENIED, "Android refused to create it")
            opOk(file)
        }
    }

    private fun write(req: WriteRequest): OperationResponse {
        if (req.offset < 0) return opError(ErrorCode.INVALID_REQUEST, "Negative offset")
        if (req.data.size() > MAX_WRITE) {
            return opError(ErrorCode.INVALID_REQUEST, "Writes are at most $MAX_WRITE bytes")
        }
        val file = resolve(req.path) ?: return outsideStorage()
        if (!file.exists()) return opError(ErrorCode.NOT_FOUND, "No such file")
        if (file.isDirectory) return opError(ErrorCode.IS_A_DIRECTORY, "That's a folder")

        return attempt("Write failed") {
            val raf = openWriter(file)
            raf.seek(req.offset)
            raf.write(req.data.toByteArray())
            writer?.let { open ->
                open.end = maxOf(open.end, req.offset + req.data.size())
                currentWrite = WriteProgress(file.name, open.end, maxOf(raf.length(), open.end))
            }
            opOk(file, bytesWritten = req.data.size().toLong())
        }
    }

    private fun truncate(req: TruncateRequest): OperationResponse {
        if (req.size < 0) return opError(ErrorCode.INVALID_REQUEST, "Negative size")
        val file = resolve(req.path) ?: return outsideStorage()
        if (!file.exists()) return opError(ErrorCode.NOT_FOUND, "No such file")
        if (file.isDirectory) return opError(ErrorCode.IS_A_DIRECTORY, "That's a folder")

        return attempt("Resize failed") {
            openWriter(file).setLength(req.size)
            opOk(file)
        }
    }

    private fun delete(path: String): OperationResponse {
        val file = resolve(path) ?: return outsideStorage()
        if (file.path == rootPath) return opError(ErrorCode.ACCESS_DENIED, "That's the whole storage")
        if (!file.exists()) return opError(ErrorCode.NOT_FOUND, "No such file or folder")
        if (file.isDirectory && !file.list().isNullOrEmpty()) {
            return opError(ErrorCode.DIRECTORY_NOT_EMPTY, "The folder isn't empty")
        }
        runCatching { closeWriter(file) }

        return attempt("Delete failed") {
            if (!file.delete()) return opError(ErrorCode.ACCESS_DENIED, "Android refused to delete it")
            onFileChanged(file)
            opOk(null)
        }
    }

    private fun rename(req: RenameRequest): OperationResponse {
        val from = resolve(req.fromPath) ?: return outsideStorage()
        val to = resolve(req.toPath) ?: return outsideStorage()
        if (from.path == rootPath) return opError(ErrorCode.ACCESS_DENIED, "That's the whole storage")
        if (!from.exists()) return opError(ErrorCode.NOT_FOUND, "No such file or folder")

        // A case-only rename is the same file on shared storage, not a replacement.
        val sameFile = from.path.equals(to.path, ignoreCase = true)
        if (to.exists() && !sameFile) {
            if (!req.replaceExisting) return opError(ErrorCode.ALREADY_EXISTS, "That name is taken")
            if (to.isDirectory) return opError(ErrorCode.ACCESS_DENIED, "Won't replace a folder")
        }
        if (to.parentFile?.isDirectory != true) {
            return opError(ErrorCode.NOT_FOUND, "The folder it would go in doesn't exist")
        }
        return attempt("Rename failed") {
            closeWriter()
            if (to.exists() && !sameFile && !to.delete()) {
                return opError(ErrorCode.ACCESS_DENIED, "Android refused to replace it")
            }
            if (!from.renameTo(to)) return opError(ErrorCode.ACCESS_DENIED, "Android refused to rename it")
            if (to.isFile) {
                onFileChanged(from)
                onFileChanged(to)
            }
            opOk(to)
        }
    }

    /** Best effort: often refused on shared storage, so always OK if the file exists. */
    private fun setTimes(req: SetTimesRequest): OperationResponse {
        val file = resolve(req.path) ?: return outsideStorage()
        if (!file.exists()) return opError(ErrorCode.NOT_FOUND, "No such file or folder")
        if (req.lastModifiedMs >= 0) {
            file.setLastModified(req.lastModifiedMs)
        }
        return opOk(file)
    }

    /** Close and sync [path], and tell the media library. */
    private fun flush(path: String): OperationResponse {
        val file = resolve(path) ?: return outsideStorage()
        return attempt("Sync failed") {
            closeWriter(file)
            if (!file.exists()) return opError(ErrorCode.NOT_FOUND, "No such file")
            if (file.isFile) onFileChanged(file)
            opOk(file)
        }
    }

    /** The open writer for [file], opening it (and closing any other) if needed. */
    private fun openWriter(file: File): RandomAccessFile {
        writer?.let { if (it.file == file) return it.raf }
        closeWriter()
        val raf = RandomAccessFile(file, "rw")
        writer = OpenWriter(file, raf)
        return raf
    }

    /** Sync and close the open writer (only if it's [file], when given). Throws if the sync fails. */
    private fun closeWriter(file: File? = null) {
        val open = writer ?: return
        if (file != null && open.file != file) return
        writer = null
        currentWrite = null
        open.raf.use { it.fd.sync() }
    }

    private fun volumeInfo(): VolumeInfoResponse {
        val stats = StatFs(rootPath)
        return VolumeInfoResponse.newBuilder()
            .setTotalBytes(stats.totalBytes)
            .setFreeBytes(stats.availableBytes)
            .build()
    }

    private fun fileInfo(file: File): FileInfo = FileInfo.newBuilder()
        .setName(file.name)
        .setSizeBytes(if (file.isDirectory) 0 else file.length())
        .setIsDirectory(file.isDirectory)
        .setLastModifiedMs(file.lastModified())
        .build()

    private fun dirError(code: ErrorCode, message: String): DirectoryResponse =
        DirectoryResponse.newBuilder().setSuccess(false).setErrorCode(code)
            .setErrorMessage(message).build()

    private fun statError(code: ErrorCode, message: String): StatResponse =
        StatResponse.newBuilder().setSuccess(false).setErrorCode(code)
            .setErrorMessage(message).build()

    private fun readError(code: ErrorCode, message: String): FileReadResponse =
        FileReadResponse.newBuilder().setSuccess(false).setErrorCode(code)
            .setErrorMessage(message).build()

    private fun opOk(file: File?, bytesWritten: Long = 0): OperationResponse {
        val reply = OperationResponse.newBuilder().setSuccess(true).setBytesWritten(bytesWritten)
        if (file != null && file.exists()) reply.setInfo(fileInfo(file))
        return reply.build()
    }

    private fun opError(code: ErrorCode, message: String): OperationResponse =
        OperationResponse.newBuilder().setSuccess(false).setErrorCode(code)
            .setErrorMessage(message).build()

    private fun outsideStorage(): OperationResponse =
        opError(ErrorCode.ACCESS_DENIED, "That path is outside shared storage")

    /** Run a write operation, turning its exceptions into error replies. */
    private inline fun attempt(what: String, body: () -> OperationResponse): OperationResponse =
        try {
            body()
        } catch (e: SecurityException) {
            opError(ErrorCode.ACCESS_DENIED, e.message ?: "Access denied")
        } catch (e: IOException) {
            opError(errorCodeOf(e), e.message ?: what)
        }

    companion object {
        /** Must match `MAX_READ` in the PC's phone.rs. */
        const val MAX_READ = 1024 * 1024

        /** Must match `MAX_WRITE` in the PC's phone.rs. */
        const val MAX_WRITE = 1024 * 1024

        /** Java only keeps the errno in the message, e.g. "write failed: ENOSPC (...)". */
        internal fun errorCodeOf(e: IOException): ErrorCode {
            val message = e.message.orEmpty()
            return when {
                "ENOSPC" in message || "No space left" in message -> ErrorCode.NO_SPACE
                "EACCES" in message || "EPERM" in message || "Permission denied" in message ->
                    ErrorCode.ACCESS_DENIED

                else -> ErrorCode.IO_ERROR
            }
        }
    }
}
