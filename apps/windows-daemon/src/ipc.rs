//! The request/response protocol between the user-level daemon (client) and the privileged
//! service (server), spoken over a local named pipe.
//!
//! Security model
//! --------------
//! The service runs as SYSTEM and can switch drivers, so the pipe is a privilege boundary.
//! The daemon on the other end is deliberately *unelevated* — that is the whole point of the
//! split — so the ACL has to let an ordinary interactive user connect. The boundary is therefore
//! held by what the service agrees to do, not by who can reach it:
//!
//! 1. Only interactive users can connect, and remote clients are rejected outright
//!    (`PIPE_REJECT_REMOTE_CLIENTS`). See `PIPE_SDDL`.
//! 2. Clients cannot create new instances of the pipe name, so a user process can't squat the
//!    name and impersonate the service to other clients. See `PIPE_SDDL`.
//! 3. The service only ever acts on Android VID:PIDs and only on driver *packages that are
//!    already installed*. It never creates or signs packages and never touches anything else.
//!    (That policy lives in the service binary; this module is just the wire format.)
//!
//! Point 3 is the real protection. Treat every request as coming from an untrusted process,
//! because it does.
//!
//! Wire format
//! -----------
//! One request line in, one response line out, UTF-8, newline-terminated. Fields are separated
//! by spaces; the request verb comes first. Kept deliberately simple and human-readable so you
//! can exercise the service by hand and nothing needs a serialization crate.

use std::fmt;

/// Pipe path. `.` means the local machine; the name is arbitrary but must match on both sides.
pub const PIPE_NAME: &str = r"\\.\pipe\WinDroidFS.driver";

/// SDDL for the pipe's security descriptor:
///
/// - `D:` — a discretionary ACL follows.
/// - `(A;;GA;;;SY)` — allow GENERIC_ALL to SY = Local System (the service itself).
/// - `(A;;GA;;;BA)` — allow GENERIC_ALL to BA = Builtin Administrators.
/// - `(A;;0x12019b;;;IU)` — allow IU = Interactive Users the connect-and-talk subset.
///
/// `IU` is needed because the daemon runs unelevated: under UAC an unelevated process in an
/// admin account carries Administrators as a *deny-only* SID, so a BA-only ACL locks out the
/// very process this pipe exists to serve. `IU` still excludes network and service logons.
///
/// The literal mask `0x12019b` is FILE_GENERIC_READ | FILE_GENERIC_WRITE *minus*
/// FILE_CREATE_PIPE_INSTANCE (0x4). Dropping that bit matters: with it, any interactive user
/// could create another instance of this pipe name and impersonate the service to other
/// clients. `GRGW` would have granted it, which is why the mask is spelled out.
pub const PIPE_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019b;;;IU)";

/// The access a client asks for when opening the pipe: the same mask `PIPE_SDDL` grants to
/// interactive users, so the request never asks for a right that would be refused.
///
/// FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_EA | FILE_WRITE_EA | FILE_READ_ATTRIBUTES
/// | FILE_WRITE_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE.
pub const PIPE_CLIENT_ACCESS: u32 = 0x0012_019b;

/// What the daemon asks the service to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Is the service alive? (sanity check / handshake)
    Ping,
    /// Switch this device's normal identity to WinUSB for a handshake, recording a rollback.
    BeginSwap { instance_id: String },
    /// Undo the swap recorded for this device.
    Rollback { instance_id: String },
    /// Make sure this device is on WinUSB, permanently (used for the accessory identity).
    EnsureWinusb { instance_id: String },
    /// Put every Android device that's stuck on WinUSB back on its original driver.
    RestoreAll,
}

/// What the service answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Pong,
    /// Command succeeded; `detail` is a human-readable summary for logging.
    Ok {
        detail: String,
    },
    /// Command failed; `message` explains why.
    Err {
        message: String,
    },
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed message: {}", self.0)
    }
}

// --- Request encoding ---

impl Request {
    pub fn encode(&self) -> String {
        match self {
            Request::Ping => "PING".into(),
            Request::BeginSwap { instance_id } => format!("BEGIN_SWAP {instance_id}"),
            Request::Rollback { instance_id } => format!("ROLLBACK {instance_id}"),
            Request::EnsureWinusb { instance_id } => format!("ENSURE_WINUSB {instance_id}"),
            Request::RestoreAll => "RESTORE_ALL".into(),
        }
    }

    pub fn decode(line: &str) -> Result<Self, ParseError> {
        let line = line.trim();
        let (verb, rest) = match line.split_once(' ') {
            Some((v, r)) => (v, r.trim()),
            None => (line, ""),
        };

        // Instance IDs contain spaces only if something is very wrong; reject empties so the
        // service never operates on a blank target.
        let instance = || -> Result<String, ParseError> {
            if rest.is_empty() {
                Err(ParseError(format!("{verb} needs an instance id")))
            } else {
                Ok(rest.to_string())
            }
        };

        match verb {
            "PING" => Ok(Request::Ping),
            "BEGIN_SWAP" => Ok(Request::BeginSwap {
                instance_id: instance()?,
            }),
            "ROLLBACK" => Ok(Request::Rollback {
                instance_id: instance()?,
            }),
            "ENSURE_WINUSB" => Ok(Request::EnsureWinusb {
                instance_id: instance()?,
            }),
            "RESTORE_ALL" => Ok(Request::RestoreAll),
            other => Err(ParseError(format!("unknown command {other:?}"))),
        }
    }
}

// --- Response encoding ---

impl Response {
    pub fn encode(&self) -> String {
        match self {
            Response::Pong => "PONG".into(),
            // Collapse newlines so one response is always one line.
            Response::Ok { detail } => format!("OK {}", one_line(detail)),
            Response::Err { message } => format!("ERR {}", one_line(message)),
        }
    }

    pub fn decode(line: &str) -> Result<Self, ParseError> {
        let line = line.trim();
        let (verb, rest) = match line.split_once(' ') {
            Some((v, r)) => (v, r.trim().to_string()),
            None => (line, String::new()),
        };
        match verb {
            "PONG" => Ok(Response::Pong),
            "OK" => Ok(Response::Ok { detail: rest }),
            "ERR" => Ok(Response::Err { message: rest }),
            other => Err(ParseError(format!("unknown response {other:?}"))),
        }
    }
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

// ===========================================================================
// Transport (Windows named pipe)
// ===========================================================================

pub mod transport {
    //! Minimal client/server helpers over the named pipe. Both sides read and write a single
    //! newline-terminated line, so the message framing matches the encoders above.

    use std::io::{self, BufRead, BufReader, Write};

    use super::{PIPE_CLIENT_ACCESS, PIPE_NAME, Request, Response};

    /// Open the pipe as a client, asking for exactly the access `PIPE_SDDL` grants.
    ///
    /// `OpenOptions::read(true).write(true)` would ask for GENERIC_WRITE, which on a pipe
    /// expands to include FILE_CREATE_PIPE_INSTANCE (0x4) — a right the ACL deliberately
    /// withholds — so the open would be refused. Requesting the explicit mask avoids asking
    /// for a privilege we don't want and aren't granted.
    fn open_pipe_once() -> io::Result<std::fs::File> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .access_mode(PIPE_CLIENT_ACCESS)
                .open(PIPE_NAME)
        }
        #[cfg(not(windows))]
        {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(PIPE_NAME)
        }
    }

    /// How long to keep trying to reach a server that is up but not listening this instant.
    const CONNECT_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

    /// Open the pipe, retrying briefly while no instance is available.
    ///
    /// The server serves one client per pipe instance: it accepts, answers, closes, then
    /// creates the next instance. In the gap between those the name does not exist, so a
    /// client that connects right after a previous request — two in a row, as `restore-drivers`
    /// does — gets "file not found" from a service that is perfectly healthy. Retrying closes
    /// that window, and also covers the case where another client is mid-request.
    fn open_pipe() -> io::Result<std::fs::File> {
        const ERROR_FILE_NOT_FOUND: i32 = 2;
        const ERROR_PIPE_BUSY: i32 = 231;

        let deadline = std::time::Instant::now() + CONNECT_RETRY_WINDOW;
        loop {
            match open_pipe_once() {
                Ok(pipe) => return Ok(pipe),
                Err(e) => {
                    let transient = matches!(
                        e.raw_os_error(),
                        Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PIPE_BUSY)
                    );
                    if !transient || std::time::Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }

    /// Client: connect, send one request, read one response. Used by the daemon.
    pub fn send(request: &Request) -> io::Result<Response> {
        let mut pipe = open_pipe()?;

        writeln!(pipe, "{}", request.encode())?;
        pipe.flush()?;

        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Response::decode(&line)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Is the service reachable? `false` means it isn't installed/running (not an error).
    pub fn service_available() -> bool {
        matches!(send(&Request::Ping), Ok(Response::Pong))
    }

    /// Unblock a server that's sitting in `accept_one`, so it can re-check its stop flag.
    ///
    /// The server's accept is synchronous and blocks until *someone* connects, so the standard
    /// way to interrupt it is to connect once ourselves and immediately hang up. The server sees
    /// an empty request, answers into a closed pipe (harmless), loops, sees the stop flag and
    /// exits. Called from the service's stop handler; errors are deliberately ignored, since a
    /// server that isn't listening needs no waking.
    pub fn wake_server() {
        let _ = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE_NAME);
    }

    /// Server: create one instance of the pipe, wait for a client, and hand back a connected
    /// stream plus a one-shot responder. Only the service calls this.
    ///
    /// `first_instance` must be true the very first time (it sets FILE_FLAG_FIRST_PIPE_INSTANCE
    /// so a hijacker can't have pre-created the name).
    #[cfg(windows)]
    pub fn accept_one(first_instance: bool) -> io::Result<Connection> {
        imp::accept_one(first_instance)
    }

    /// A connected client, with helpers to read its request and write one response.
    #[cfg(windows)]
    pub struct Connection(imp::PipeStream);

    #[cfg(windows)]
    impl Connection {
        /// Read the client's single request line.
        pub fn read_request(&mut self) -> io::Result<Request> {
            let mut reader = BufReader::new(&mut self.0);
            let mut line = String::new();
            reader.read_line(&mut line)?;
            Request::decode(&line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        }

        /// Write the response and flush.
        pub fn respond(&mut self, response: &Response) -> io::Result<()> {
            writeln!(self.0, "{}", response.encode())?;
            self.0.flush()
        }
    }

    #[cfg(windows)]
    mod imp {
        use std::io::{self, Read, Write};
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use std::ptr::null_mut;

        use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE, LocalFree};
        use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows_sys::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        const SDDL_REVISION_1: u32 = 1;
        const ERROR_PIPE_CONNECTED: u32 = 535;

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(Some(0)).collect()
        }

        /// A connected pipe instance. Closes on drop (see the note below the impls).
        pub struct PipeStream {
            handle: OwnedHandle,
        }

        impl Read for PipeStream {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                // Reuse std's File read/write; the handle is a byte-mode pipe.
                use std::os::windows::io::AsRawHandle;
                let mut file = unsafe {
                    std::mem::ManuallyDrop::new(std::fs::File::from_raw_handle(
                        self.handle.as_raw_handle(),
                    ))
                };
                file.read(buf)
            }
        }

        impl Write for PipeStream {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                use std::os::windows::io::AsRawHandle;
                let mut file = unsafe {
                    std::mem::ManuallyDrop::new(std::fs::File::from_raw_handle(
                        self.handle.as_raw_handle(),
                    ))
                };
                file.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        // No Drop impl on purpose: letting `OwnedHandle` close the handle is what we want.
        //
        // The obvious thing here is DisconnectNamedPipe, and it is wrong: it *forcibly
        // discards* anything the client has not read yet, so the response we just wrote is
        // destroyed before the client can read it (the client sees ERROR_PIPE_NOT_CONNECTED,
        // 233). Closing the handle instead lets the client drain the pipe and then see a
        // clean end-of-file. FlushFileBuffers would also work, but it blocks until the client
        // reads, which would let a stuck client hang the service.

        /// Owns the security descriptor built from the SDDL and frees it on drop.
        struct SecurityDescriptor(*mut core::ffi::c_void);
        impl Drop for SecurityDescriptor {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0 as _) };
            }
        }

        fn build_security_descriptor() -> io::Result<SecurityDescriptor> {
            let sddl = wide(super::super::PIPE_SDDL);
            let mut psd: *mut core::ffi::c_void = null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut psd,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(SecurityDescriptor(psd))
        }

        pub fn accept_one(first_instance: bool) -> io::Result<super::Connection> {
            let name = wide(super::super::PIPE_NAME);
            let sd = build_security_descriptor()?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: 0,
            };

            let mut open_mode = PIPE_ACCESS_DUPLEX;
            if first_instance {
                open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
            }

            let handle = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    open_mode,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    8,    // a few concurrent instances
                    4096, // out buffer
                    4096, // in buffer
                    0,    // default timeout
                    &sa,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: CreateNamedPipeW returned a valid, owned handle.
            let handle = unsafe { OwnedHandle::from_raw_handle(handle as _) };

            // Block until a client connects. ERROR_PIPE_CONNECTED means one connected in the
            // tiny window before this call; that's success too.
            use std::os::windows::io::AsRawHandle;
            let connected = unsafe { ConnectNamedPipe(handle.as_raw_handle() as _, null_mut()) };
            if connected == 0 {
                let err = unsafe { GetLastError() };
                if err != ERROR_PIPE_CONNECTED {
                    return Err(io::Error::from_raw_os_error(err as i32));
                }
                // else: a client connected in the race window; treat as success.
            }

            Ok(super::Connection(PipeStream { handle }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let cases = [
            Request::Ping,
            Request::RestoreAll,
            Request::BeginSwap {
                instance_id: r"USB\VID_04E8&PID_6860\R5CY204BABT".into(),
            },
            Request::Rollback {
                instance_id: r"USB\VID_04E8&PID_6860\R5CY204BABT".into(),
            },
            Request::EnsureWinusb {
                instance_id: r"USB\VID_18D1&PID_2D01\R5CY204BABT".into(),
            },
        ];
        for req in cases {
            assert_eq!(Request::decode(&req.encode()).unwrap(), req);
        }
    }

    #[test]
    fn responses_round_trip() {
        let cases = [
            Response::Pong,
            Response::Ok {
                detail: "reinstalled oem6.inf".into(),
            },
            Response::Err {
                message: "not elevated".into(),
            },
        ];
        for resp in cases {
            assert_eq!(Response::decode(&resp.encode()).unwrap(), resp);
        }
    }

    #[test]
    fn rejects_empty_instance_id() {
        assert!(Request::decode("BEGIN_SWAP").is_err());
        assert!(Request::decode("ROLLBACK   ").is_err());
    }

    #[test]
    fn rejects_unknown_verbs() {
        assert!(Request::decode("DELETE_EVERYTHING foo").is_err());
    }

    #[test]
    fn multiline_detail_stays_one_line() {
        let encoded = Response::Ok {
            detail: "line1\nline2".into(),
        }
        .encode();
        assert!(!encoded[3..].contains('\n'));
    }

    // --- Live pipe tests ---
    //
    // These create a real pipe, so they must not run concurrently with each other or with a
    // running service: the pipe name is global. They are #[ignore]d for that reason, not
    // because of privilege — an unelevated client is expected to work, and one of these tests
    // exists precisely to keep that true.
    //
    //     cargo test --offline live_pipe -- --ignored --test-threads=1 --nocapture

    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// A request really does reach the server and the response comes back — and specifically,
    /// it does so from an *unelevated* client, which is how the daemon actually runs.
    #[test]
    #[ignore]
    fn live_pipe_round_trip() {
        // The server reports its own outcome rather than panicking on a background thread,
        // where the message would be easy to miss while the client fails with something vague.
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let outcome = (|| -> Result<Request, String> {
                let mut connection =
                    transport::accept_one(true).map_err(|e| format!("accept: {e}"))?;
                let _ = ready_tx.send(());
                let request = connection
                    .read_request()
                    .map_err(|e| format!("read: {e}"))?;
                connection
                    .respond(&Response::Pong)
                    .map_err(|e| format!("respond: {e}"))?;
                Ok(request)
            })();
            let _ = done_tx.send(outcome);
        });

        // Wait for the pipe to exist rather than racing a fixed sleep.
        let _ = ready_rx.recv_timeout(Duration::from_secs(5));

        let response = transport::send(&Request::Ping);
        let served = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("server thread should finish");

        assert_eq!(served, Ok(Request::Ping), "server side failed");
        assert_eq!(response.expect("client send failed"), Response::Pong);
    }

    /// `wake_server` unblocks an accept that's waiting for a client. This is what lets the
    /// service stop promptly instead of hanging until the next real request.
    #[test]
    #[ignore]
    fn live_pipe_wake_unblocks_accept() {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = transport::accept_one(true);
            let _ = tx.send(result.is_ok());
        });

        thread::sleep(Duration::from_millis(200));
        transport::wake_server();

        let woke = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("accept_one should return promptly after wake_server");
        assert!(woke, "accept_one should have succeeded, not errored");
    }
}
