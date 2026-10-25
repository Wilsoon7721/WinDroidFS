//! Code shared by the two WinDroidFS executables:
//!
//! - `windows-daemon` (src/main.rs): runs as the normal user; does the USB handshake and talks
//!   to the phone.
//! - `windroidfs-service` (src/bin/windroidfs-service.rs): a Windows service running as SYSTEM;
//!   the only part allowed to switch drivers.
//!
//! The daemon asks the service for driver changes over a named pipe (see `ipc`).

pub mod driver;
pub mod ipc;

pub mod android {
    //! USB identities shared by the daemon and the driver service.

    /// Standard Android manufacturer vendor IDs
    pub const ANDROID_VIDS: &[u16] = &[
        0x04e8, // Samsung
        0x18d1, // Google / Nexus / Pixel
        0x22d9, // OnePlus / Oppo
        0x2717, // Xiaomi
        0x1004, // LG
        0x0fce, // Sony
        0x22b8, // Motorola
        0x2a70, // OnePlus
    ];

    /// The phone's identity once it's in Accessory Mode (Google VID, AOA PIDs)
    pub const AOA_VID: u16 = 0x18d1;
    pub const AOA_PIDS: std::ops::RangeInclusive<u16> = 0x2d00..=0x2d05;

    pub fn is_aoa_identity(vid: u16, pid: u16) -> bool {
        vid == AOA_VID && AOA_PIDS.contains(&pid)
    }

    /// A phone in its normal (MTP / file transfer) mode, i.e. what the AOA handshake is sent to.
    pub fn is_normal_android_identity(vid: u16, pid: u16) -> bool {
        ANDROID_VIDS.contains(&vid) && !is_aoa_identity(vid, pid)
    }

    /// Every accessory identity as `(vid, pid)` pairs.
    pub fn aoa_identities() -> Vec<(u16, u16)> {
        AOA_PIDS.map(|pid| (AOA_VID, pid)).collect()
    }
}
