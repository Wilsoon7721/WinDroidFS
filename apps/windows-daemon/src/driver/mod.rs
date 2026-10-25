mod package;
mod setupapi;

use std::fmt;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

use journal::JournalEntry;
use setupapi::{DevInfoSet, DeviceNode, DriverCandidate, Presence};

// Errors

#[derive(Debug)]
pub enum DriverError {
    Win32 {
        call: &'static str,
        code: u32,
    },
    NotElevated,
    DeviceNotFound(String),
    NoWinUsbPackage(String),
    /// The install reported success but the device is still running a different driver
    SwapDidNotTake {
        instance_id: String,
        service: Option<String>,
        children: usize,
    },
    /// Windows wouldn't let the device be removed, so the new driver can't be loaded.
    RemovalRefused {
        instance_id: String,
        code: u32,
        vetoed_by: String,
        veto_reason: &'static str,
    },
    Journal(std::io::Error),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Win32 { call, code } => {
                write!(f, "{call} failed (Win32 error {code} / 0x{code:08X})")
            }
            DriverError::NotElevated => {
                write!(f, "switching drivers requires running as Administrator")
            }
            DriverError::DeviceNotFound(id) => write!(f, "device {id} is not known to Windows"),
            DriverError::NoWinUsbPackage(id) => write!(
                f,
                "no WinUSB driver package in the driver store for {id}; run Zadig once for this device"
            ),
            DriverError::SwapDidNotTake {
                instance_id,
                service,
                children,
            } => {
                let running = service.as_deref().unwrap_or("an unknown driver");
                write!(
                    f,
                    "the driver change for {instance_id} did not take effect: the device reports \
                     \"{running}\" and still has {children} child interface devnode(s), so the \
                     original composite driver stack is still the one running"
                )
            }
            DriverError::RemovalRefused {
                instance_id,
                code,
                vetoed_by,
                veto_reason,
            } => {
                let who = if vetoed_by.is_empty() {
                    "something".to_string()
                } else {
                    format!("\"{vetoed_by}\"")
                };
                write!(
                    f,
                    "Windows refused to restart {instance_id} (CONFIGRET {code}): {who} blocked \
                     it because {veto_reason}. Free it, or unplug and replug the phone"
                )
            }
            DriverError::Journal(e) => {
                write!(f, "could not read/write the driver swap journal: {e}")
            }
        }
    }
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Journal(e)
    }
}

#[derive(Debug)]
pub enum RollbackOutcome {
    /// Device was still connected; its original driver was reinstalled.
    Reinstalled { driver: String },
    /// Device was disconnected; its devnode was deleted so Windows re-picks next time.
    DevnodeRemoved,
    /// Nothing was on WinUSB anymore.
    NothingToDo,
}

impl fmt::Display for RollbackOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RollbackOutcome::Reinstalled { driver } => write!(f, "reinstalled \"{driver}\""),
            RollbackOutcome::DevnodeRemoved => {
                write!(
                    f,
                    "devnode removed, Windows will pick the original driver on next plug-in"
                )
            }
            RollbackOutcome::NothingToDo => write!(f, "already on its original driver"),
        }
    }
}

/// The swap currently in progress, shared with the Ctrl+C handler.
/// The lock is held for the whole rollback, so the handler can't exit halfway through one.
static ACTIVE_SWAP: Mutex<Option<JournalEntry>> = Mutex::new(None);

/// A temporary WinUSB binding. Call [`WinUsbSwap::rollback`] when done. If it's dropped without that, the rollback still runs from `Drop`.
pub struct WinUsbSwap {
    instance_id: String,
    /// Human-readable summary of what `begin` did; see [`WinUsbSwap::detail`].
    detail: String,
}

impl WinUsbSwap {
    /// Record what to restore, then switch `instance_id` to WinUSB.
    /// If it's already on WinUSB (e.g. Zadig was used by hand), this only records the rollback.
    pub fn begin(instance_id: &str) -> Result<Self, DriverError> {
        require_elevation()?;

        let (set, node) = DevInfoSet::open(instance_id)?;
        let drivers = set.compatible_drivers(&node)?;
        let already_winusb = is_on_zadig_winusb(&set, &node, &drivers);

        // What to put back afterwards. If someone already swapped it by hand we don't know the
        // original, so use the best-ranked non-WinUSB driver Windows has for it (Samsung's).
        let rollback_inf = if already_winusb {
            best_non_winusb(&drivers).map(|d| d.inf_name.clone())
        } else {
            set.current_inf(&node)
        };

        let entry = JournalEntry {
            instance_id: instance_id.to_string(),
            rollback_inf,
        };

        // Journal first, driver second. If we crash after this line, the next run cleans up.
        journal::write(&entry)?;
        *lock_active_swap() = Some(entry);
        let mut swap = WinUsbSwap {
            instance_id: instance_id.to_string(),
            detail: String::new(),
        };

        // Say which package is involved and whether anything actually changed. The two cases
        // look identical from outside but aren't: when the device is already on WinUSB we
        // install nothing and only record the rollback.
        swap.detail = if already_winusb {
            let inf = set
                .current_inf(&node)
                .unwrap_or_else(|| "unknown package".to_string());
            println!("🔧 {instance_id} is already on WinUSB ({inf}); no driver change needed.");
            format!("already on WinUSB ({inf}), nothing to switch")
        } else {
            let mut current_drivers = drivers;

            // If no package exists, generate one now.
            if pick_winusb(&current_drivers).is_none() {
                let (vid, pid) = parse_parent_instance_id(instance_id)
                    .ok_or_else(|| DriverError::DeviceNotFound(instance_id.to_string()))?;
                println!(
                    "📦 No WinUSB package in driver store for {vid:04X}:{pid:04X}; generating and self-signing one..."
                );
                let oem_inf =
                    package::ensure_winusb_package(vid, pid, "WinDroidFS Android Device")?;
                println!("   ...staged {oem_inf} into the Windows Driver Store.");

                // Drop what's cached and re-open the device so Windows discovers the newly staged INF.
                drop(node);
                drop(set);
                let (new_set, new_node) = DevInfoSet::open(instance_id)?;
                current_drivers = new_set.compatible_drivers(&new_node)?;

                #[allow(unused_variables)]
                let set = new_set;
                #[allow(unused_variables)]
                let node = new_node;
            }

            let winusb = pick_winusb(&current_drivers)
                .ok_or_else(|| DriverError::NoWinUsbPackage(instance_id.to_string()))?;
            println!(
                "🔧 Switching {instance_id} to WinUSB ({}) for the handshake...",
                winusb.inf_name
            );

            // Force the package on by hardware ID
            let hardware_id = hardware_id_of(instance_id)
                .ok_or_else(|| DriverError::DeviceNotFound(instance_id.to_string()))?;
            let pending_restart = DevInfoSet::force_install(&hardware_id, &winusb.inf_path)?;
            if pending_restart {
                println!("   install recorded, but Windows says it needs the device to restart");
            }

            // Recording the driver is not applying it. Windows only switches a device over when
            // it next starts, and a phone whose MTP, modem and ADB children are in use never
            // stops — so the old stack keeps running while the registry claims otherwise. Pull
            // the device out of the tree and re-enumerate it to force the restart for real.
            //
            // A veto is not fatal: the driver is installed either way, and unplugging the phone
            // applies it. That is exactly what Zadig relies on — it never forces a removal, so
            // it works straight away when nothing holds the device and needs a replug when
            // something does. So say who is holding it and wait, rather than giving up.
            let blocked = match setupapi::remove_and_reenumerate(instance_id) {
                Ok(()) => false,
                Err(DriverError::RemovalRefused {
                    vetoed_by,
                    veto_reason,
                    ..
                }) => {
                    let holder = if vetoed_by.is_empty() {
                        "something unnamed".to_string()
                    } else {
                        vetoed_by.clone()
                    };
                    println!("   ⚠️  Windows won't restart the device yet: {veto_reason}.");
                    println!("      Blocked by: {holder}");
                    for line in likely_holders(&vetoed_by) {
                        println!("      {line}");
                    }
                    println!(
                        "      WinUSB is installed either way; waiting up to 60s to apply it."
                    );
                    true
                }
                Err(e) => return Err(e),
            };

            // Now check what is actually running. Two signals, because the obvious one lies:
            // the recorded service name flips to WinUSB as soon as the driver is *chosen*,
            // while the composite children only disappear once it is genuinely *loaded*.
            // WinUSB owns the whole device and creates no children, so any child left behind
            // means the old composite stack is still the live one.
            let bound = if blocked {
                // Keep retrying the removal while waiting. Releasing a handle does not restart
                // the device by itself, so without this, freeing the holder would achieve
                // nothing and the wait would expire with the obstacle already gone. Retrying
                // means the swap finishes by itself the moment adb (or whatever) lets go, and
                // a replug still works too, since that restarts the device directly.
                retry_removal_until_winusb(instance_id, Duration::from_secs(60))
            } else {
                wait_for_winusb(instance_id, Duration::from_secs(15))
            };
            let children = setupapi::children(instance_id);
            let on_winusb = bound
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(WINUSB_SERVICE));
            if !on_winusb || !children.is_empty() {
                return Err(DriverError::SwapDidNotTake {
                    instance_id: instance_id.to_string(),
                    service: bound,
                    children: children.len(),
                });
            }

            println!("   ...confirmed: WinUSB is loaded and the composite stack is gone.");
            if let Ok((set_now, node_now)) = DevInfoSet::open(instance_id) {
                let guids = set_now.device_interface_guids(&node_now);
                if guids.is_empty() {
                    println!(
                        "   ⚠️  no DeviceInterfaceGUIDs registered — libusb may still be refused"
                    );
                } else {
                    println!("   interface GUIDs: {}", guids.join(", "));
                }
            }
            format!("switched to WinUSB ({})", winusb.inf_name)
        };

        Ok(swap)
    }

    /// What `begin` actually did, for logging: which package, and whether it changed anything.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Put the original driver back and delete the journal.
    pub fn rollback(self) -> Result<RollbackOutcome, DriverError> {
        // `Drop` runs right after this and finds nothing left to do.
        finish_active_swap()
    }
}

impl Drop for WinUsbSwap {
    fn drop(&mut self) {
        match finish_active_swap() {
            Ok(RollbackOutcome::NothingToDo) => {}
            Ok(outcome) => println!("↩️  Restored driver for {}: {outcome}", self.instance_id),
            Err(e) => println!(
                "⚠️  Could not restore the driver for {}: {e}\n   -> Run `windows-daemon restore-drivers` as Administrator.",
                self.instance_id
            ),
        }
    }
}

fn lock_active_swap() -> std::sync::MutexGuard<'static, Option<JournalEntry>> {
    // A panic while holding the lock shouldn't stop us from rolling back.
    ACTIVE_SWAP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Roll back the active swap (if any). Shared by `rollback`, `Drop` and the Ctrl+C handler.
fn finish_active_swap() -> Result<RollbackOutcome, DriverError> {
    let mut active = lock_active_swap();
    let Some(entry) = active.as_ref() else {
        return Ok(RollbackOutcome::NothingToDo);
    };
    let outcome = restore(entry)?;
    journal::remove()?;
    *active = None;
    Ok(outcome)
}

// ===========================================================================
// Rollback logic
// ===========================================================================

fn restore(entry: &JournalEntry) -> Result<RollbackOutcome, DriverError> {
    let (set, node) = match DevInfoSet::open(&entry.instance_id) {
        Ok(found) => found,
        // Devnode already gone (e.g. removed by hand): nothing left to fix.
        Err(DriverError::DeviceNotFound(_)) => return Ok(RollbackOutcome::NothingToDo),
        Err(e) => return Err(e),
    };

    let drivers = set.compatible_drivers(&node)?;
    if !is_on_zadig_winusb(&set, &node, &drivers) {
        return Ok(RollbackOutcome::NothingToDo);
    }

    match setupapi::presence(&entry.instance_id) {
        // Uninstall driver and remove devnode after disconnect
        Presence::Disconnected => {
            if let Some(winusb) = pick_winusb(&drivers) {
                let _ = setupapi::uninstall_driver_package(&winusb.inf_name);
            }

            set.remove_devnode(&node)?;
            Ok(RollbackOutcome::DevnodeRemoved)
        }

        // Handshake failed, or recovering while the phone is plugged in: reinstall directly.
        Presence::Present => {
            let recorded = entry.rollback_inf.as_deref().and_then(|inf| {
                drivers
                    .iter()
                    .find(|d| d.inf_name.eq_ignore_ascii_case(inf) && !d.is_zadig_winusb)
            });

            match recorded.or_else(|| best_non_winusb(&drivers)) {
                Some(original) => {
                    // Force it, for the same reason the swap does, and because an unforced
                    // rollback that silently doesn't take would leave the phone on WinUSB with
                    // MTP broken. Fall back to the gentler call if forcing isn't possible.
                    let forced = hardware_id_of(&entry.instance_id).map(|hardware_id| {
                        DevInfoSet::force_install(&hardware_id, &original.inf_path)
                    });
                    match forced {
                        Some(Ok(_)) => {}
                        _ => set.install_driver(&node, original)?,
                    }
                    // Same reason as the swap: recording the driver is not applying it. If the
                    // device won't come out of the tree, fall back to the gentler restart
                    // rather than failing — the vendor driver is Windows' own preference, so
                    // it comes back on the next replug regardless.
                    if setupapi::remove_and_reenumerate(&entry.instance_id).is_err() {
                        let _ = set.restart_device(&node);
                    }
                    Ok(RollbackOutcome::Reinstalled {
                        driver: format!(
                            "{} ({}, {})",
                            original.description, original.provider, original.inf_name
                        ),
                    })
                }
                // No non-WinUSB driver is installed for it at all. Removing the devnode is the
                // best we can do; Windows will fall back to its generic driver on next plug-in.
                None => {
                    set.remove_devnode(&node)?;
                    Ok(RollbackOutcome::DevnodeRemoved)
                }
            }
        }
    }
}

/// The driver-list entry the device is installed with right now (matched by INF name).
fn current_driver<'a>(
    set: &DevInfoSet,
    node: &DeviceNode,
    drivers: &'a [DriverCandidate],
) -> Option<&'a DriverCandidate> {
    let inf = set.current_inf(node)?;
    drivers
        .iter()
        .find(|d| d.inf_name.eq_ignore_ascii_case(&inf))
}

/// The kernel service name WinUSB devices are bound to.
const WINUSB_SERVICE: &str = "WinUSB";

/// `USB\VID_04E8&PID_6860\R5CY204BABT` -> `USB\VID_04E8&PID_6860`, the hardware ID an INF
/// matches on.
fn hardware_id_of(instance_id: &str) -> Option<String> {
    let (vid, pid) = parse_parent_instance_id(instance_id)?;
    Some(format!("USB\\VID_{vid:04X}&PID_{pid:04X}"))
}

/// Wait for the device to come back on WinUSB with its composite children gone.
///
/// Both conditions matter. The recorded service name flips to WinUSB the moment Windows
/// *chooses* the driver, which is why it alone could not be trusted; the children only vanish
/// once WinUSB is actually *loaded*, because WinUSB owns the whole device and creates none.
/// Returns the last service name seen, so a failure can report what is really running.
///
/// The device is briefly absent after being pulled out of the tree; that just makes the open
/// fail, and we poll again.
/// Guess what is holding a vetoing interface, from the interface's name.
///
/// This is a hint, not a finding. Windows names the *device* that refused and a broad reason
/// category; it never says which process holds a handle, and getting that would take
/// undocumented handle enumeration. All we can do is read the interface suffix — `&ADB`,
/// `&MTP`, `&MODEM` — and name the software that usually owns that kind of interface. Hence
/// "usually" rather than a claim. Every suggestion here is cheap and safe to try, and the
/// replug fallback works regardless of which one is right.
fn likely_holders(vetoed_by: &str) -> Vec<&'static str> {
    let id = vetoed_by.to_ascii_uppercase();
    let mut hints = Vec::new();

    if id.contains("&ADB") {
        hints.push("That's the ADB interface, usually held by a running adb server.");
        hints.push("Try `adb kill-server` (Android Studio and scrcpy start one too).");
    }
    if id.contains("MTP") || id.contains("&WPD") {
        hints.push("That's the file-transfer interface. Close any Explorer window on the");
        hints.push("phone, plus Phone Link, Photos import and Samsung Smart Switch.");
    }
    if id.contains("&MODEM") || id.contains("&DIAG") {
        hints.push("That's the modem interface, usually held by a dial-up or telephony service.");
    }
    if id.contains("&MIDI") || id.contains("&AUDIO") {
        hints.push("That's an audio interface, usually held by the Windows audio service.");
    }

    if hints.is_empty() {
        hints.push("Close anything using the phone: Explorer windows, Phone Link, Samsung");
        hints.push("software, adb, or any backup or photo-import tool.");
    }
    hints.push("Unplugging and replugging the phone always works as a fallback.");
    hints
}

/// Keep asking Windows to restart the device until it agrees and WinUSB loads.
///
/// Used after a veto. Each pass retries the removal, so whichever happens first — the user
/// freeing the holder, or unplugging the phone — finishes the swap without further input.
fn retry_removal_until_winusb(instance_id: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        if let Ok((set, node)) = DevInfoSet::open(instance_id) {
            last = set.service(&node);
            let loaded = last
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(WINUSB_SERVICE));
            if loaded && setupapi::children(instance_id).is_empty() {
                return last;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        // Ignore the outcome: a veto here just means "still blocked, try again shortly", and
        // once the device is already restarting this call fails harmlessly.
        let _ = setupapi::remove_and_reenumerate(instance_id);
        sleep(Duration::from_secs(1));
    }
}

fn wait_for_winusb(instance_id: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        // Re-open each time: the devnode is destroyed and rebuilt, so an older handle would
        // keep answering with stale values.
        if let Ok((set, node)) = DevInfoSet::open(instance_id) {
            last = set.service(&node);
            let loaded = last
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(WINUSB_SERVICE));
            if loaded && setupapi::children(instance_id).is_empty() {
                return last;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        sleep(Duration::from_millis(250));
    }
}

/// Is the device on a Zadig/libwdi WinUSB package (the kind libusb can actually open)?
fn is_on_zadig_winusb(set: &DevInfoSet, node: &DeviceNode, drivers: &[DriverCandidate]) -> bool {
    current_driver(set, node, drivers).is_some_and(|d| d.is_zadig_winusb)
}

/// Windows' own pick among the rest: lowest rank, then newest driver date.
fn best_non_winusb(drivers: &[DriverCandidate]) -> Option<&DriverCandidate> {
    drivers
        .iter()
        .filter(|d| !d.is_zadig_winusb)
        .min_by_key(|d| (d.rank, std::cmp::Reverse(d.date)))
}

fn pick_winusb(drivers: &[DriverCandidate]) -> Option<&DriverCandidate> {
    drivers
        .iter()
        .filter(|d| d.is_zadig_winusb)
        .min_by_key(|d| (d.rank, std::cmp::Reverse(d.date)))
}

// ===========================================================================
// Public helpers used by main.rs
// ===========================================================================

pub fn is_elevated() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

fn require_elevation() -> Result<(), DriverError> {
    if is_elevated() {
        Ok(())
    } else {
        Err(DriverError::NotElevated)
    }
}

/// Finish a rollback a previous run didn't get to (crash, killed process, power loss...).
/// Returns the device it fixed, if there was one.
pub fn recover_from_journal() -> Result<Option<(String, RollbackOutcome)>, DriverError> {
    let Some(entry) = journal::read()? else {
        return Ok(None);
    };
    require_elevation()?;
    let outcome = restore(&entry)?;
    journal::remove()?;
    Ok(Some((entry.instance_id, outcome)))
}

/// Manual "put everything back": every normal-mode Android devnode (connected or not) that is
/// on WinUSB goes back to its best non-WinUSB driver. Accessory identities are left alone.
pub fn restore_android_drivers(
    android_vids: &[u16],
    is_accessory: impl Fn(u16, u16) -> bool,
) -> Result<Vec<(String, RollbackOutcome)>, DriverError> {
    require_elevation()?;
    let mut restored = Vec::new();

    let set = DevInfoSet::all_usb()?;
    for node in set.devices() {
        let Some((vid, pid)) = parse_parent_instance_id(&node.instance_id) else {
            continue;
        };
        if !android_vids.contains(&vid) || is_accessory(vid, pid) {
            continue;
        }
        let drivers = set.compatible_drivers(&node)?;
        if !is_on_zadig_winusb(&set, &node, &drivers) {
            continue;
        }
        let entry = JournalEntry {
            instance_id: node.instance_id.clone(),
            rollback_inf: None,
        };
        restored.push((node.instance_id.clone(), restore(&entry)?));
    }

    journal::remove()?;
    Ok(restored)
}

/// Make sure a device is bound to WinUSB permanently (no journal, no rollback).
pub fn ensure_winusb(instance_id: &str) -> Result<bool, DriverError> {
    let (mut set, mut node) = DevInfoSet::open(instance_id)?;
    let mut drivers = set.compatible_drivers(&node)?;
    if is_on_zadig_winusb(&set, &node, &drivers) && setupapi::children(instance_id).is_empty() {
        return Ok(false);
    }
    require_elevation()?;

    let (vid, pid) = parse_parent_instance_id(instance_id)
        .ok_or_else(|| DriverError::DeviceNotFound(instance_id.to_string()))?;

    if pick_winusb(&drivers).is_none() {
        println!(
            "📦 No WinUSB package in driver store for accessory {vid:04X}:{pid:04X}; generating and self-signing one..."
        );
        let oem_inf = package::ensure_winusb_package(vid, pid, "WinDroidFS Android Accessory")?;
        println!("   ...staged {oem_inf} into the Windows Driver Store.");

        // drop to invalidate cached drivers and discover the new INF
        drop(node);
        drop(set);
        let (new_set, new_node) = DevInfoSet::open(instance_id)?;
        set = new_set;
        node = new_node;
        drivers = set.compatible_drivers(&node)?;
    }

    let winusb = pick_winusb(&drivers)
        .ok_or_else(|| DriverError::NoWinUsbPackage(instance_id.to_string()))?;

    println!(
        "🔧 Binding {instance_id} to WinUSB ({})...",
        winusb.inf_name
    );
    let hardware_id = hardware_id_of(instance_id)
        .ok_or_else(|| DriverError::DeviceNotFound(instance_id.to_string()))?;
    let _ = DevInfoSet::force_install(&hardware_id, &winusb.inf_path)?;
    if setupapi::remove_and_reenumerate(instance_id).is_err() {
        let _ = set.restart_device(&node);
    }
    let _ = wait_for_winusb(instance_id, Duration::from_secs(15));
    Ok(true)
}

/// The instance ID of the connected device with this VID:PID (the device itself, not one of
/// its interfaces), e.g. `USB\VID_04E8&PID_6860\R5CY204BABT`.
pub fn find_connected_device(vid: u16, pid: u16) -> Option<String> {
    let set = DevInfoSet::all_usb().ok()?;
    set.devices()
        .into_iter()
        .find(|node| {
            parse_parent_instance_id(&node.instance_id) == Some((vid, pid))
                && matches!(setupapi::presence(&node.instance_id), Presence::Present)
        })
        .map(|node| node.instance_id)
}

/// Poll until one of `ids` shows up as a connected device, or `timeout` passes.
pub fn wait_for_device(ids: &[(u16, u16)], timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(id) = ids
            .iter()
            .find_map(|&(vid, pid)| find_connected_device(vid, pid))
        {
            return Some(id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_millis(250));
    }
}

/// Poll until the devnode is no longer connected. Returns `false` on timeout.
pub fn wait_until_disconnected(instance_id: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if matches!(setupapi::presence(instance_id), Presence::Disconnected) {
            return true;
        }
        sleep(Duration::from_millis(250));
    }
    false
}

/// `USB\VID_04E8&PID_6860\R5CY204BABT` -> `Some((0x04E8, 0x6860))`.
/// Interface/child devnodes (`USB\VID_04E8&PID_6860&MI_00\...`, `...&ADB\...`) -> `None`.
pub fn parse_parent_instance_id(instance_id: &str) -> Option<(u16, u16)> {
    let hardware_part = instance_id.strip_prefix("USB\\")?.split('\\').next()?;
    let (vid_part, pid_part) = hardware_part.split_once('&')?;
    let vid = u16::from_str_radix(vid_part.strip_prefix("VID_")?, 16).ok()?;
    // Anything after the PID (another '&...') means it's a child devnode.
    let pid = u16::from_str_radix(pid_part.strip_prefix("PID_")?, 16).ok()?;
    Some((vid, pid))
}

// ===========================================================================
// Ctrl+C / console close
// ===========================================================================

/// If the user hits Ctrl+C (or closes the console) mid-swap, roll back before exiting.
/// Outside a swap, the default behaviour (exit immediately) is kept.
pub fn install_ctrl_guard() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) };
}

unsafe extern "system" fn on_console_event(_event: u32) -> windows_sys::core::BOOL {
    if lock_active_swap().is_some() {
        println!("\n⏳ Restoring the original USB driver before exiting...");
        match finish_active_swap() {
            Ok(outcome) => println!("↩️  {outcome}"),
            Err(e) => println!("⚠️  Rollback failed ({e}). It will be retried on the next start."),
        }
    }
    // FALSE = carry on with the default handler, which exits the process.
    0
}

// ===========================================================================
// The swap journal
// ===========================================================================

mod journal {
    //! A tiny file that says "this device is on WinUSB because of us, here's what to put
    //! back". Written *before* the driver is touched and deleted only *after* the rollback
    //! succeeds, so a crash in between is always recoverable.
    //!
    //! Location: `%ProgramData%\WinDroidFS\driver-swap.journal`
    //! Format: plain `key=value` lines, so you can read (or delete) it by hand.

    use std::fs;
    use std::io::{self, Write};
    use std::path::PathBuf;

    #[derive(Debug, Clone)]
    pub struct JournalEntry {
        /// e.g. `USB\VID_04E8&PID_6860\R5CY204BABT`
        pub instance_id: String,
        /// Driver-store INF to reinstall if the device is still connected, e.g. `oem6.inf`.
        /// `None` = no known original; let Windows pick the best-ranked driver.
        pub rollback_inf: Option<String>,
    }

    fn journal_path() -> PathBuf {
        let program_data =
            std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(program_data)
            .join("WinDroidFS")
            .join("driver-swap.journal")
    }

    pub fn write(entry: &JournalEntry) -> io::Result<()> {
        let path = journal_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }

        let mut contents = format!("instance_id={}\n", entry.instance_id);
        if let Some(inf) = &entry.rollback_inf {
            contents.push_str(&format!("rollback_inf={inf}\n"));
        }

        // Write to a temp file, flush it to disk, then rename over the real one, so a
        // crash mid-write can never leave a half-written journal behind.
        let tmp = path.with_extension("tmp");
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(tmp, path)
    }

    pub fn read() -> io::Result<Option<JournalEntry>> {
        let contents = match fs::read_to_string(journal_path()) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };

        let mut instance_id = None;
        let mut rollback_inf = None;
        for line in contents.lines() {
            match line.split_once('=') {
                Some(("instance_id", value)) => instance_id = Some(value.to_string()),
                Some(("rollback_inf", value)) => rollback_inf = Some(value.to_string()),
                _ => {}
            }
        }

        Ok(instance_id.map(|instance_id| JournalEntry {
            instance_id,
            rollback_inf,
        }))
    }

    pub fn remove() -> io::Result<()> {
        match fs::remove_file(journal_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hammer the read-only enumeration path the daemon runs after a rollback, looking for
    /// a crash. Read-only; changes nothing.
    /// Run with: cargo test stress_enumeration -- --ignored --nocapture
    #[test]
    #[ignore]
    fn stress_enumeration() {
        for i in 0..25 {
            let set = DevInfoSet::all_usb().expect("all_usb");
            for node in set.devices() {
                let _ = set.service(&node);
                let _ = set.current_inf(&node);
                let _ = setupapi::presence(&node.instance_id);
                let _ = set.compatible_drivers(&node);
            }
            if i % 5 == 0 {
                println!("iteration {i} ok");
            }
        }
        println!("stress_enumeration survived");
    }

    /// Read-only look at what the swap logic would decide on this machine. Changes nothing.
    /// Run with: cargo test print_driver_plan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_driver_plan() {
        let set = DevInfoSet::all_usb().unwrap();
        for node in set.devices() {
            let Some((vid, pid)) = parse_parent_instance_id(&node.instance_id) else {
                continue;
            };
            if ![0x04e8, 0x18d1].contains(&vid) {
                continue;
            }
            let connected = matches!(setupapi::presence(&node.instance_id), Presence::Present);
            println!(
                "\n{} ({vid:04X}:{pid:04X}) connected={connected} service={:?} inf={:?}",
                node.instance_id,
                set.service(&node),
                set.current_inf(&node)
            );
            let drivers = set.compatible_drivers(&node).unwrap();
            for d in &drivers {
                println!(
                    "    {:<12} rank={:08X} zadig={:<5} {} / {}",
                    d.inf_name, d.rank, d.is_zadig_winusb, d.provider, d.description
                );
            }
            println!(
                "    -> swap to:      {:?}",
                pick_winusb(&drivers).map(|d| &d.inf_name)
            );
            println!(
                "    -> rollback to:  {:?}",
                best_non_winusb(&drivers).map(|d| &d.inf_name)
            );
        }
    }

    #[test]
    fn parses_device_instance_ids() {
        assert_eq!(
            parse_parent_instance_id(r"USB\VID_04E8&PID_6860\R5CY204BABT"),
            Some((0x04E8, 0x6860))
        );
        assert_eq!(
            parse_parent_instance_id(r"USB\VID_18D1&PID_2D01\R5CY204BABT"),
            Some((0x18D1, 0x2D01))
        );
    }

    #[test]
    fn rejects_interface_devnodes() {
        assert_eq!(
            parse_parent_instance_id(r"USB\VID_04E8&PID_6860&MI_00\8&1&0000"),
            None
        );
        assert_eq!(
            parse_parent_instance_id(r"USB\VID_18D1&PID_2D01&ADB\8&248cc597&1&0001"),
            None
        );
        assert_eq!(
            parse_parent_instance_id(r"BTHENUM\{0000110c}\8&5cb3c28"),
            None
        );
    }
}
