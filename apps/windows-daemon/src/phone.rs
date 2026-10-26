use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::frame::{self, Frame};
use prost::Message;
use rusb::{Context, DeviceHandle};

use crate::windroid::packet::Payload;
use crate::windroid::{
    CreateRequest, DeleteRequest, DirectoryRequest, ErrorCode, FileInfo, FileReadRequest,
    FlushRequest, Goodbye, OperationResponse, Packet, Ping, RenameRequest, SetTimesRequest,
    StatRequest, TruncateRequest, UsbSpeed, VolumeInfoRequest, WriteRequest,
};

/// How long to wait for the phone to answer a request
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// After this long with no requests, the USB thread pings the phone to check it's still there
const HEARTBEAT_AFTER: Duration = Duration::from_secs(3);

/// How long a health-check ping may take. A ping is answered straight away by a phone that's alive, so this can be much shorter than [`REPLY_TIMEOUT`]
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// Size of each USB read. Must be a multiple of the max packet size (512, or 1024 on USB 3) or a full packet can overflow it. Frames longer than this simply take several reads.
const READ_CHUNK: usize = 64 * 1024;

/// The most file data asked for in one request. The phone allocates a buffer this size, and it keeps every frame far below `frame::MAX_PAYLOAD`. Larger reads are split into several.
pub const MAX_READ: u32 = 1024 * 1024;

/// The most file data sent in one write request. Larger writes are split.
pub const MAX_WRITE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub enum PhoneError {
    /// Phone was unplugged or the link failed
    Disconnected,
    /// Request couldn't be delivered or answered (timeout, USB error)
    Transport(String),
    /// Phone answered, and the answer was no
    Remote { code: ErrorCode, message: String },
    /// Phone answered with a message of the wrong type
    UnexpectedReply,
}

impl std::fmt::Display for PhoneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhoneError::Disconnected => write!(f, "the phone is disconnected"),
            PhoneError::Transport(why) => write!(f, "{why}"),
            PhoneError::Remote { code, message } => write!(f, "{message} ({code:?})"),
            PhoneError::UnexpectedReply => write!(f, "the phone sent an unexpected reply"),
        }
    }
}

/// One chunk of file data.
pub struct ReadChunk {
    pub data: Vec<u8>,
    pub eof: bool,
}

pub struct VolumeInfo {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

struct UsbJob {
    payload: Payload,
    // How long to wait for the phone to reply
    timeout: Duration,
    reply: mpsc::SyncSender<Result<Payload, PhoneError>>,
}

#[derive(Clone, Debug)]
pub struct PhoneClient {
    tx: mpsc::Sender<UsbJob>,
    speed: UsbSpeed,
    alive: Arc<AtomicBool>,
}

impl PhoneClient {
    /// Take ownership of the opened accessory and start the USB thread
    /// The thread runs until every clone of the returned client has been dropped, then releases the interface
    pub fn start(handle: DeviceHandle<Context>, interface: u8, out_ep: u8, in_ep: u8) -> Self {
        let (tx, rx) = mpsc::channel::<UsbJob>();
        let alive = Arc::new(AtomicBool::new(true));
        let speed = usb_speed(handle.device().speed());
        let link = Link {
            speed,
            handle,
            out_ep,
            in_ep,
            inbox: Vec::new(),
            scratch: vec![0u8; READ_CHUNK],
            next_id: 0,
        };
        let thread_alive = alive.clone();
        thread::Builder::new()
            .name("windroid-usb".into())
            .spawn(move || usb_thread(link, interface, rx, &thread_alive))
            .expect("failed to start the USB thread");
        PhoneClient { tx, speed, alive }
    }

    // Tell the phone this PC is disconnecting so it ends the link
    pub fn goodbye(&self) {
        let _ = self.call_within(Payload::Goodbye(Goodbye {}), Duration::from_secs(1));
    }

    /// The speed the USB link was negotiated at, which caps how fast transfers can go
    pub fn usb_speed(&self) -> UsbSpeed {
        self.speed
    }

    /// False once the link has died. Will never become true again - reconnecting means a new handshake and client
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Send one request and wait for its reply
    fn call(&self, payload: Payload) -> Result<Payload, PhoneError> {
        self.call_within(payload, REPLY_TIMEOUT)
    }

    /// [`PhoneClient::call`], waiting at most `timeout` for the reply
    fn call_within(&self, payload: Payload, timeout: Duration) -> Result<Payload, PhoneError> {
        if !self.is_connected() {
            return Err(PhoneError::Disconnected);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(UsbJob {
                payload,
                timeout,
                reply: reply_tx,
            })
            .map_err(|_| PhoneError::Disconnected)?;
        reply_rx.recv().map_err(|_| PhoneError::Disconnected)?
    }

    /// Check the phone is answering. Returns its name ("Wilson's S25+"), which is empty if the phone app predates sending it.
    /// Gives up after a few seconds: a phone app that's listening answers a ping near instantly.
    pub fn ping(&self) -> Result<String, PhoneError> {
        match self.call_within(Payload::Ping(ping(self.speed)), HEARTBEAT_TIMEOUT)? {
            Payload::Pong(pong) => Ok(pong.device_name),
            _ => Err(PhoneError::UnexpectedReply),
        }
    }

    /// Entries of the directory at `path` (Separated by `/`, `/` is the root)
    pub fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, PhoneError> {
        let reply = self.call(Payload::DirRequest(DirectoryRequest {
            path: path.to_string(),
        }))?;
        let Payload::DirResponse(resp) = reply else {
            return Err(PhoneError::UnexpectedReply);
        };
        if !resp.success {
            return Err(remote_error(resp.error_code(), resp.error_message));
        }
        Ok(resp.files)
    }

    /// Metadata for one path
    pub fn stat(&self, path: &str) -> Result<FileInfo, PhoneError> {
        let reply = self.call(Payload::StatRequest(StatRequest {
            path: path.to_string(),
        }))?;
        let Payload::StatResponse(resp) = reply else {
            return Err(PhoneError::UnexpectedReply);
        };
        if !resp.success {
            return Err(remote_error(resp.error_code(), resp.error_message));
        }
        resp.info.ok_or(PhoneError::UnexpectedReply)
    }

    /// Up to `len` bytes of the file at `path`, starting at `offset`. `len` is clamped to [`MAX_READ`], callers wanting more should loop.
    pub fn read(&self, path: &str, offset: u64, len: u32) -> Result<ReadChunk, PhoneError> {
        let reply = self.call(Payload::FileReq(FileReadRequest {
            path: path.to_string(),
            offset: offset as i64,
            max_bytes: len.min(MAX_READ) as i32,
        }))?;
        let Payload::FileRes(resp) = reply else {
            return Err(PhoneError::UnexpectedReply);
        };
        if !resp.success {
            return Err(remote_error(resp.error_code(), resp.error_message));
        }
        Ok(ReadChunk {
            data: resp.data,
            eof: resp.eof,
        })
    }

    pub fn volume_info(&self) -> Result<VolumeInfo, PhoneError> {
        let reply = self.call(Payload::VolumeRequest(VolumeInfoRequest {}))?;
        let Payload::VolumeResponse(resp) = reply else {
            return Err(PhoneError::UnexpectedReply);
        };
        Ok(VolumeInfo {
            total_bytes: resp.total_bytes.max(0) as u64,
            free_bytes: resp.free_bytes.max(0) as u64,
        })
    }

    /// Writes

    /// Send a write-type request and unwrap its `OperationResponse`
    fn operation(&self, payload: Payload) -> Result<OperationResponse, PhoneError> {
        let Payload::OpResponse(resp) = self.call(payload)? else {
            return Err(PhoneError::UnexpectedReply);
        };
        if !resp.success {
            return Err(remote_error(resp.error_code(), resp.error_message));
        }
        Ok(resp)
    }

    /// Create an empty file or a folder and return its metadata
    pub fn create(&self, path: &str, is_directory: bool) -> Result<FileInfo, PhoneError> {
        let resp = self.operation(Payload::CreateRequest(CreateRequest {
            path: path.to_string(),
            is_directory,
        }))?;
        resp.info.ok_or(PhoneError::UnexpectedReply)
    }

    /// Write `data` at `offset` - returns the bytes written and the file's metadata afterwards. `data` must be at most [`MAX_WRITE`] bytes, callers with more should loop.
    pub fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(u64, FileInfo), PhoneError> {
        debug_assert!(data.len() <= MAX_WRITE, "write larger than MAX_WRITE");
        let resp = self.operation(Payload::WriteRequest(WriteRequest {
            path: path.to_string(),
            offset: offset as i64,
            data: data.to_vec(),
        }))?;
        let info = resp.info.ok_or(PhoneError::UnexpectedReply)?;
        Ok((resp.bytes_written.max(0) as u64, info))
    }

    /// Set a file's length and return its metadata afterwards
    pub fn truncate(&self, path: &str, size: u64) -> Result<FileInfo, PhoneError> {
        let resp = self.operation(Payload::TruncateRequest(TruncateRequest {
            path: path.to_string(),
            size: size as i64,
        }))?;
        resp.info.ok_or(PhoneError::UnexpectedReply)
    }

    /// Delete a file or an empty folder
    pub fn delete(&self, path: &str) -> Result<(), PhoneError> {
        self.operation(Payload::DeleteRequest(DeleteRequest {
            path: path.to_string(),
        }))
        .map(|_| ())
    }

    pub fn rename(&self, from: &str, to: &str, replace_existing: bool) -> Result<(), PhoneError> {
        self.operation(Payload::RenameRequest(RenameRequest {
            from_path: from.to_string(),
            to_path: to.to_string(),
            replace_existing,
        }))
        .map(|_| ())
    }

    /// Set the modification time. May not always reflect due to Android file system behaviour.
    pub fn set_modified(&self, path: &str, modified_ms: i64) -> Result<(), PhoneError> {
        self.operation(Payload::SetTimesRequest(SetTimesRequest {
            path: path.to_string(),
            last_modified_ms: modified_ms,
        }))
        .map(|_| ())
    }

    /// Have the phone close and sync a file it has been writing.
    pub fn flush(&self, path: &str) -> Result<(), PhoneError> {
        self.operation(Payload::FlushRequest(FlushRequest {
            path: path.to_string(),
        }))
        .map(|_| ())
    }
}

/// A failed response. An `OK` code alongside `success = false` would mean a phone that didn't fill the code in, raises `IoError` in this case.
fn remote_error(code: ErrorCode, message: String) -> PhoneError {
    let code = if code == ErrorCode::Ok {
        ErrorCode::IoError
    } else {
        code
    };
    PhoneError::Remote { code, message }
}

fn usb_speed(speed: rusb::Speed) -> UsbSpeed {
    match speed {
        rusb::Speed::Low => UsbSpeed::Low,
        rusb::Speed::Full => UsbSpeed::Full,
        rusb::Speed::High => UsbSpeed::High,
        rusb::Speed::Super => UsbSpeed::Super,
        rusb::Speed::SuperPlus => UsbSpeed::SuperPlus,
        _ => UsbSpeed::Unknown,
    }
}

/// The ping sent to the phone, carrying this PC's name and the link speed for the phone's dashboard. The phone has no way to find out the speed itself.
fn ping(speed: UsbSpeed) -> Ping {
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ping {
        timestamp_ms,
        host_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
        usb_speed: speed as i32,
    }
}

/// The opened accessory plus the state that has to survive between requests
struct Link {
    speed: UsbSpeed,
    handle: DeviceHandle<Context>,
    out_ep: u8,
    in_ep: u8,
    /// Bytes read from the phone that don't yet make up a whole frame. Kept across requests.
    inbox: Vec<u8>,
    scratch: Vec<u8>,
    next_id: u32,
}

fn usb_thread(mut link: Link, interface: u8, rx: mpsc::Receiver<UsbJob>, alive: &AtomicBool) {
    let mut established = false;

    loop {
        let job = match rx.recv_timeout(HEARTBEAT_AFTER) {
            Ok(job) => job,
            // Been quiet for a while. Make sure the phone is still there.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !established || link.healthy() {
                    continue;
                }
                break;
            }

            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let result = link.exchange(job.payload, job.timeout);
        established |= result.is_ok();
        let link_failed = match &result {
            Err(PhoneError::Disconnected) => true,
            Err(PhoneError::Transport(_)) => established && !link.healthy(),
            _ => false,
        };

        let _ = job.reply.send(result);
        if link_failed {
            break;
        }
    }

    // From here on every request fails at once (see `PhoneClient::call`). Queued jobs will also fail.
    alive.store(false, Ordering::SeqCst);
    let _ = link.handle.release_interface(interface);
}

impl Link {
    fn healthy(&mut self) -> bool {
        matches!(
            self.exchange(Payload::Ping(ping(self.speed)), HEARTBEAT_TIMEOUT),
            Ok(Payload::Pong(_))
        )
    }

    /// Send one request and wait up to `timeout` for the reply carrying its `request_id`
    fn exchange(&mut self, payload: Payload, timeout: Duration) -> Result<Payload, PhoneError> {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let request = Packet {
            request_id: self.next_id,
            payload: Some(payload),
        };
        exchange(
            &self.handle,
            self.out_ep,
            self.in_ep,
            &request,
            &mut self.inbox,
            &mut self.scratch,
            timeout,
        )
    }
}

/// Send `request` and wait up to `timeout` for the reply carrying the same `request_id`
fn exchange(
    handle: &DeviceHandle<Context>,
    out_ep: u8,
    in_ep: u8,
    request: &Packet,
    inbox: &mut Vec<u8>,
    scratch: &mut [u8],
    timeout: Duration,
) -> Result<Payload, PhoneError> {
    // Header and payload go out in one write, so they arrive as one transfer
    let framed = frame::encode(&request.encode_to_vec());
    handle
        .write_bulk(out_ep, &framed, WRITE_TIMEOUT.min(timeout))
        .map_err(usb_error)?;

    let deadline = Instant::now() + timeout;
    loop {
        // Use whatever is already buffered. An earlier read may have brought in this reply, or a stale one that needs clearing.
        loop {
            match frame::take_frame(inbox) {
                Frame::Complete(bytes) => match Packet::decode(bytes.as_slice()) {
                    Ok(reply) if reply.request_id == request.request_id => {
                        return reply.payload.ok_or(PhoneError::UnexpectedReply);
                    }
                    Ok(stale) => log::warn!(
                        "⚠️  Dropped a late reply to request {} (waiting for {}).",
                        stale.request_id,
                        request.request_id
                    ),
                    Err(e) => log::warn!("⚠️  Dropped an unreadable message from the phone: {e}"),
                },
                Frame::Skipped(n) => {
                    log::warn!("⚠️  Skipped {n} corrupt bytes on the USB link while resyncing.")
                }
                Frame::Incomplete => break,
            }
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(PhoneError::Transport(format!(
                "the phone didn't answer request {} within {}s",
                request.request_id,
                timeout.as_secs()
            )));
        }

        // A timeout here just means nothing more has arrived yet: keep what's buffered and go round again until the deadline.
        // Throwing partial data away is what used to leave the 2 sides permanently out of step.
        match handle.read_bulk(in_ep, scratch, deadline - now) {
            Ok(n) => inbox.extend_from_slice(&scratch[..n]),
            Err(rusb::Error::Timeout) => {}
            Err(e) => return Err(usb_error(e)),
        }
    }
}

fn usb_error(e: rusb::Error) -> PhoneError {
    match e {
        rusb::Error::NoDevice | rusb::Error::NotFound | rusb::Error::Io => PhoneError::Disconnected,
        other => PhoneError::Transport(format!("USB error: {other}")),
    }
}

#[cfg(test)]
impl PhoneClient {
    /// A client answered from a local folder instead of a phone, mirroring the phone's FileServer.kt. Lets the filesystem layers be tested for real without a device.
    pub fn local(root: std::path::PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<UsbJob>();
        thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                let _ = job.reply.send(Ok(local::answer(&root, job.payload)));
            }
        });
        PhoneClient {
            tx,
            speed: UsbSpeed::Unknown,
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
}

#[cfg(test)]
mod local {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::time::UNIX_EPOCH;

    use super::{MAX_READ, Payload};
    use crate::windroid::{
        DirectoryResponse, ErrorCode, FileInfo, FileReadResponse, OperationResponse, Pong,
        StatResponse, VolumeInfoResponse,
    };

    pub fn answer(root: &Path, request: Payload) -> Payload {
        match request {
            Payload::Ping(p) => Payload::Pong(Pong {
                timestamp_ms: p.timestamp_ms,
                device_name: "Test Phone".into(),
            }),
            Payload::DirRequest(r) => Payload::DirResponse(list(&resolve(root, &r.path))),
            Payload::StatRequest(r) => Payload::StatResponse(stat(root, &resolve(root, &r.path))),
            Payload::FileReq(r) => {
                Payload::FileRes(read(&resolve(root, &r.path), r.offset, r.max_bytes))
            }
            Payload::VolumeRequest(_) => Payload::VolumeResponse(VolumeInfoResponse {
                total_bytes: 64 * 1024 * 1024 * 1024,
                free_bytes: 16 * 1024 * 1024 * 1024,
            }),
            Payload::CreateRequest(r) => op(create(&resolve(root, &r.path), r.is_directory)),
            Payload::WriteRequest(r) => op(write(&resolve(root, &r.path), r.offset, &r.data)),
            Payload::TruncateRequest(r) => op(truncate(&resolve(root, &r.path), r.size)),
            Payload::DeleteRequest(r) => op(delete(&resolve(root, &r.path))),
            Payload::RenameRequest(r) => op(rename(
                &resolve(root, &r.from_path),
                &resolve(root, &r.to_path),
                r.replace_existing,
            )),
            // Best effort on the phone, so always fine here too.
            Payload::SetTimesRequest(_) | Payload::FlushRequest(_) => op(Ok((None, 0))),
            other => panic!("the local phone doesn't answer {other:?}"),
        }
    }

    type OpResult = Result<(Option<FileInfo>, i64), ErrorCode>;

    fn op(result: OpResult) -> Payload {
        Payload::OpResponse(match result {
            Ok((info, bytes_written)) => OperationResponse {
                success: true,
                info,
                bytes_written,
                ..Default::default()
            },
            Err(code) => OperationResponse {
                success: false,
                error_code: code as i32,
                ..Default::default()
            },
        })
    }

    fn info_of(path: &Path) -> Option<FileInfo> {
        fs::metadata(path).ok().map(|m| info(path, &m))
    }

    fn create(path: &Path, is_directory: bool) -> OpResult {
        if path.exists() {
            return Err(ErrorCode::AlreadyExists);
        }
        if !path.parent().is_some_and(Path::is_dir) {
            return Err(ErrorCode::NotFound);
        }
        let made = if is_directory {
            fs::create_dir(path)
        } else {
            fs::File::create_new(path).map(|_| ())
        };
        made.map_err(|_| ErrorCode::IoError)?;
        Ok((info_of(path), 0))
    }

    fn write(path: &Path, offset: i64, data: &[u8]) -> OpResult {
        use std::io::Write;
        if offset < 0 {
            return Err(ErrorCode::InvalidRequest);
        }
        if path.is_dir() {
            return Err(ErrorCode::IsADirectory);
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|_| ErrorCode::NotFound)?;
        file.seek(SeekFrom::Start(offset as u64))
            .and_then(|_| file.write_all(data))
            .map_err(|_| ErrorCode::IoError)?;
        drop(file);
        Ok((info_of(path), data.len() as i64))
    }

    fn truncate(path: &Path, size: i64) -> OpResult {
        if size < 0 {
            return Err(ErrorCode::InvalidRequest);
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|_| ErrorCode::NotFound)?;
        file.set_len(size as u64).map_err(|_| ErrorCode::IoError)?;
        drop(file);
        Ok((info_of(path), 0))
    }

    fn delete(path: &Path) -> OpResult {
        let meta = fs::metadata(path).map_err(|_| ErrorCode::NotFound)?;
        if meta.is_dir() {
            if fs::read_dir(path)
                .map_err(|_| ErrorCode::IoError)?
                .next()
                .is_some()
            {
                return Err(ErrorCode::DirectoryNotEmpty);
            }
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
        .map_err(|_| ErrorCode::IoError)?;
        Ok((None, 0))
    }

    fn rename(from: &Path, to: &Path, replace_existing: bool) -> OpResult {
        if !from.exists() {
            return Err(ErrorCode::NotFound);
        }
        // Renaming only the case of a name ("photo.jpg" -> "Photo.jpg") is the same file on a case-insensitive store. It mustn't be treated as replacing another one.
        let same_file =
            from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
        if to.exists() && !same_file {
            if !replace_existing {
                return Err(ErrorCode::AlreadyExists);
            }
            if to.is_dir() {
                return Err(ErrorCode::AccessDenied);
            }
            fs::remove_file(to).map_err(|_| ErrorCode::IoError)?;
        }
        fs::rename(from, to).map_err(|_| ErrorCode::IoError)?;
        Ok((None, 0))
    }

    fn resolve(root: &Path, path: &str) -> PathBuf {
        root.join(path.trim_start_matches('/'))
    }

    fn info(path: &Path, meta: &fs::Metadata) -> FileInfo {
        FileInfo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size_bytes: if meta.is_dir() { 0 } else { meta.len() as i64 },
            is_directory: meta.is_dir(),
            last_modified_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        }
    }

    fn list(dir: &Path) -> DirectoryResponse {
        let fail = |code: ErrorCode| DirectoryResponse {
            success: false,
            error_code: code as i32,
            ..Default::default()
        };
        match fs::metadata(dir) {
            Err(_) => return fail(ErrorCode::NotFound),
            Ok(m) if !m.is_dir() => return fail(ErrorCode::NotADirectory),
            Ok(_) => {}
        }
        let files = fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| Some(info(&e.path(), &e.metadata().ok()?)))
            .collect();
        DirectoryResponse {
            success: true,
            files,
            ..Default::default()
        }
    }

    fn stat(root: &Path, path: &Path) -> StatResponse {
        match fs::metadata(path) {
            Ok(meta) => {
                let mut i = info(path, &meta);
                if path == root {
                    i.name.clear();
                }
                StatResponse {
                    success: true,
                    info: Some(i),
                    ..Default::default()
                }
            }
            Err(_) => StatResponse {
                success: false,
                error_code: ErrorCode::NotFound as i32,
                ..Default::default()
            },
        }
    }

    fn read(path: &Path, offset: i64, max_bytes: i32) -> FileReadResponse {
        let fail = |code: ErrorCode| FileReadResponse {
            success: false,
            error_code: code as i32,
            ..Default::default()
        };
        if max_bytes <= 0 || max_bytes as u32 > MAX_READ {
            return fail(ErrorCode::InvalidRequest);
        }
        let Ok(mut file) = fs::File::open(path) else {
            return fail(ErrorCode::NotFound);
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0) as i64;
        let mut data = vec![0u8; max_bytes as usize];
        file.seek(SeekFrom::Start(offset as u64)).unwrap();
        let mut got = 0;
        while got < data.len() {
            match file.read(&mut data[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        data.truncate(got);
        FileReadResponse {
            success: true,
            data,
            total_file_size: len,
            eof: offset + got as i64 >= len,
            ..Default::default()
        }
    }
}
