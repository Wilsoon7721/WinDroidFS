use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::adb::AdbPhone;
use crate::phone::{PhoneError, ReadChunk};
use crate::windroid::{ErrorCode, FileInfo};

/// Android 11+ keeps every app out of these, the phone app included, but adb's shell user may enter. Lowercase.
pub const RESTRICTED_FOLDERS: [&str; 2] = ["/android/data", "/android/obb"];

/// Where the phone app's `/` is on the phone.
const STORAGE_ROOT: &str = "/storage/emulated/0";
const ADB_SERVER: &str = "127.0.0.1:5037";
const TIMEOUT: Duration = Duration::from_secs(30);
/// adb refuses bigger DATA packets.
const SYNC_CHUNK: usize = 64 * 1024;
/// How long a downloaded copy is trusted before checking the phone's copy hasn't changed.
const RECHECK_AFTER: Duration = Duration::from_secs(2);
/// rw-r--r--, as a regular file.
const SEND_MODE: u32 = 0o100644;

/// `path` is one of [`RESTRICTED_FOLDERS`] or inside one.
pub fn is_restricted(path: &str) -> bool {
    let path = path.to_lowercase();
    RESTRICTED_FOLDERS.iter().any(|r| {
        path == *r
            || path
                .strip_prefix(r)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// `Android/data` and `Android/obb` reached through adb's file transfer (`adb pull`/`push`) instead of the phone app.
/// That can only move whole files, so a file is downloaded to a temporary copy on first read, written there, and sent back on flush.
pub struct AdbFiles {
    phone: AdbPhone,
    sync: Mutex<Option<TcpStream>>,
    copies: Mutex<HashMap<String, Copy>>,
    temp_dir: PathBuf,
}

/// A file's temporary copy on this PC.
struct Copy {
    file: File,
    path: PathBuf,
    size: u64,
    modified_ms: i64,
    /// Written here and not yet sent.
    dirty: bool,
    checked: Instant,
}

struct Stat {
    mode: u32,
    size: u64,
    mtime_s: i64,
}

impl Stat {
    fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
}

impl AdbFiles {
    pub fn new(phone: AdbPhone) -> AdbFiles {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp_dir = std::env::temp_dir().join(format!(
            "WinDroidFS-adb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        AdbFiles {
            phone,
            sync: Mutex::new(None),
            copies: Mutex::new(HashMap::new()),
            temp_dir,
        }
    }

    pub fn stat(&self, path: &str) -> Result<FileInfo, PhoneError> {
        if let Some(copy) = lock(&self.copies).get(&key(path))
            && copy.dirty
        {
            return Ok(info(path, false, copy.size, copy.modified_ms));
        }
        let stat = self.stat_raw(path)?;
        Ok(info(path, stat.is_dir(), stat.size, stat.mtime_s * 1000))
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, PhoneError> {
        if !self.stat_raw(path)?.is_dir() {
            return Err(remote(ErrorCode::NotADirectory, "Not a folder"));
        }
        let entries = self.with_sync(|s| list(s, &device_path(path)))?;
        let copies = lock(&self.copies);
        Ok(entries
            .into_iter()
            .filter(|(name, _)| name != "." && name != "..")
            .map(|(name, stat)| {
                let child = format!("{}/{name}", path.trim_end_matches('/'));
                match copies.get(&key(&child)).filter(|c| c.dirty) {
                    Some(copy) => info(&child, false, copy.size, copy.modified_ms),
                    None => info(&child, stat.is_dir(), stat.size, stat.mtime_s * 1000),
                }
            })
            .collect())
    }

    pub fn read(&self, path: &str, offset: u64, len: u32) -> Result<ReadChunk, PhoneError> {
        let mut copies = lock(&self.copies);
        let copy = self.copy_of(&mut copies, path, true)?;
        let mut data = vec![0; len as usize];
        copy.file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        let mut got = 0;
        while got < data.len() {
            match copy.file.read(&mut data[got..]).map_err(io_error)? {
                0 => break,
                n => got += n,
            }
        }
        data.truncate(got);
        Ok(ReadChunk {
            eof: offset + got as u64 >= copy.size,
            data,
        })
    }

    pub fn create(&self, path: &str, is_directory: bool) -> Result<FileInfo, PhoneError> {
        match self.stat_raw(path) {
            Ok(_) => return Err(remote(ErrorCode::AlreadyExists, "That name is taken")),
            Err(PhoneError::Remote {
                code: ErrorCode::NotFound,
                ..
            }) => {}
            Err(e) => return Err(e),
        }
        if is_directory {
            self.exec(&format!("mkdir {}", quote(&device_path(path))))?;
        } else {
            self.with_sync(|s| send(s, &device_path(path), &mut io::empty(), now_s()))?;
        }
        self.stat(path)
    }

    pub fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(u64, FileInfo), PhoneError> {
        let mut copies = lock(&self.copies);
        let copy = self.copy_of(&mut copies, path, true)?;
        copy.file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        copy.file.write_all(data).map_err(io_error)?;
        copy.size = copy.size.max(offset + data.len() as u64);
        copy.touch();
        Ok((
            data.len() as u64,
            info(path, false, copy.size, copy.modified_ms),
        ))
    }

    pub fn truncate(&self, path: &str, size: u64) -> Result<FileInfo, PhoneError> {
        let mut copies = lock(&self.copies);
        // Emptying a file needs nothing from the phone.
        let copy = self.copy_of(&mut copies, path, size > 0)?;
        copy.file.set_len(size).map_err(io_error)?;
        copy.size = size;
        copy.touch();
        Ok(info(path, false, copy.size, copy.modified_ms))
    }

    /// Send a file's copy back to the phone if it was written to.
    pub fn flush(&self, path: &str) -> Result<(), PhoneError> {
        let mut copies = lock(&self.copies);
        let Some(copy) = copies.get_mut(&key(path)).filter(|c| c.dirty) else {
            return Ok(());
        };
        let (file, size, mtime) = (&copy.file, copy.size, (copy.modified_ms / 1000) as u32);
        self.with_sync(|s| {
            // From the top again if a retry follows a dropped connection.
            let mut source = file;
            source.seek(SeekFrom::Start(0))?;
            send(s, &device_path(path), &mut source.take(size), mtime)
        })?;
        copy.dirty = false;
        copy.checked = Instant::now();
        Ok(())
    }

    pub fn delete(&self, path: &str) -> Result<(), PhoneError> {
        let stat = self.stat_raw(path)?;
        self.forget(path);
        let command = if stat.is_dir() { "rmdir" } else { "rm" };
        self.exec(&format!("{command} {}", quote(&device_path(path))))
            .map_err(|e| match e {
                PhoneError::Remote { message, .. } if message.contains("not empty") => {
                    remote(ErrorCode::DirectoryNotEmpty, "The folder isn't empty")
                }
                e => e,
            })
    }

    /// Either side may be outside the restricted folders: adb's shell can reach both.
    pub fn rename(&self, from: &str, to: &str, replace_existing: bool) -> Result<(), PhoneError> {
        let same_file = from.eq_ignore_ascii_case(to);
        if !same_file && let Ok(existing) = self.stat_raw(to) {
            if !replace_existing {
                return Err(remote(ErrorCode::AlreadyExists, "That name is taken"));
            }
            if existing.is_dir() {
                return Err(remote(ErrorCode::AccessDenied, "Won't replace a folder"));
            }
        }
        self.flush(from)?;
        self.forget(from);
        self.forget(to);
        self.exec(&format!(
            "mv -f {} {}",
            quote(&device_path(from)),
            quote(&device_path(to))
        ))
    }

    pub fn set_modified(&self, path: &str, modified_ms: i64) -> Result<(), PhoneError> {
        if let Some(copy) = lock(&self.copies).get_mut(&key(path)) {
            copy.modified_ms = modified_ms;
        }
        let _ = self.exec(&format!(
            "touch -m -d @{} {}",
            modified_ms / 1000,
            quote(&device_path(path))
        ));
        Ok(())
    }

    /// The temporary copy of `path`, downloaded (again) when `fetch` and it's missing or the phone's changed.
    fn copy_of<'a>(
        &self,
        copies: &'a mut MutexGuard<'_, HashMap<String, Copy>>,
        path: &str,
        fetch: bool,
    ) -> Result<&'a mut Copy, PhoneError> {
        let k = key(path);
        let fresh = match copies.get_mut(&k) {
            Some(copy) if copy.dirty || copy.checked.elapsed() < RECHECK_AFTER => true,
            Some(copy) if fetch => match self.stat_raw(path) {
                Ok(stat) if stat.size == copy.size && stat.mtime_s * 1000 == copy.modified_ms => {
                    copy.checked = Instant::now();
                    true
                }
                _ => false,
            },
            _ => false,
        };
        if !fresh {
            let copy = self.download(path, fetch)?;
            if let Some(old) = copies.insert(k.clone(), copy) {
                let _ = fs::remove_file(old.path);
            }
        }
        Ok(copies.get_mut(&k).expect("just inserted"))
    }

    fn download(&self, path: &str, fetch: bool) -> Result<Copy, PhoneError> {
        fs::create_dir_all(&self.temp_dir).map_err(io_error)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = self
            .temp_dir
            .join(format!("{}.bin", NEXT.fetch_add(1, Ordering::SeqCst)));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)
            .map_err(io_error)?;
        let (size, modified_ms) = if fetch {
            let stat = self.stat_raw(path)?;
            if stat.is_dir() {
                return Err(remote(ErrorCode::IsADirectory, "That's a folder"));
            }
            self.with_sync(|s| {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                recv(s, &device_path(path), &mut file)
            })?;
            (stat.size, stat.mtime_s * 1000)
        } else {
            (0, now_s() as i64 * 1000)
        };
        Ok(Copy {
            file,
            path: temp,
            size,
            modified_ms,
            dirty: !fetch,
            checked: Instant::now(),
        })
    }

    fn forget(&self, path: &str) {
        if let Some(copy) = lock(&self.copies).remove(&key(path)) {
            let _ = fs::remove_file(copy.path);
        }
    }

    fn stat_raw(&self, path: &str) -> Result<Stat, PhoneError> {
        let (errno, stat) = self.with_sync(|s| stat(s, &device_path(path)))?;
        match errno {
            0 => Ok(stat),
            2 => Err(remote(ErrorCode::NotFound, "No such file or folder")),
            13 | 1 => Err(remote(
                ErrorCode::AccessDenied,
                "adb isn't allowed there either",
            )),
            n => Err(remote(
                ErrorCode::IoError,
                &format!("stat failed (errno {n})"),
            )),
        }
    }

    /// Run one request on the long-lived sync connection, reconnecting once if it dropped.
    fn with_sync<T>(
        &self,
        mut op: impl FnMut(&mut TcpStream) -> io::Result<T>,
    ) -> Result<T, PhoneError> {
        let mut sync = lock(&self.sync);
        for attempt in 0..2 {
            if sync.is_none() {
                *sync = Some(self.open("sync:").map_err(io_error)?);
            }
            match op(sync.as_mut().expect("just opened")) {
                Ok(value) => return Ok(value),
                // The phone's own refusal, on a connection that's still fine
                Err(e) if e.kind() == io::ErrorKind::Other => return Err(io_error(e)),
                Err(e) => {
                    *sync = None;
                    if attempt == 1 {
                        return Err(io_error(e));
                    }
                }
            }
        }
        unreachable!()
    }

    /// Run a shell command on the phone as adb's shell user. Fails with its output unless it succeeded.
    fn exec(&self, command: &str) -> Result<(), PhoneError> {
        const OK: &str = "__windroidfs_ok__";
        let mut conn = self
            .open(&format!("exec:{command} 2>&1 && echo {OK}"))
            .map_err(io_error)?;
        let mut out = String::new();
        conn.read_to_string(&mut out).map_err(io_error)?;
        if out.trim_end().ends_with(OK) {
            Ok(())
        } else {
            let message = out.trim();
            let code = if message.contains("No such file") {
                ErrorCode::NotFound
            } else if message.contains("ermission denied") {
                ErrorCode::AccessDenied
            } else {
                ErrorCode::IoError
            };
            Err(remote(code, message))
        }
    }

    /// A connection to the adb server, switched to this phone and then to `service`.
    fn open(&self, service: &str) -> io::Result<TcpStream> {
        let mut conn = TcpStream::connect(ADB_SERVER)?;
        conn.set_read_timeout(Some(TIMEOUT))?;
        conn.set_write_timeout(Some(TIMEOUT))?;
        request(&mut conn, &format!("host:transport:{}", self.phone.serial))?;
        request(&mut conn, service)?;
        Ok(conn)
    }
}

impl Drop for AdbFiles {
    fn drop(&mut self) {
        if let Some(mut sync) = lock(&self.sync).take() {
            let _ = sync_send(&mut sync, b"QUIT", b"");
        }
        lock(&self.copies).clear();
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

impl Copy {
    fn touch(&mut self) {
        self.dirty = true;
        self.modified_ms = now_s() as i64 * 1000;
    }
}

/// adb's host protocol: a 4-hex-digit length, the request, then OKAY or FAIL
fn request(conn: &mut TcpStream, what: &str) -> io::Result<()> {
    conn.write_all(format!("{:04x}{what}", what.len()).as_bytes())?;
    let mut status = [0; 4];
    conn.read_exact(&mut status)?;
    if &status == b"OKAY" {
        return Ok(());
    }
    let mut len = [0; 4];
    conn.read_exact(&mut len)?;
    let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap_or("0"), 16).unwrap_or(0);
    let mut message = vec![0; len];
    conn.read_exact(&mut message)?;
    Err(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!("adb: {}", String::from_utf8_lossy(&message)),
    ))
}

fn sync_send(conn: &mut TcpStream, id: &[u8; 4], data: &[u8]) -> io::Result<()> {
    let mut packet = Vec::with_capacity(8 + data.len());
    packet.extend_from_slice(id);
    packet.extend_from_slice(&(data.len() as u32).to_le_bytes());
    packet.extend_from_slice(data);
    conn.write_all(&packet)
}

fn read_u32(conn: &mut TcpStream) -> io::Result<u32> {
    let mut b = [0; 4];
    conn.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// A FAIL packet's message, as an error that leaves the connection usable
fn sync_fail(conn: &mut TcpStream) -> io::Error {
    let message = read_u32(conn).and_then(|len| {
        let mut message = vec![0; len as usize];
        conn.read_exact(&mut message)?;
        Ok(String::from_utf8_lossy(&message).into_owned())
    });
    io::Error::other(message.unwrap_or_else(|e| e.to_string()))
}

/// `STA2`: (errno, stat)
fn stat(conn: &mut TcpStream, path: &str) -> io::Result<(u32, Stat)> {
    sync_send(conn, b"STA2", path.as_bytes())?;
    let mut reply = [0; 72];
    conn.read_exact(&mut reply)?;
    if &reply[..4] != b"STA2" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "adb sent an unexpected stat reply",
        ));
    }
    let u32_at = |i: usize| u32::from_le_bytes(reply[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(reply[i..i + 8].try_into().unwrap());
    // id, error, dev, ino, mode, nlink, uid, gid, size, atime, mtime, ctime
    Ok((
        u32_at(4),
        Stat {
            mode: u32_at(24),
            size: u64_at(40),
            mtime_s: u64_at(56) as i64,
        },
    ))
}

/// `LIS2`: every entry's name and stat, "." and ".." included
fn list(conn: &mut TcpStream, path: &str) -> io::Result<Vec<(String, Stat)>> {
    sync_send(conn, b"LIS2", path.as_bytes())?;
    let mut entries = Vec::new();
    loop {
        // id, error, dev, ino, mode, nlink, uid, gid, size, atime, mtime, ctime, namelen
        let mut head = [0; 76];
        conn.read_exact(&mut head)?;
        let u32_at = |i: usize| u32::from_le_bytes(head[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(head[i..i + 8].try_into().unwrap());
        match &head[..4] {
            b"DONE" => return Ok(entries),
            b"DNT2" => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "adb sent an unexpected listing",
                ));
            }
        }
        let mut name = vec![0; u32_at(72) as usize];
        conn.read_exact(&mut name)?;
        if u32_at(4) == 0 {
            entries.push((
                String::from_utf8_lossy(&name).into_owned(),
                Stat {
                    mode: u32_at(24),
                    size: u64_at(40),
                    mtime_s: u64_at(56) as i64,
                },
            ));
        }
    }
}

/// `RECV`: the whole file into `out`.
fn recv(conn: &mut TcpStream, path: &str, out: &mut File) -> io::Result<()> {
    sync_send(conn, b"RECV", path.as_bytes())?;
    let mut buf = vec![0; SYNC_CHUNK];
    loop {
        let mut id = [0; 4];
        conn.read_exact(&mut id)?;
        match &id {
            b"DATA" => {
                let len = read_u32(conn)? as usize;
                if len > buf.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "adb sent an oversized chunk",
                    ));
                }
                conn.read_exact(&mut buf[..len])?;
                out.write_all(&buf[..len])?;
            }
            b"DONE" => {
                read_u32(conn)?;
                return out.flush();
            }
            b"FAIL" => return Err(sync_fail(conn)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "adb sent an unexpected reply",
                ));
            }
        }
    }
}

/// `SEND`: all of `contents` as the file at `path`, replacing it.
fn send(conn: &mut TcpStream, path: &str, contents: &mut dyn Read, mtime_s: u32) -> io::Result<()> {
    sync_send(conn, b"SEND", format!("{path},{SEND_MODE}").as_bytes())?;
    let mut buf = vec![0; SYNC_CHUNK];
    loop {
        let n = contents.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sync_send(conn, b"DATA", &buf[..n])?;
    }
    conn.write_all(b"DONE")?;
    conn.write_all(&mtime_s.to_le_bytes())?;
    let mut id = [0; 4];
    conn.read_exact(&mut id)?;
    match &id {
        b"OKAY" => read_u32(conn).map(|_| ()),
        b"FAIL" => Err(sync_fail(conn)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "adb sent an unexpected reply",
        )),
    }
}

fn device_path(path: &str) -> String {
    format!("{STORAGE_ROOT}{path}")
}

/// Shared storage ignores case, as Windows does.
fn key(path: &str) -> String {
    path.to_lowercase()
}

/// For the phone's shell: one single-quoted word, whatever's in it.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn info(path: &str, is_directory: bool, size: u64, modified_ms: i64) -> FileInfo {
    FileInfo {
        name: path.rsplit('/').next().unwrap_or_default().to_string(),
        size_bytes: if is_directory { 0 } else { size as i64 },
        is_directory,
        last_modified_ms: modified_ms,
    }
}

fn remote(code: ErrorCode, message: &str) -> PhoneError {
    PhoneError::Remote {
        code,
        message: message.to_string(),
    }
}

fn io_error(e: io::Error) -> PhoneError {
    match e.kind() {
        io::ErrorKind::Other => {
            let message = e.to_string();
            let code = if message.contains("No such file") {
                ErrorCode::NotFound
            } else if message.contains("ermission denied") {
                ErrorCode::AccessDenied
            } else if message.contains("No space") {
                ErrorCode::NoSpace
            } else {
                ErrorCode::IoError
            };
            PhoneError::Remote { code, message }
        }
        _ => PhoneError::Transport(format!("adb: {e}")),
    }
}

fn now_s() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_restricted_paths() {
        assert!(is_restricted("/Android/data"));
        assert!(is_restricted("/android/OBB/com.game/main.obb"));
        assert!(!is_restricted("/Android"));
        assert!(!is_restricted("/Android/database"));
        assert!(!is_restricted("/Download/Android/data"));
    }

    /// Needs a phone plugged in with USB debugging on: `cargo test round_trips_through_adb -- --ignored`
    #[test]
    #[ignore]
    fn round_trips_through_adb() {
        let phone = crate::adb::phone().expect("no phone in `adb devices`");
        let files = AdbFiles::new(phone.clone());
        let listed = files.list_dir("/Android/data").unwrap();
        assert!(
            listed.iter().any(|f| f.is_directory),
            "Android/data looked empty"
        );
        println!("Android/data: {} entries", listed.len());

        let dir = "/Android/obb/.windroidfs-test";
        let _ = files.exec(&format!("rm -rf {}", quote(&device_path(dir))));
        files.create(dir, true).unwrap();
        let path = format!("{dir}/it's a test.bin");
        files.create(&path, false).unwrap();
        assert!(matches!(
            files.create(&path, false),
            Err(PhoneError::Remote {
                code: ErrorCode::AlreadyExists,
                ..
            })
        ));

        // Over one sync chunk, written out of order.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        files.write(&path, 100_000, &data[100_000..]).unwrap();
        files.write(&path, 0, &data[..100_000]).unwrap();
        assert_eq!(files.stat(&path).unwrap().size_bytes, 200_000);
        files.flush(&path).unwrap();

        // A fresh client has no copy, so this comes from the phone.
        let fresh = AdbFiles::new(phone);
        let mut back = Vec::new();
        while back.len() < data.len() {
            let chunk = fresh.read(&path, back.len() as u64, 65_536).unwrap();
            back.extend_from_slice(&chunk.data);
            if chunk.eof {
                break;
            }
        }
        assert_eq!(back, data);

        fresh.truncate(&path, 0).unwrap();
        fresh.write(&path, 0, b"short").unwrap();
        fresh.flush(&path).unwrap();
        assert_eq!(files.stat(&path).unwrap().size_bytes, 5);

        let renamed = format!("{dir}/renamed.bin");
        files.rename(&path, &renamed, false).unwrap();
        assert!(matches!(
            files.stat(&path),
            Err(PhoneError::Remote {
                code: ErrorCode::NotFound,
                ..
            })
        ));
        assert!(matches!(
            files.delete(dir),
            Err(PhoneError::Remote {
                code: ErrorCode::DirectoryNotEmpty,
                ..
            })
        ));
        files.delete(&renamed).unwrap();
        files.delete(dir).unwrap();
        assert!(files.stat(dir).is_err());
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(quote("/a b/it's"), r"'/a b/it'\''s'");
    }
}
