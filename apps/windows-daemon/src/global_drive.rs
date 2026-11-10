//! Making the phone's drive usable by programs run as administrator by mounting a UNC path alongside the drive letter

use std::io;

use windows_sys::Win32::Storage::FileSystem::{
    DDD_EXACT_MATCH_ON_REMOVE, DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION, DefineDosDeviceW,
    GetVolumeInformationW, QueryDosDeviceW,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};

/// The filesystem name WinDroidFS mounts with. Only volumes reporting it are ever published.
pub const FILESYSTEM_NAME: &str = "WinDroidFS";

/// The server in the drive's UNC path, `\\windroidfs.invalid\<name>`. Without `.invalid`, NetBIOS broadcasts cause delay in loading the actual file list
pub const SERVER_NAME: &str = "windroidfs.invalid";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `Z:` for `'Z'`, or `None` for anything that isn't a drive letter
fn device_name(letter: char) -> Option<String> {
    letter
        .is_ascii_alphabetic()
        .then(|| format!("{}:", letter.to_ascii_uppercase()))
}

pub fn target_of(letter: char) -> Option<String> {
    let name = wide(&device_name(letter)?);
    let mut buf = vec![0u16; 1024];
    let len = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 {
        return None;
    }

    let first = buf.split(|&c| c == 0).next()?;
    Some(String::from_utf16_lossy(first))
}

/// Whether `target` displays a WinFsp volume like a WinDroidFS drive letter does: `\Device\Volume{GUID}`, followed for a network drive by `\<SERVER_NAME>\<name>`
pub fn is_volume_device(target: &str) -> bool {
    let Some(rest) = target.strip_prefix(r"\Device\Volume{") else {
        return false;
    };
    let Some((guid, after)) = rest.split_once('}') else {
        return false;
    };
    if !after.is_empty() {
        let share = after
            .strip_prefix('\\')
            .and_then(|a| a.split_once('\\'))
            .filter(|(server, _)| server.eq_ignore_ascii_case(SERVER_NAME))
            .map(|(_, share)| share);
        if !share.is_some_and(|share| !share.is_empty() && !share.contains('\\')) {
            return false;
        }
    }
    let groups: Vec<&str> = guid.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The filesystem name of the volume at `target`, or `None` if it isn't there anymore
pub fn filesystem_name(target: &str) -> Option<String> {
    let root = wide(&format!(r"\\?\GLOBALROOT{target}\"));
    let mut fs = vec![0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Some(String::from_utf16_lossy(&fs[..len]))
}

/// Show network drive as a `label` in Explorer
pub fn label_in_explorer(label: &str) {
    let key = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\MountPoints2\##{SERVER_NAME}#{}",
        label.replace('\\', "#")
    );
    let (key, name, value) = (wide(&key), wide("_LabelFromReg"), wide(label));
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        )
    };
    if status != 0 {
        log::debug!("Couldn't set Explorer's label for the drive ({status}).");
    }
}

/// Point `letter` at `target` in the caller's own namespace
pub fn define(letter: char, target: &str) -> io::Result<()> {
    let name = device_name(letter).ok_or_else(|| io::Error::other("not a drive letter"))?;
    let (name, target) = (wide(&name), wide(target));
    if unsafe { DefineDosDeviceW(DDD_RAW_TARGET_PATH, name.as_ptr(), target.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Remove `letter`, but only if it still points at `target`
pub fn remove(letter: char, target: &str) -> io::Result<()> {
    let name = device_name(letter).ok_or_else(|| io::Error::other("not a drive letter"))?;
    let (name, target) = (wide(&name), wide(target));
    let flags = DDD_RAW_TARGET_PATH | DDD_REMOVE_DEFINITION | DDD_EXACT_MATCH_ON_REMOVE;
    if unsafe { DefineDosDeviceW(flags, name.as_ptr(), target.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_volume_devices() {
        assert!(is_volume_device(
            r"\Device\Volume{1f4bf5e6-dcd5-11f1-9bda-001a7dda7115}"
        ));
        assert!(!is_volume_device(r"\Device\HarddiskVolume3"));
        assert!(!is_volume_device(r"\??\C:\Windows"));
        assert!(!is_volume_device(
            r"\Device\Volume{1f4bf5e6-dcd5-11f1-9bda-001a7dda7115}\x"
        ));
        assert!(!is_volume_device(r"\Device\Volume{not-a-guid}"));
        assert!(!is_volume_device(
            r"\Device\Volume{1f4bf5e6-dcd5-11f1-9bda-001a7dda711Z}"
        ));
    }

    #[test]
    fn accepts_windroidfs_network_volumes_only() {
        let volume = r"\Device\Volume{1f4bfdc6-dcd5-11f1-9bda-001a7dda7115}";
        assert!(is_volume_device(&format!(
            r"{volume}\windroidfs.invalid\Wilson's S25+"
        )));
        assert!(is_volume_device(&format!(
            r"{volume}\WinDroidFS.invalid\Pixel 9"
        )));
        assert!(!is_volume_device(&format!(r"{volume}\windroidfs\Pixel 9")));
        assert!(!is_volume_device(&format!(r"{volume}\otherserver\share")));
        assert!(!is_volume_device(&format!(r"{volume}\windroidfs.invalid\")));
        assert!(!is_volume_device(&format!(
            r"{volume}\windroidfs.invalid\share\deeper"
        )));
        assert!(!is_volume_device(&format!(r"{volume}\windroidfs.invalid")));
        assert!(!is_volume_device(&format!(
            r"{volume}x\windroidfs.invalid\share"
        )));
    }

    #[test]
    fn names_only_drive_letters() {
        assert_eq!(device_name('z').as_deref(), Some("Z:"));
        assert_eq!(device_name('1'), None);
        assert_eq!(device_name('é'), None);
    }
}
