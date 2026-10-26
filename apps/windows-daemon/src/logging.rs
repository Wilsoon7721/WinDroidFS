//! Where progress messages go: the console when there is one, a log file when it's switched on.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use log::{Level, LevelFilter, Log, Metadata, Record};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_SIZE: u64 = 1024 * 1024;
const LOG_NAME: &str = "windroidfs.log";
const OLD_LOG_NAME: &str = "windroidfs.old.log";

struct Logger {
    console: AtomicBool,
    /// The open log file and how many bytes are in it
    file: Mutex<Option<(File, u64)>>,
}

static LOGGER: Logger = Logger {
    console: AtomicBool::new(false),
    file: Mutex::new(None),
};

/// Start logging, to the console or not.
pub fn init(console: bool) {
    LOGGER.console.store(console, Ordering::SeqCst);
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

/// Switch the log file on or off. Safe to call at any time from any thread.
pub fn set_file_logging(enabled: bool) {
    let mut file = LOGGER.file.lock().unwrap_or_else(|e| e.into_inner());
    *file = if enabled { open_log() } else { None };
}

/// The folder the log file is in - used for "Open log folder"
pub fn log_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("WinDroidFS"))
}

/// Open the log for appending - first moving it aside if it's already full
fn open_log() -> Option<(File, u64)> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(LOG_NAME);
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size >= MAX_LOG_SIZE {
        let _ = std::fs::rename(&path, dir.join(OLD_LOG_NAME));
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some((file, size))
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let target = metadata.target();
        target.starts_with("windroidfs") || target.starts_with("windows_daemon")
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        if self.console.load(Ordering::SeqCst) {
            if record.level() <= Level::Warn {
                eprintln!("{}", record.args());
            } else {
                println!("{}", record.args());
            }
        }

        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let Some((handle, size)) = file.as_mut() else {
            return;
        };
        let line = format!(
            "{} {:<5} {}\r\n",
            timestamp(),
            record.level(),
            record.args()
        );
        if handle.write_all(line.as_bytes()).is_err() {
            return;
        }
        *size += line.len() as u64;
        if *size >= MAX_LOG_SIZE {
            *file = None; // close it so it can be renamed
            *file = open_log();
        }
    }

    fn flush(&self) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((handle, _)) = file.as_mut() {
            let _ = handle.flush();
        }
    }
}

/// Local time as `YYYY-MM-DD HH:MM:SS`
fn timestamp() -> String {
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
