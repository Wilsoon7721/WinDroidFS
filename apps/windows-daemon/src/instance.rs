use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// Another user signed in at the same time runs their own copy
const MUTEX_NAME: &str = "Local\\WinDroidFS.phone-link";

/// Proof this process is the only copy. Released when dropped (or when the process exits, however it exits).
pub struct Instance(HANDLE);

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// `None` if another copy is already running
pub fn claim() -> Option<Instance> {
    let name: Vec<u16> = MUTEX_NAME.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Some(Instance(handle));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(Instance(handle))
}
