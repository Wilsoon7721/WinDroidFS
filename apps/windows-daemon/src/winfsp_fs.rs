use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    LocalFree, STATUS_ACCESS_DENIED, STATUS_BUFFER_OVERFLOW, STATUS_DEVICE_NOT_CONNECTED,
    STATUS_DIRECTORY_NOT_EMPTY, STATUS_DISK_FULL, STATUS_END_OF_FILE, STATUS_FILE_IS_A_DIRECTORY,
    STATUS_INVALID_PARAMETER, STATUS_IO_DEVICE_ERROR, STATUS_MEDIA_WRITE_PROTECTED,
    STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, GetLogicalDrives,
};
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use winfsp::constants::FspCleanupFlags;
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{FspError, U16CStr};

use crate::phone::{MAX_READ, MAX_WRITE, PhoneClient, PhoneError};
use crate::windroid::{self as proto, ErrorCode};

/// How long a directory listing is trusted. Short, because the phone's files can change while mounted (a new photo, say): a file created on the phone shows up after at most this long.
const LISTING_TTL: Duration = Duration::from_secs(3);

/// How long Windows itself may cache file metadata before asking again (in milliseconds).
const KERNEL_INFO_TIMEOUT_MS: u32 = 1000;

/// Cluster size reported to Windows. Only affects "size on disk" in file properties.
const ALLOCATION_UNIT: u64 = 4096;

/// The drive's name in File Explorer when the phone doesn't say what it's called.
const FALLBACK_LABEL: &str = "Android";

/// The most UTF-16 units a volume label can have (WinFsp's buffer, and NTFS's own limit).
const MAX_LABEL_LEN: usize = 32;

/// FILETIME of the Unix epoch: 100ns intervals between 1601-01-01 and 1970-01-01.
const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;

/// `create_options` flag: the caller wants a directory. (An NT create option, defined here rather than pulling in the driver-kit bindings for one constant).
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

/// Why the WinFsp mount couldn't be used.
#[derive(Debug)]
pub enum MountError {
    /// WinFsp isn't installed. Expected on a fresh machine, the WebDAV mount still works.
    WinFspMissing,
    /// WinFsp is there but mounting failed.
    Failed(String),
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::WinFspMissing => write!(f, "WinFsp isn't installed"),
            MountError::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// Load WinFsp's DLL so the filesystem can run. Fails if WinFsp isn't installed.
pub fn load_winfsp() -> Result<(), MountError> {
    if let Some(dir) = winfsp_install_dir() {
        let dll = dir.join("bin").join(winfsp_dll_name());
        let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
        // Failure reported by winfsp_init.
        unsafe { LoadLibraryW(wide.as_ptr()) };
    }
    winfsp::winfsp_init()
        .map(|_| ())
        .map_err(|_| MountError::WinFspMissing)
}

fn winfsp_dll_name() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "winfsp-a64.dll"
    } else if cfg!(target_arch = "x86") {
        "winfsp-x86.dll"
    } else {
        "winfsp-x64.dll"
    }
}

/// WinFsp's `InstallDir`. The installer is 32-bit.
fn winfsp_install_dir() -> Option<PathBuf> {
    ["SOFTWARE\\WOW6432Node\\WinFsp", "SOFTWARE\\WinFsp"]
        .iter()
        .find_map(|key| read_registry_string(key, "InstallDir"))
        .map(PathBuf::from)
}

fn read_registry_string(key: &str, value: &str) -> Option<String> {
    let key = to_wide(key);
    let value = to_wide(value);
    let mut buf = [0u16; 512];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// How to mount the phone
#[derive(Clone, Debug)]
pub struct MountOptions {
    /// The drive's name in Explorer. Pass it through [`volume_label`] first.
    pub label: String,
    pub drive: DriveChoice,
    /// Refuse every change. Windows enforces it before a request ever gets here.
    pub read_only: bool,
    /// Mark dot-files (`.thumbnails`, `.nomedia`, ...) hidden, as Android and Linux treat them.
    pub hide_dotfiles: bool,
}

/// Which drive letter to mount on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DriveChoice {
    /// The first free letter, counting down from Z:
    #[default]
    Auto,
    /// This letter, or [`DriveChoice::Auto`]'s pick if it's taken.
    Letter(char),
}

/// A mounted phone. Unmounted by [`Mount::unmount`] or when dropped.
pub struct Mount {
    host: FileSystemHost<PhoneFs>,
    pub drive: char,
}

impl Mount {
    pub fn unmount(mut self) {
        self.host.stop();
        self.host.unmount();
    }
}

/// Mount the phone on the drive letter `options` asks for if it's free (or the first free one starting from Z:).
pub fn mount(phone: PhoneClient, options: &MountOptions) -> Result<Mount, MountError> {
    load_winfsp()?;

    let fs = PhoneFs {
        phone,
        label: options.label.clone(),
        hide_dotfiles: options.hide_dotfiles,
        read_only: options.read_only,
        security_descriptor: build_security_descriptor().map_err(MountError::Failed)?,
        listings: Mutex::new(HashMap::new()),
    };

    let mut params = VolumeParams::new();
    params
        .filesystem_name("WinDroidFS")
        .sector_size(512)
        .sectors_per_allocation_unit((ALLOCATION_UNIT / 512) as u16)
        .max_component_length(255)
        .volume_creation_time(filetime_now())
        .volume_serial_number(0x5744_4653) // "WDFS"
        .file_info_timeout(KERNEL_INFO_TIMEOUT_MS)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(false)
        .read_only_volume(options.read_only)
        .post_cleanup_when_modified_only(true);

    // The host's own calls return `windows::core::Error`, not the crate's `FspError`.
    let failed = |what: &str, e: &dyn std::fmt::Display| MountError::Failed(format!("{what}: {e}"));
    let mut host: FileSystemHost<PhoneFs> =
        FileSystemHost::new(params, fs).map_err(|e| failed("couldn't create the volume", &e))?;

    let drive = match options.drive {
        DriveChoice::Letter(letter) if drive_letter_free(letter) => Some(letter),
        _ => free_drive_letter(),
    }
    .ok_or_else(|| MountError::Failed("every drive letter is in use".into()))?;
    host.mount(format!("{drive}:"))
        .map_err(|e| failed(&format!("couldn't mount {drive}:"), &e))?;
    host.start()
        .map_err(|e| failed("couldn't start the filesystem", &e))?;

    Ok(Mount { host, drive })
}

/// First letter not in use, searching down from Z: (where the WebDAV mount used to live, and where removable drives rarely land).
fn free_drive_letter() -> Option<char> {
    ('D'..='Z').rev().find(|&letter| drive_letter_free(letter))
}

fn drive_letter_free(letter: char) -> bool {
    let letter = letter.to_ascii_uppercase();
    if !letter.is_ascii_uppercase() {
        return false;
    }
    let used = unsafe { GetLogicalDrives() };
    used & (1 << (letter as u8 - b'A')) == 0
}

/// Make a phone's name usable as a volume label.
pub fn volume_label(device_name: &str) -> String {
    let cleaned: String = device_name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();

    // Collapse the spaces that removed characters can leave.
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut label = String::new();
    let mut units = 0;
    for c in cleaned.chars() {
        units += c.len_utf16();
        if units > MAX_LABEL_LEN {
            break;
        }
        label.push(c);
    }
    let label = label.trim_end().to_string();
    if label.is_empty() {
        FALLBACK_LABEL.to_string()
    } else {
        label
    }
}

/// A fixed security descriptor for every file: owned by Administrators, full access for SYSTEM, Administrators and Everyone.
fn build_security_descriptor() -> Result<Vec<u8>, String> {
    let sddl = to_wide("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)");
    let mut descriptor: *mut c_void = null_mut();
    let mut len = 0u32;
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1, // SDDL_REVISION_1
            &mut descriptor,
            &mut len,
        )
    };
    if ok == 0 || descriptor.is_null() {
        return Err("couldn't build the security descriptor".into());
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(descriptor as *const u8, len as usize) }.to_vec();
    unsafe { LocalFree(descriptor) };
    Ok(bytes)
}

pub struct PhoneFs {
    phone: PhoneClient,
    /// The drive's name in Explorer, already made safe by [`volume_label`].
    label: String,
    hide_dotfiles: bool,
    read_only: bool,
    /// Self-relative security descriptor, copied into WinFsp's buffer on request.
    security_descriptor: Vec<u8>,
    listings: Mutex<HashMap<String, Listing>>,
}

struct Listing {
    entries: Vec<proto::FileInfo>,
    fetched: Instant,
}

/// An open file or directory.
pub struct PhoneFile {
    /// Separated by `/`, `/` for the root.
    path: Mutex<String>,
    info: Mutex<FileInfo>,
    /// Written to since the phone last synced it. Checked on cleanup and flush.
    dirty: AtomicBool,
    /// Entries handed to Windows across several `read_directory` calls. Only used for directories.
    dir_buffer: DirBuffer,
}

impl PhoneFile {
    fn new(path: String, info: FileInfo) -> Self {
        PhoneFile {
            path: Mutex::new(path),
            info: Mutex::new(info),
            dirty: AtomicBool::new(false),
            dir_buffer: DirBuffer::new(),
        }
    }

    fn path(&self) -> String {
        lock(&self.path).clone()
    }

    fn info(&self) -> FileInfo {
        lock(&self.info).clone()
    }

    fn set_info(&self, info: FileInfo) {
        *lock(&self.info) = info;
    }

    fn is_dir(&self) -> bool {
        lock(&self.info).file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// A poisoned lock only means another callback panicked mid-update. The data is still usable and refusing every later request would take the whole drive down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl PhoneFs {
    /// Fails with "The media is write protected" on a read-only mount as a locked SD card would. Checked by all write operations.
    fn writable(&self) -> winfsp::Result<()> {
        if self.read_only {
            return Err(FspError::NTSTATUS(STATUS_MEDIA_WRITE_PROTECTED));
        }
        Ok(())
    }

    /// Windows' view of a phone file, with this mount's options applied.
    fn file_info(&self, info: &proto::FileInfo) -> FileInfo {
        to_file_info(info, self.hide_dotfiles)
    }

    /// Metadata for `path`, from a recent listing of its parent if there is one.
    fn lookup(&self, path: &str) -> winfsp::Result<proto::FileInfo> {
        if let Some((parent, name)) = split_parent(path)
            && let Some(answer) = self.lookup_in_listing(parent, name)
        {
            return answer;
        }
        self.phone.stat(path).map_err(nt_error)
    }

    /// `None` when there's no usable listing of `parent`, so the caller should ask the phone.
    fn lookup_in_listing(
        &self,
        parent: &str,
        name: &str,
    ) -> Option<winfsp::Result<proto::FileInfo>> {
        let listings = lock(&self.listings);
        let listing = listings.get(&cache_key(parent))?;
        if listing.fetched.elapsed() > LISTING_TTL {
            return None;
        }
        let wanted = name.to_lowercase();
        Some(
            listing
                .entries
                .iter()
                .find(|e| e.name.to_lowercase() == wanted)
                .cloned()
                .ok_or(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
        )
    }

    /// List `path` from the phone and remember the result.
    fn list(&self, path: &str) -> winfsp::Result<Vec<proto::FileInfo>> {
        let entries = self.phone.list_dir(path).map_err(nt_error)?;
        let mut listings = lock(&self.listings);
        listings.retain(|_, l| l.fetched.elapsed() <= LISTING_TTL);
        listings.insert(
            cache_key(path),
            Listing {
                entries: entries.clone(),
                fetched: Instant::now(),
            },
        );
        Ok(entries)
    }

    /// Forget cached listings that a change to `path` makes wrong: its parent's (which lists it) and its own (if it's a folder).
    /// Without this, a file just created would still be "not found" according to the parent's listing until the TTL expires.
    fn invalidate(&self, path: &str) {
        let mut listings = lock(&self.listings);
        listings.remove(&cache_key(path));
        if let Some((parent, _)) = split_parent(path) {
            listings.remove(&cache_key(parent));
        }
    }

    /// Set a file's length on the phone and record the result.
    fn truncate_to(&self, context: &PhoneFile, size: u64) -> winfsp::Result<()> {
        let path = context.path();
        let result = self.phone.truncate(&path, size);
        self.invalidate(&path);
        context.set_info(self.file_info(&result.map_err(nt_error)?));
        context.dirty.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Have the phone close and sync a file this handle has written to.
    fn sync(&self, context: &PhoneFile) -> winfsp::Result<()> {
        if context.dirty.swap(false, Ordering::SeqCst)
            && let Err(e) = self.phone.flush(&context.path())
        {
            // Still unsynced: let a later flush or cleanup try again.
            context.dirty.store(true, Ordering::SeqCst);
            return Err(nt_error(e));
        }
        Ok(())
    }

    /// Copy the fixed security descriptor into WinFsp's buffer if it supplied one.
    fn write_security_descriptor(&self, buffer: Option<&mut [c_void]>) -> winfsp::Result<u64> {
        let sd = &self.security_descriptor;
        if let Some(buffer) = buffer {
            // winfsp-rs reports success after this returns without checking the size, so a short buffer has to be caught here rather than overrun.
            if buffer.len() < sd.len() {
                return Err(FspError::NTSTATUS(STATUS_BUFFER_OVERFLOW));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(sd.as_ptr(), buffer.as_mut_ptr().cast(), sd.len())
            };
        }
        Ok(sd.len() as u64)
    }
}

impl FileSystemContext for PhoneFs {
    type FileContext = PhoneFile;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let info = self.lookup(&android_path(file_name))?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: self.write_security_descriptor(security_descriptor)?,
            attributes: attributes_of(&info, self.hide_dotfiles),
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<PhoneFile> {
        let path = android_path(file_name);
        let info = self.file_info(&self.lookup(&path)?);
        *file_info.as_mut() = info.clone();
        Ok(PhoneFile::new(path, info))
    }

    fn close(&self, _context: PhoneFile) {}

    fn get_file_info(&self, context: &PhoneFile, file_info: &mut FileInfo) -> winfsp::Result<()> {
        *file_info = context.info();
        Ok(())
    }

    fn get_security(
        &self,
        _context: &PhoneFile,
        security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        self.write_security_descriptor(security_descriptor)
    }

    fn read_directory(
        &self,
        context: &PhoneFile,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        // The buffer is filled once, on the first call for this listing (no marker), and then read out in pieces across later calls.
        if let Ok(filler) = context.dir_buffer.acquire(marker.is_none(), None) {
            let path = context.path();
            let entries = self.list(&path)?;

            // Every directory except the root lists "." and ".." as on NTFS.
            if path != "/" {
                for dot in [".", ".."] {
                    let mut entry: DirInfo = DirInfo::new();
                    entry.set_name(dot)?;
                    *entry.file_info_mut() = context.info();
                    filler.write(&mut entry)?;
                }
            }
            for file in &entries {
                let mut entry: DirInfo = DirInfo::new();
                entry.set_name(&file.name)?;
                *entry.file_info_mut() = self.file_info(file);
                filler.write(&mut entry)?;
            }
        }
        Ok(context.dir_buffer.read(marker, buffer))
    }

    fn read(&self, context: &PhoneFile, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        let size = context.info().file_size;
        if offset >= size {
            return Err(FspError::NTSTATUS(STATUS_END_OF_FILE));
        }

        // One phone request carries at most MAX_READ, so fill bigger reads in pieces.
        let path = context.path();
        let want = buffer.len().min((size - offset) as usize);
        let mut filled = 0;
        while filled < want {
            let len = (want - filled).min(MAX_READ as usize) as u32;
            let chunk = self
                .phone
                .read(&path, offset + filled as u64, len)
                .map_err(nt_error)?;
            let n = chunk.data.len().min(want - filled);
            buffer[filled..filled + n].copy_from_slice(&chunk.data[..n]);
            filled += n;
            if chunk.eof || n == 0 {
                break;
            }
        }
        Ok(filled as u32)
    }

    // Writes

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        _file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<PhoneFile> {
        self.writable()?;
        let path = android_path(file_name);
        let is_directory = create_options & FILE_DIRECTORY_FILE != 0;
        let created = self.phone.create(&path, is_directory);
        self.invalidate(&path);
        let info = self.file_info(&created.map_err(nt_error)?);
        *file_info.as_mut() = info.clone();
        Ok(PhoneFile::new(path, info))
    }

    /// Empty a file when opening it with replace semantics.
    fn overwrite(
        &self,
        context: &PhoneFile,
        _file_attributes: u32,
        _replace_file_attributes: bool,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.writable()?;
        self.truncate_to(context, 0)?;
        *file_info = context.info();
        Ok(())
    }

    fn write(
        &self,
        context: &PhoneFile,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        self.writable()?;
        let path = context.path();
        let size = context.info().file_size;
        let offset = if write_to_eof { size } else { offset };

        // A constrained I/O should keep only the part lands before its current end.
        let data = if constrained_io {
            if offset >= size {
                *file_info = context.info();
                return Ok(0);
            }
            &buffer[..buffer.len().min((size - offset) as usize)]
        } else {
            buffer
        };

        // One phone request carries at most MAX_WRITE, so send bigger writes in pieces.
        let mut written = 0usize;
        let mut failure = None;
        for chunk in data.chunks(MAX_WRITE) {
            match self.phone.write(&path, offset + written as u64, chunk) {
                Ok((n, info)) => {
                    written += n as usize;
                    context.set_info(self.file_info(&info));
                    if (n as usize) < chunk.len() {
                        break; // the phone stopped short (full storage?)
                    }
                }
                Err(e) => {
                    failure = Some(nt_error(e));
                    break;
                }
            }
        }

        if written > 0 {
            context.dirty.store(true, Ordering::SeqCst);
            self.invalidate(&path);
        }
        *file_info = context.info();
        // A partly completed write is reported as what got through. Only a write where nothing landed is an error.
        match failure {
            Some(e) if written == 0 => Err(e),
            _ => Ok(written as u32),
        }
    }

    fn set_file_size(
        &self,
        context: &PhoneFile,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.writable()?;
        let current = context.info();
        if set_allocation_size {
            // Windows reserving space before writing (a copy does this first).
            // The phone has no such thing, so only a reservation smaller than the data cuts the file.
            if new_size < current.file_size {
                self.truncate_to(context, new_size)?;
            } else {
                let mut info = current;
                info.allocation_size = new_size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT;
                context.set_info(info);
            }
        } else if new_size != current.file_size {
            self.truncate_to(context, new_size)?;
        }
        *file_info = context.info();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn set_basic_info(
        &self,
        context: &PhoneFile,
        _file_attributes: u32,
        _creation_time: u64,
        _last_access_time: u64,
        last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.writable()?;
        // Only the modification time means anything: Attributes come from the name (dot-files are hidden) and Android keeps no creation time. 0 means "leave it".
        if last_write_time != 0 {
            let path = context.path();
            self.phone
                .set_modified(&path, unix_ms_from_filetime(last_write_time))
                .map_err(nt_error)?;
            let mut info = context.info();
            info.last_write_time = last_write_time;
            info.change_time = last_write_time;
            context.set_info(info);
            self.invalidate(&path);
        }
        *file_info = context.info();
        Ok(())
    }

    /// Only a check. Windows deletes in steps, and the file is actually removed in `cleanup` once it's sure. A folder must be empty.
    fn set_delete(
        &self,
        context: &PhoneFile,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        self.writable()?;
        if delete_file && context.is_dir() && !self.list(&context.path())?.is_empty() {
            return Err(FspError::NTSTATUS(STATUS_DIRECTORY_NOT_EMPTY));
        }
        Ok(())
    }

    fn rename(
        &self,
        context: &PhoneFile,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        self.writable()?;
        let from = android_path(file_name);
        let to = android_path(new_file_name);
        let result = self.phone.rename(&from, &to, replace_if_exists);
        // Renaming a folder moves everything under it too, so rather than work out which cached listings are affected, drop them all.
        lock(&self.listings).clear();
        result.map_err(nt_error)?;
        *lock(&context.path) = to;
        Ok(())
    }

    /// The last handle to a file was closed. Delete it if that's what Windows asked for, otherwise sync it if it was written.
    fn cleanup(&self, context: &PhoneFile, _file_name: Option<&U16CStr>, flags: u32) {
        let path = context.path();
        if FspCleanupFlags::FspCleanupDelete.is_flagged(flags) {
            // The phone closes the file if it has it open for writing, so no sync is needed.
            context.dirty.store(false, Ordering::SeqCst);
            if let Err(e) = self.phone.delete(&path) {
                log::warn!("⚠️  Couldn't delete {path} on the phone: {e}");
            }
            self.invalidate(&path);
        } else if self.sync(context).is_err() {
            log::warn!("⚠️  Couldn't sync {path} on the phone");
        }
    }

    fn flush(&self, context: Option<&PhoneFile>, file_info: &mut FileInfo) -> winfsp::Result<()> {
        let Some(context) = context else {
            return Ok(());
        };
        self.sync(context)?;
        *file_info = context.info();
        Ok(())
    }

    // Volume

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        let volume = self.phone.volume_info().map_err(nt_error)?;
        out_volume_info.total_size = volume.total_bytes;
        out_volume_info.free_size = volume.free_bytes;
        out_volume_info.set_volume_label(&self.label);
        Ok(())
    }
}

/// `\DCIM\Camera` (as WinFsp passes it) -> `/DCIM/Camera` (as the phone expects).
fn android_path(file_name: &U16CStr) -> String {
    let path = file_name.to_string_lossy().replace('\\', "/");
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `/DCIM/Camera` -> `("/DCIM", "Camera")`. `None` for the root, which has no parent.
fn split_parent(path: &str) -> Option<(&str, &str)> {
    let (parent, name) = path.rsplit_once('/')?;
    if name.is_empty() {
        return None;
    }
    Some((if parent.is_empty() { "/" } else { parent }, name))
}

fn cache_key(path: &str) -> String {
    path.to_lowercase()
}

fn attributes_of(info: &proto::FileInfo, hide_dotfiles: bool) -> u32 {
    let mut attributes = if info.is_directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        0
    };
    // Dot-files are hidden on Android and Linux by convention (.thumbnails, .nomedia, ...).
    // Showing them as hidden keeps Explorer's default view as uncluttered as the phone's.
    if hide_dotfiles && info.name.starts_with('.') {
        attributes |= FILE_ATTRIBUTE_HIDDEN;
    }
    // Deliberately never READONLY: Android has no such attribute, and Windows copies attributes along with files, so a read-only flag would follow every file copied off the phone.
    if attributes == 0 {
        FILE_ATTRIBUTE_NORMAL
    } else {
        attributes
    }
}

fn to_file_info(info: &proto::FileInfo, hide_dotfiles: bool) -> FileInfo {
    let size = info.size_bytes.max(0) as u64;
    // Android reports only a modification time. Used for all timestamps.
    let time = filetime_from_unix_ms(info.last_modified_ms);
    FileInfo {
        file_attributes: attributes_of(info, hide_dotfiles),
        file_size: size,
        allocation_size: size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT,
        creation_time: time,
        last_access_time: time,
        last_write_time: time,
        change_time: time,
        ..Default::default()
    }
}

fn filetime_from_unix_ms(ms: i64) -> u64 {
    UNIX_EPOCH_AS_FILETIME + (ms.max(0) as u64) * 10_000
}

fn unix_ms_from_filetime(filetime: u64) -> i64 {
    (filetime.saturating_sub(UNIX_EPOCH_AS_FILETIME) / 10_000) as i64
}

fn filetime_now() -> u64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    filetime_from_unix_ms(ms)
}

/// The Windows error matching a phone failure, so Explorer shows "Access is denied" or "cannot find the file" rather than a generic error.
fn nt_error(e: PhoneError) -> FspError {
    let status = match &e {
        PhoneError::Remote { code, .. } => match code {
            ErrorCode::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
            ErrorCode::AccessDenied => STATUS_ACCESS_DENIED,
            ErrorCode::NotADirectory => STATUS_NOT_A_DIRECTORY,
            ErrorCode::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
            ErrorCode::InvalidRequest => STATUS_INVALID_PARAMETER,
            ErrorCode::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
            ErrorCode::DirectoryNotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
            ErrorCode::NoSpace => STATUS_DISK_FULL,
            ErrorCode::IoError | ErrorCode::Ok => STATUS_IO_DEVICE_ERROR,
        },
        PhoneError::Disconnected => STATUS_DEVICE_NOT_CONNECTED,
        PhoneError::Transport(_) | PhoneError::UnexpectedReply => STATUS_IO_DEVICE_ERROR,
    };

    if !matches!(e, PhoneError::Remote { .. } | PhoneError::Disconnected) {
        log::warn!("⚠️  {e}");
    }
    FspError::NTSTATUS(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winfsp::U16CString;

    fn wide(s: &str) -> U16CString {
        U16CString::from_str(s).unwrap()
    }

    #[test]
    fn converts_windows_paths() {
        assert_eq!(android_path(&wide("\\")), "/");
        assert_eq!(android_path(&wide("")), "/");
        assert_eq!(android_path(&wide("\\DCIM")), "/DCIM");
        assert_eq!(android_path(&wide("\\DCIM\\Camera\\")), "/DCIM/Camera");
        assert_eq!(
            android_path(&wide("\\My Files\\café.jpg")),
            "/My Files/café.jpg"
        );
    }

    #[test]
    fn splits_parent_and_name() {
        assert_eq!(split_parent("/"), None);
        assert_eq!(split_parent("/DCIM"), Some(("/", "DCIM")));
        assert_eq!(split_parent("/DCIM/Camera"), Some(("/DCIM", "Camera")));
    }

    #[test]
    fn hides_dotfiles_and_never_marks_readonly() {
        let file = |name: &str, dir: bool| proto::FileInfo {
            name: name.into(),
            is_directory: dir,
            ..Default::default()
        };
        assert_eq!(
            attributes_of(&file("photo.jpg", false), true),
            FILE_ATTRIBUTE_NORMAL
        );
        assert_eq!(
            attributes_of(&file("DCIM", true), true),
            FILE_ATTRIBUTE_DIRECTORY
        );
        assert_eq!(
            attributes_of(&file(".thumbnails", true), true),
            FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_HIDDEN
        );
        assert_eq!(
            attributes_of(&file(".nomedia", false), true),
            FILE_ATTRIBUTE_HIDDEN
        );
        assert_eq!(
            attributes_of(&file(".nomedia", false), false),
            FILE_ATTRIBUTE_NORMAL
        );
    }

    #[test]
    fn converts_unix_time_to_filetime() {
        assert_eq!(filetime_from_unix_ms(0), UNIX_EPOCH_AS_FILETIME);
        // 2024-01-01T00:00:00Z
        assert_eq!(
            filetime_from_unix_ms(1_704_067_200_000),
            133_485_408_000_000_000
        );
        // Negative (unknown) times clamp to the epoch rather than wrapping.
        assert_eq!(filetime_from_unix_ms(-5), UNIX_EPOCH_AS_FILETIME);
    }

    #[test]
    #[ignore]
    fn mounts_and_serves_a_folder() {
        use std::fs;
        use std::os::windows::fs::MetadataExt;

        // A folder standing in for the phone's storage.
        let backing = std::env::temp_dir().join(format!("windroidfs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&backing);
        fs::create_dir_all(backing.join("DCIM").join("Camera")).unwrap();
        fs::write(backing.join("hello.txt"), b"hello world").unwrap();
        fs::write(backing.join(".nomedia"), b"").unwrap();
        fs::write(backing.join("My Files.txt"), "spaces and café").unwrap();
        // Over MAX_READ (1 MiB), so a read has to be split across several phone requests.
        let big: Vec<u8> = (0..2_500_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(backing.join("DCIM").join("Camera").join("video.mp4"), &big).unwrap();

        let options = MountOptions {
            label: "Test Phone".into(),
            drive: DriveChoice::Auto,
            read_only: false,
            hide_dotfiles: true,
        };
        let mount = mount(PhoneClient::local(backing.clone()), &options).expect("mount failed");
        let root = format!("{}:\\", mount.drive);
        println!("mounted at {root}");

        // Listing, including names with spaces and non-ASCII characters.
        let mut names: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [".nomedia", "DCIM", "My Files.txt", "hello.txt"]);

        // Small and large reads, the large one spanning several MAX_READ chunks.
        assert_eq!(
            fs::read(format!("{root}hello.txt")).unwrap(),
            b"hello world"
        );
        assert_eq!(
            fs::read_to_string(format!("{root}My Files.txt")).unwrap(),
            "spaces and café"
        );
        let video = format!("{root}DCIM\\Camera\\video.mp4");
        assert_eq!(fs::metadata(&video).unwrap().len(), big.len() as u64);
        assert!(
            fs::read(&video).unwrap() == big,
            "large file content differs"
        );

        // Case-insensitive lookups, as Windows expects.
        assert_eq!(
            fs::read(format!("{root}HELLO.TXT")).unwrap(),
            b"hello world"
        );

        // Attributes: dot-files hidden, nothing marked read-only.
        let attrs = |p: &str| {
            fs::metadata(format!("{root}{p}"))
                .unwrap()
                .file_attributes()
        };
        assert_ne!(attrs(".nomedia") & FILE_ATTRIBUTE_HIDDEN, 0);
        assert_eq!(attrs("hello.txt") & 0x1, 0, "files must not be READONLY");

        // A missing file is "not found", not some generic failure.
        let missing = fs::read(format!("{root}nope.txt")).unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

        // --- Writing ---

        // A new file, written in one go, lands on the "phone" and is visible straight away
        // (the parent's cached listing must not still say it doesn't exist).
        fs::write(format!("{root}new.txt"), b"fresh").unwrap();
        assert_eq!(fs::read(backing.join("new.txt")).unwrap(), b"fresh");
        assert_eq!(fs::read(format!("{root}new.txt")).unwrap(), b"fresh");

        // Overwriting replaces the content, including making it shorter.
        fs::write(format!("{root}new.txt"), b"hi").unwrap();
        assert_eq!(fs::read(backing.join("new.txt")).unwrap(), b"hi");

        // A large write, over MAX_WRITE, so it's split across several phone requests. This
        // is Explorer's path for a copy: fs::copy uses CopyFileEx.
        let local_big = backing.with_extension("big");
        fs::write(&local_big, &big).unwrap();
        fs::copy(&local_big, format!("{root}DCIM\\copied.mp4")).unwrap();
        let _ = fs::remove_file(&local_big);
        assert!(fs::read(backing.join("DCIM").join("copied.mp4")).unwrap() == big);

        // Appending.
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(format!("{root}new.txt"))
                .unwrap();
            f.write_all(b" there").unwrap();
        }
        assert_eq!(fs::read(backing.join("new.txt")).unwrap(), b"hi there");

        // Truncating through set_len.
        fs::OpenOptions::new()
            .write(true)
            .open(format!("{root}new.txt"))
            .unwrap()
            .set_len(2)
            .unwrap();
        assert_eq!(fs::read(backing.join("new.txt")).unwrap(), b"hi");

        // Folders: create, rename, and refuse to delete one that isn't empty.
        fs::create_dir(format!("{root}Folder")).unwrap();
        assert!(backing.join("Folder").is_dir());
        fs::write(format!("{root}Folder\\inside.txt"), b"x").unwrap();
        fs::rename(format!("{root}Folder"), format!("{root}Renamed")).unwrap();
        assert!(backing.join("Renamed").join("inside.txt").is_file());
        assert!(fs::remove_dir(format!("{root}Renamed")).is_err());

        // Renaming a file, including a case-only rename (the same file on both sides).
        fs::rename(format!("{root}new.txt"), format!("{root}New.txt")).unwrap();
        fs::rename(format!("{root}New.txt"), format!("{root}moved.txt")).unwrap();
        assert!(!backing.join("new.txt").exists());
        assert_eq!(fs::read(backing.join("moved.txt")).unwrap(), b"hi");

        // Replacing an existing file by renaming onto it.
        fs::rename(format!("{root}moved.txt"), format!("{root}hello.txt")).unwrap();
        assert_eq!(fs::read(backing.join("hello.txt")).unwrap(), b"hi");

        // Deleting: files, then the now-empty folder.
        fs::remove_file(format!("{root}Renamed\\inside.txt")).unwrap();
        fs::remove_dir(format!("{root}Renamed")).unwrap();
        assert!(!backing.join("Renamed").exists());
        assert!(fs::metadata(format!("{root}Renamed")).is_err());

        // Creating something that already exists is refused.
        assert!(fs::create_dir(format!("{root}DCIM")).is_err());

        mount.unmount();

        // Read-only, with dot-files shown: writes are refused before they reach the phone.
        let options = MountOptions {
            read_only: true,
            hide_dotfiles: false,
            ..options
        };
        let read_only =
            super::mount(PhoneClient::local(backing.clone()), &options).expect("mount failed");
        let root = format!("{}:\\", read_only.drive);
        assert_eq!(fs::read(format!("{root}hello.txt")).unwrap(), b"hi");
        assert!(fs::write(format!("{root}refused.txt"), b"x").is_err());
        assert!(!backing.join("refused.txt").exists());
        let nomedia = fs::metadata(format!("{root}.nomedia")).unwrap();
        assert_eq!(nomedia.file_attributes() & FILE_ATTRIBUTE_HIDDEN, 0);
        read_only.unmount();

        let _ = fs::remove_dir_all(&backing);
    }

    #[test]
    fn makes_device_names_into_volume_labels() {
        assert_eq!(volume_label("Wilson's S25+"), "Wilson's S25+");
        assert_eq!(
            volume_label("Wilson\u{2019}s Pixel"),
            "Wilson\u{2019}s Pixel"
        );
        assert_eq!(volume_label("a/b\\c:d*e?f\"g<h>i|j"), "a b c d e f g h i j");
        assert_eq!(volume_label("tab\there\nnewline"), "tab here newline");
        assert_eq!(volume_label("  spaced   out  "), "spaced out");
        assert_eq!(volume_label(""), "Android");
        assert_eq!(volume_label("???"), "Android");

        // 32 UTF-16 units at most, cut between characters.
        let long = "x".repeat(40);
        assert_eq!(volume_label(&long).len(), 32);
        // 31 x's then an emoji (2 units): the emoji doesn't fit and is dropped whole.
        let emoji = format!("{}\u{1F4F1}", "x".repeat(31));
        assert_eq!(volume_label(&emoji), "x".repeat(31));
        let fits = format!("{}\u{1F4F1}", "x".repeat(30));
        assert_eq!(volume_label(&fits), fits);
    }

    #[test]
    fn allocation_size_rounds_up_to_a_cluster() {
        let size = |n| {
            to_file_info(
                &proto::FileInfo {
                    size_bytes: n,
                    ..Default::default()
                },
                true,
            )
            .allocation_size
        };
        assert_eq!(size(0), 0);
        assert_eq!(size(1), 4096);
        assert_eq!(size(4096), 4096);
        assert_eq!(size(4097), 8192);
    }
}
