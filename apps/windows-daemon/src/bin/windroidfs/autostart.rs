use std::ptr::null_mut;

use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "WinDroidFS";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Whether this app is set to start with Windows. Only counts if the entry points at this exe.
pub fn is_enabled() -> bool {
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(VALUE_NAME).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return false;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len])) == command()
}

pub fn set_enabled(enabled: bool) {
    let status = if enabled {
        let Some(command) = command() else { return };
        let data = wide(&command);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(VALUE_NAME).as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        }
    } else {
        unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(VALUE_NAME).as_ptr(),
            )
        }
    };
    if status != 0 {
        log::warn!("⚠️  Couldn't change \"Start with Windows\" (error {status}).");
    }
}

/// The command line Windows runs.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}
