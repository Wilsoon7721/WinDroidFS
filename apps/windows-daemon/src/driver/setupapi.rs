use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::Properties::{DEVPKEY_Device_DriverInfPath, DEVPROPTYPE};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Registry::{RegCloseKey, RegQueryValueExW};

use super::DriverError;

const INVALID_SET: HDEVINFO = -1;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn last_error(call: &'static str) -> DriverError {
    DriverError::Win32 {
        call,
        code: unsafe { GetLastError() },
    }
}

fn new_devinfo_data() -> SP_DEVINFO_DATA {
    let mut data: SP_DEVINFO_DATA = unsafe { zeroed() };
    data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    data
}

/// One devnode inside a [`DevInfoSet`]. Only valid together with the set it came from.
pub struct DeviceNode {
    pub instance_id: String,
    data: SP_DEVINFO_DATA,
}

/// One driver package that could be installed on a device.
/// Only valid together with the set (and device) it was listed for.
pub struct DriverCandidate {
    /// File name in the driver store, e.g. `oem6.inf`.
    pub inf_name: String,
    /// Full path to that INF, e.g. `C:\Windows\INF\oem6.inf`. Needed to force an install.
    pub inf_path: PathBuf,
    pub provider: String,
    pub description: String,
    /// Windows' own preference score. Lower is better.
    pub rank: u32,
    /// Driver date as a FILETIME (100ns ticks). Windows breaks rank ties with the newest date.
    pub date: u64,
    /// Is this a Zadig/libwdi WinUSB package? Samsung's WinUSB-based drivers ("WinUsb v2",
    /// e.g. ssudaoa.inf) don't count: they don't register the DeviceInterfaceGUIDs libusb
    /// needs, so libusb still can't open the device.
    pub is_zadig_winusb: bool,
    info: SP_DRVINFO_DATA_V2_W,
}

pub enum Presence {
    /// Plugged in right now.
    Present,
    /// Windows remembers it, but it isn't plugged in (or has re-enumerated as something else).
    Disconnected,
}

/// Owns a SetupAPI `HDEVINFO` handle and frees it on drop.
pub struct DevInfoSet(HDEVINFO);

impl Drop for DevInfoSet {
    fn drop(&mut self) {
        // Also frees any driver lists built on this set.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

impl DevInfoSet {
    /// Every USB devnode Windows remembers, including disconnected ones.
    pub fn all_usb() -> Result<Self, DriverError> {
        let enumerator = to_wide("USB");
        let set = unsafe {
            SetupDiGetClassDevsW(null(), enumerator.as_ptr(), null_mut(), DIGCF_ALLCLASSES)
        };
        if set == INVALID_SET {
            return Err(last_error("SetupDiGetClassDevsW"));
        }
        Ok(Self(set))
    }

    /// A set containing exactly one devnode (connected or not).
    pub fn open(instance_id: &str) -> Result<(Self, DeviceNode), DriverError> {
        let set = unsafe { SetupDiCreateDeviceInfoList(null(), null_mut()) };
        if set == INVALID_SET {
            return Err(last_error("SetupDiCreateDeviceInfoList"));
        }
        let set = Self(set);

        let id = to_wide(instance_id);
        let mut data = new_devinfo_data();
        if unsafe { SetupDiOpenDeviceInfoW(set.0, id.as_ptr(), null_mut(), 0, &mut data) } == 0 {
            return Err(DriverError::DeviceNotFound(instance_id.to_string()));
        }

        let node = DeviceNode {
            instance_id: instance_id.to_string(),
            data,
        };
        Ok((set, node))
    }

    pub fn devices(&self) -> Vec<DeviceNode> {
        let mut nodes = Vec::new();
        for index in 0.. {
            let mut data = new_devinfo_data();
            if unsafe { SetupDiEnumDeviceInfo(self.0, index, &mut data) } == 0 {
                break; // ERROR_NO_MORE_ITEMS
            }
            if let Some(instance_id) = self.instance_id(&data) {
                nodes.push(DeviceNode { instance_id, data });
            }
        }
        nodes
    }

    fn instance_id(&self, data: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buf = [0u16; 512];
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(
                self.0,
                data,
                buf.as_mut_ptr(),
                buf.len() as u32,
                null_mut(),
            )
        };
        (ok != 0).then(|| from_wide(&buf))
    }

    /// The kernel service currently bound to the device, e.g. `WinUSB` or `dg_ssudbus`.
    pub fn service(&self, node: &DeviceNode) -> Option<String> {
        let mut buf = [0u16; 256];
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                self.0,
                &node.data,
                SPDRP_SERVICE,
                null_mut(),
                buf.as_mut_ptr() as *mut u8,
                size_of_val(&buf) as u32,
                null_mut(),
            )
        };
        (ok != 0).then(|| from_wide(&buf))
    }

    /// The driver-store INF the device is currently installed with, e.g. `oem10.inf`.
    pub fn current_inf(&self, node: &DeviceNode) -> Option<String> {
        let mut buf = [0u16; 260];
        let mut prop_type: DEVPROPTYPE = 0;
        let ok = unsafe {
            SetupDiGetDevicePropertyW(
                self.0,
                &node.data,
                &DEVPKEY_Device_DriverInfPath,
                &mut prop_type,
                buf.as_mut_ptr() as *mut u8,
                size_of_val(&buf) as u32,
                null_mut(),
                0,
            )
        };
        (ok != 0).then(|| from_wide(&buf))
    }

    /// Every driver package in the store that matches this device.
    pub fn compatible_drivers(
        &self,
        node: &DeviceNode,
    ) -> Result<Vec<DriverCandidate>, DriverError> {
        // The API wants a *mut, but only uses it to find the device inside the set.
        let mut data = node.data;
        if unsafe { SetupDiBuildDriverInfoList(self.0, &mut data, SPDIT_COMPATDRIVER) } == 0 {
            return Err(last_error("SetupDiBuildDriverInfoList"));
        }

        let mut drivers = Vec::new();
        for index in 0.. {
            let mut info: SP_DRVINFO_DATA_V2_W = unsafe { zeroed() };
            info.cbSize = size_of::<SP_DRVINFO_DATA_V2_W>() as u32;
            if unsafe {
                SetupDiEnumDriverInfoW(self.0, &data, SPDIT_COMPATDRIVER, index, &mut info)
            } == 0
            {
                break; // ERROR_NO_MORE_ITEMS
            }

            let Some(inf_path) = self.driver_inf_path(&data, &info) else {
                continue;
            };

            let mut params: SP_DRVINSTALL_PARAMS = unsafe { zeroed() };
            params.cbSize = size_of::<SP_DRVINSTALL_PARAMS>() as u32;
            let rank = if unsafe {
                SetupDiGetDriverInstallParamsW(self.0, &data, &info, &mut params)
            } != 0
            {
                params.Rank
            } else {
                u32::MAX
            };

            let provider = from_wide(&info.ProviderName).trim().to_string();
            let is_zadig_winusb =
                provider.eq_ignore_ascii_case("libwdi") && inf_installs_winusb(&inf_path);
            drivers.push(DriverCandidate {
                inf_name: inf_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                inf_path: inf_path.clone(),
                provider,
                description: from_wide(&info.Description),
                rank,
                date: ((info.DriverDate.dwHighDateTime as u64) << 32)
                    | info.DriverDate.dwLowDateTime as u64,
                is_zadig_winusb,
                info,
            });
        }
        Ok(drivers)
    }

    /// Full path of the INF behind one entry of a driver list.
    fn driver_inf_path(
        &self,
        data: &SP_DEVINFO_DATA,
        info: &SP_DRVINFO_DATA_V2_W,
    ) -> Option<PathBuf> {
        // First call: ask how big the (variable-length) detail struct is.
        let mut required = 0u32;
        unsafe { SetupDiGetDriverInfoDetailW(self.0, data, info, null_mut(), 0, &mut required) };
        let bytes = (required as usize).max(size_of::<SP_DRVINFO_DETAIL_DATA_W>());

        // A u64 backing buffer keeps the struct correctly aligned.
        let mut buf = vec![0u64; bytes.div_ceil(8)];
        let detail = buf.as_mut_ptr() as *mut SP_DRVINFO_DETAIL_DATA_W;
        unsafe { (*detail).cbSize = size_of::<SP_DRVINFO_DETAIL_DATA_W>() as u32 };

        let ok = unsafe {
            SetupDiGetDriverInfoDetailW(self.0, data, info, detail, bytes as u32, null_mut())
        };
        if ok == 0 {
            return None;
        }
        let inf_file_name = unsafe { &(*detail).InfFileName };
        Some(PathBuf::from(from_wide(inf_file_name)))
    }

    /// Install a specific driver package on a connected device (Device Manager's
    /// "Update driver > Let me pick"). Needs Administrator.
    pub fn install_driver(
        &self,
        node: &DeviceNode,
        driver: &DriverCandidate,
    ) -> Result<(), DriverError> {
        let mut need_reboot = 0;
        let ok = unsafe {
            DiInstallDevice(
                null_mut(),
                self.0,
                &node.data,
                &driver.info,
                0,
                &mut need_reboot,
            )
        };
        if ok == 0 {
            return Err(last_error("DiInstallDevice"));
        }

        // Then force the device to restart. DiInstallDevice changes which driver is bound, but
        // on a composite device that is not enough on its own: the parent gets the new driver
        // while usbccgp and the per-interface child devnodes stay live on the old one, so
        // WinUSB never takes ownership and libusb cannot get an interface handle (it reports
        // Access, or NotFound from a control transfer). A property-change restart tears the
        // whole stack down and rebuilds it — the same thing Device Manager's disable/enable
        // does. Best-effort: the driver is already installed, and a failure here is worth a
        // warning rather than undoing the swap.
        if let Err(e) = self.restart_device(node) {
            println!("⚠️  Could not restart the device after the driver change: {e}");
        }
        Ok(())
    }

    /// The `DeviceInterfaceGUIDs` registered under the device's Device Parameters key.
    ///
    /// This is the thing libusb needs in order to open a WinUSB device at all: libwdi's INF
    /// writes it, and without it WinUSB can be bound and still be unusable — exactly the
    /// "driver says WinUSB but libusb is refused" shape. Empty means it isn't registered.
    pub fn device_interface_guids(&self, node: &DeviceNode) -> Vec<String> {
        const KEY_READ: u32 = 0x2_0019;
        let key = unsafe {
            SetupDiOpenDevRegKey(self.0, &node.data, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ)
        };
        if key.is_null() || key as isize == -1 {
            return Vec::new();
        }

        let name = to_wide("DeviceInterfaceGUIDs");
        let mut buf = [0u16; 512];
        let mut bytes = size_of_val(&buf) as u32;
        let status = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                null(),
                null_mut(),
                buf.as_mut_ptr() as *mut u8,
                &mut bytes,
            )
        };
        unsafe { RegCloseKey(key) };
        if status != 0 {
            return Vec::new();
        }

        // REG_MULTI_SZ: NUL-separated strings, terminated by an empty one.
        let used = (bytes as usize / 2).min(buf.len());
        buf[..used]
            .split(|&c| c == 0)
            .filter(|part| !part.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    /// Force a specific INF onto every device with `hardware_id`, overriding Windows' own
    /// ranking, then restart the device. Needs Administrator.
    ///
    /// This is what Zadig/libwdi does, and it succeeds where `install_driver` quietly doesn't.
    /// `DiInstallDevice` selects a driver node and lets Windows decide; when two packages tie
    /// on rank, Windows keeps the one it prefers (the newer vendor driver) loaded even though
    /// it reports the install as successful. `INSTALLFLAG_FORCE` removes that choice.
    ///
    /// Note the scope difference: this matches on hardware ID, so it applies to every device
    /// with that VID:PID rather than one instance. For a phone that is the same thing.
    /// Returns whether Windows says the change only takes effect after a restart — which means
    /// it has *not* taken effect now. Ignoring this is what let "switched to WinUSB" be printed
    /// over a device that was still running the vendor driver.
    pub fn force_install(hardware_id: &str, inf_path: &Path) -> Result<bool, DriverError> {
        let id = to_wide(hardware_id);
        let inf = to_wide(&inf_path.to_string_lossy());
        let mut need_reboot = 0;

        let ok = unsafe {
            UpdateDriverForPlugAndPlayDevicesW(
                null_mut(),
                id.as_ptr(),
                inf.as_ptr(),
                INSTALLFLAG_FORCE | INSTALLFLAG_NONINTERACTIVE,
                &mut need_reboot,
            )
        };
        if ok == 0 {
            return Err(last_error("UpdateDriverForPlugAndPlayDevicesW"));
        }
        Ok(need_reboot != 0)
    }

    /// Tear down and rebuild the device's driver stack (DIF_PROPERTYCHANGE / DICS_PROPCHANGE).
    pub fn restart_device(&self, node: &DeviceNode) -> Result<(), DriverError> {
        let mut params: SP_PROPCHANGE_PARAMS = unsafe { zeroed() };
        params.ClassInstallHeader.cbSize = size_of::<SP_CLASSINSTALL_HEADER>() as u32;
        params.ClassInstallHeader.InstallFunction = DIF_PROPERTYCHANGE;
        params.StateChange = DICS_PROPCHANGE;
        params.Scope = DICS_FLAG_GLOBAL;
        params.HwProfile = 0;

        // The header is the first field, so a pointer to the struct is a pointer to it; the
        // size tells SetupAPI how much of the larger struct to read.
        let header = (&raw const params).cast::<SP_CLASSINSTALL_HEADER>();
        let ok = unsafe {
            SetupDiSetClassInstallParamsW(
                self.0,
                &node.data,
                header,
                size_of::<SP_PROPCHANGE_PARAMS>() as u32,
            )
        };
        if ok == 0 {
            return Err(last_error("SetupDiSetClassInstallParamsW"));
        }

        let ok = unsafe { SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, self.0, &node.data) };
        if ok == 0 {
            return Err(last_error("SetupDiCallClassInstaller(DIF_PROPERTYCHANGE)"));
        }
        Ok(())
    }

    /// Delete the devnode (not the driver package). Windows forgets which driver the device
    /// had, so it picks the best-ranked one next time the device appears. Needs Administrator.
    pub fn remove_devnode(&self, node: &DeviceNode) -> Result<(), DriverError> {
        let mut need_reboot = 0;
        let ok = unsafe { DiUninstallDevice(null_mut(), self.0, &node.data, 0, &mut need_reboot) };
        if ok == 0 {
            return Err(last_error("DiUninstallDevice"));
        }
        Ok(())
    }
}

/// Tear the device and its children out of the device tree, then bring it back.
///
/// This is what finally makes a driver change real on a composite device. Installing a driver
/// only records the choice; Windows applies it when the device next starts. If the device is
/// in use — and a phone's MTP, modem and ADB children generally are — it never stops, so the
/// old stack keeps running and the new driver is left pending a reboot. Removing the subtree
/// (what "Safely remove hardware" does) and re-enumerating the parent forces the restart
/// without unplugging anything.
///
/// If something refuses to let go, Windows names the culprit, which is reported back.
pub fn remove_and_reenumerate(instance_id: &str) -> Result<(), DriverError> {
    let id = to_wide(instance_id);
    let mut devinst = 0u32;
    if unsafe { CM_Locate_DevNodeW(&mut devinst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return Err(DriverError::DeviceNotFound(instance_id.to_string()));
    }

    // Remember the parent (the USB hub) before the node goes away; re-enumerating just that
    // branch brings the device back without rescanning the whole machine.
    let mut parent = 0u32;
    let have_parent = unsafe { CM_Get_Parent(&mut parent, devinst, 0) } == CR_SUCCESS;

    let mut veto_type: PNP_VETO_TYPE = PNP_VetoTypeUnknown;
    let mut veto_name = [0u16; 260];
    let removed = unsafe {
        CM_Query_And_Remove_SubTreeW(
            devinst,
            &mut veto_type,
            veto_name.as_mut_ptr(),
            veto_name.len() as u32,
            CM_REMOVE_UI_NOT_OK,
        )
    };
    if removed != CR_SUCCESS {
        return Err(DriverError::RemovalRefused {
            instance_id: instance_id.to_string(),
            code: removed,
            vetoed_by: from_wide(&veto_name),
            veto_reason: describe_veto(veto_type),
        });
    }

    // Bring it back. Without this the phone stays missing until it is physically replugged.
    let target = if have_parent {
        parent
    } else {
        let mut root = 0u32;
        if unsafe { CM_Locate_DevNodeW(&mut root, null(), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS
        {
            return Err(DriverError::DeviceNotFound("the device tree root".into()));
        }
        root
    };
    unsafe { CM_Reenumerate_DevNode(target, CM_REENUMERATE_SYNCHRONOUS) };
    Ok(())
}

/// Plain-English version of a PNP veto type.
///
/// Windows names the *device* that refused, never the process holding it — finding that would
/// take undocumented handle enumeration. The veto type is the next best thing: it says whether
/// an application has the device open, a service holds it, or the device simply can't be
/// stopped, which is usually enough to know what to close.
fn describe_veto(veto_type: PNP_VETO_TYPE) -> &'static str {
    // Compared rather than matched: these constants aren't SCREAMING_CASE, so as match patterns
    // they look like bindings, and one that fell out of scope would silently become a catch-all
    // arm that swallows every veto type.
    if veto_type == PNP_VetoOutstandingOpen {
        "a program still has it open"
    } else if veto_type == PNP_VetoWindowsApp {
        "a running application is using it"
    } else if veto_type == PNP_VetoWindowsService {
        "a Windows service is using it"
    } else if veto_type == PNP_VetoPendingClose {
        "it is in the middle of closing"
    } else if veto_type == PNP_VetoDevice || veto_type == PNP_VetoLegacyDevice {
        "the device itself refused"
    } else if veto_type == PNP_VetoDriver || veto_type == PNP_VetoLegacyDriver {
        "its driver refused"
    } else if veto_type == PNP_VetoNonDisableable {
        "it is marked as not disableable"
    } else if veto_type == PNP_VetoInsufficientRights {
        "this process lacks the rights"
    } else if veto_type == PNP_VetoAlreadyRemoved {
        "it is already gone"
    } else {
        "no reason given"
    }
}

/// The instance IDs of a devnode's children.
///
/// A composite device normally has one child per USB interface, each with its own driver. If
/// those are still present after WinUSB is bound to the parent, the old composite stack never
/// went away — which is one reason libusb can be refused a device that looks correctly bound.
pub fn children(instance_id: &str) -> Vec<String> {
    let id = to_wide(instance_id);
    let mut parent = 0u32;
    if unsafe { CM_Locate_DevNodeW(&mut parent, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return Vec::new();
    }

    let mut child = 0u32;
    if unsafe { CM_Get_Child(&mut child, parent, 0) } != CR_SUCCESS {
        return Vec::new();
    }

    let mut ids = Vec::new();
    loop {
        let mut buf = [0u16; 512];
        if unsafe { CM_Get_Device_IDW(child, buf.as_mut_ptr(), buf.len() as u32, 0) } == CR_SUCCESS
        {
            ids.push(from_wide(&buf));
        }
        let mut sibling = 0u32;
        if unsafe { CM_Get_Sibling(&mut sibling, child, 0) } != CR_SUCCESS {
            break;
        }
        child = sibling;
    }
    ids
}

/// Is the devnode plugged in right now?
pub fn presence(instance_id: &str) -> Presence {
    let id = to_wide(instance_id);
    let mut devinst = 0u32;
    // NORMAL only finds devnodes that are currently connected.
    let result = unsafe { CM_Locate_DevNodeW(&mut devinst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) };
    if result == CR_SUCCESS {
        Presence::Present
    } else {
        Presence::Disconnected
    }
}

pub fn uninstall_driver_package(inf_name: &str) -> Result<(), DriverError> {
    let inf_w = to_wide(inf_name);
    // 1 = SUOI_FORCEDELETE
    let ok = unsafe { SetupUninstallOEMInfW(inf_w.as_ptr(), 1, null_mut()) };
    if ok == 0 {
        return Err(last_error("SetupUninstallOEMInfW"));
    }
    Ok(())
}

fn inf_installs_winusb(inf_path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(inf_path) else {
        return false;
    };

    // INF files are either UTF-16LE (with a BOM) or plain ASCII/ANSI.
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };

    text.lines().any(|line| {
        let code = line.split(';').next().unwrap_or("");
        let compact: String = code
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        compact.starts_with("addservice=winusb,")
    })
}
#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupUninstallOEMInfW(InfFileName: *const u16, Flags: u32, Reserved: *mut c_void) -> i32;
}
