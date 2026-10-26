use std::net::SocketAddr;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rusb::{
    Context, Device, DeviceHandle, Direction, Recipient, RequestType, TransferType, UsbContext,
};

use crate::android::{aoa_identities, is_aoa_identity, is_normal_android_identity};
use crate::driver;
use crate::ipc::transport::send;
use crate::ipc::{Request, Response};
use crate::phone::PhoneClient;
use crate::{webdav, winfsp_fs};

pub use crate::windroid::UsbSpeed;
pub use crate::winfsp_fs::DriveChoice;

const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;

/// Where the WebDAV fallback listens. Loopback only: nothing off this machine can reach it.
const WEBDAV_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 8080);
pub const WINFSP_DOWNLOAD: &str = "https://winfsp.dev/rel/";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct Options {
    /// Skip WinFsp and always use the WebDAV mount.
    pub force_webdav: bool,
    /// Which drive letter to mount on.
    pub drive: DriveChoice,
    /// Mount without write access. (The WebDAV mount is always read-only)
    pub read_only: bool,
    /// Show dot-files as hidden. (WinFsp only)
    pub hide_dotfiles: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            force_webdav: false,
            drive: DriveChoice::Auto,
            read_only: false,
            hide_dotfiles: true,
        }
    }
}

/// Milestones of a connection, for whoever is showing progress.
#[derive(Clone, Debug)]
pub enum Event {
    /// Found a phone; switching drivers, handshaking and opening the link.
    Connecting,
    /// The link is open but the phone app isn't answering yet: it needs opening on the phone.
    /// Waits for as long as the phone stays plugged in.
    WaitingForApp,
    /// Mounted and serving.
    Mounted {
        drive: char,
        /// The phone's name as its owner set it.
        device_name: String,
        /// Mounted through the WebDAV fallback rather than WinFsp.
        webdav: bool,
        /// How fast the USB link is - see [`speed_advice`].
        usb_speed: UsbSpeed,
    },
}

/// How a connection ended. Always unmounted by then.
#[derive(Clone, Debug)]
pub enum Ended {
    /// [`Stop::request`] was called (Ctrl+C, the tray's off switch, quitting).
    Stopped,
    /// The phone went away while mounted. Either unplugged or the link was closed on the phone.
    PhoneLost,
    /// Never got as far as mounting. The message says why for the user.
    Failed(String),
}

/// A request to end the connection, shared between whoever wants it ended and [`run`].
#[derive(Clone, Debug, Default)]
pub struct Stop(Arc<AtomicBool>);

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Sleep for `duration`, waking early if a stop is requested. True if requested.
    pub fn sleep(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if self.requested() {
                return true;
            }
            sleep(Duration::from_millis(100).min(deadline - Instant::now()));
        }
        self.requested()
    }
}

/// The link speed in actual names.
pub fn speed_name(speed: UsbSpeed) -> &'static str {
    match speed {
        UsbSpeed::Low => "USB 1.0",
        UsbSpeed::Full => "USB 1.1",
        UsbSpeed::High => "USB 2.0",
        UsbSpeed::Super => "USB 3.0",
        UsbSpeed::SuperPlus => "USB 3.1+",
        UsbSpeed::Unknown => "USB (speed unknown)",
    }
}

/// Speed advice depending on the USB version.
pub fn speed_advice(speed: UsbSpeed) -> Option<String> {
    let max = match speed {
        UsbSpeed::Low => "0.2 MB/s",
        UsbSpeed::Full => "1.5 MB/s",
        UsbSpeed::High => "60 MB/s",
        UsbSpeed::Super | UsbSpeed::SuperPlus | UsbSpeed::Unknown => return None,
    };
    Some(format!(
        "Maxes out at {max}, use a USB 3.0+ port or supported cable to increase this."
    ))
}

/// Whether an Android phone is plugged in, whether in its normal identity or Accessory Mode.
pub fn phone_present() -> bool {
    let Ok(context) = Context::new() else {
        return false;
    };
    let Ok(devices) = context.devices() else {
        return false;
    };
    devices.iter().any(|device| {
        device.device_descriptor().is_ok_and(|desc| {
            let (vid, pid) = (desc.vendor_id(), desc.product_id());
            is_normal_android_identity(vid, pid) || is_aoa_identity(vid, pid)
        })
    })
}

/// Connect to the phone, mount it, and stay mounted until `stop` is requested or the phone goes away.
pub fn run(options: &Options, stop: &Stop, on_event: &dyn Fn(Event)) -> Ended {
    match connect_and_serve(options, stop, on_event) {
        Ok(ended) => ended,
        Err(e) => failed(format!("USB error: {e}")),
    }
}

/// Print why a connection failed, and say so to the caller.
fn failed(why: impl Into<String>) -> Ended {
    let why = why.into();
    log::error!("❌ {why}");
    Ended::Failed(why)
}

fn connect_and_serve(
    options: &Options,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> rusb::Result<Ended> {
    log::info!("Scanning for an Android device...");
    on_event(Event::Connecting);

    let phone = {
        let context = Context::new()?;
        match find_android_device(&context)? {
            Some(device) => {
                let desc = device.device_descriptor()?;
                Some((desc.vendor_id(), desc.product_id()))
            }
            None => None,
        }
    };

    // Push the phone into AOA mode
    match phone {
        Some((vid, pid)) => {
            log::info!("Found Android device. Sending AOA Handshake...");
            handshake_with_driver_swap(vid, pid)?;
        }
        None => log::info!("No Android devices found (or it's already in Accessory Mode)."),
    }

    // Wait for the phone to come back as the accessory, and have the service bind it to WinUSB
    let Some(aoa_instance) = driver::wait_for_device(&aoa_identities(), Duration::from_secs(10))
    else {
        return Ok(failed(
            "The phone didn't switch to Accessory Mode. Was its prompt dismissed?",
        ));
    };
    match send(&Request::EnsureWinusb {
        instance_id: aoa_instance.clone(),
    }) {
        Ok(Response::Ok { detail }) => log::info!("Accessory on WinUSB: {detail}"),
        Ok(Response::Err { message }) => {
            return Ok(failed(format!(
                "Couldn't bind the accessory to WinUSB: {message}"
            )));
        }
        Ok(_) | Err(_) => {
            return Ok(failed(
                "Lost contact with the driver service while binding the accessory.",
            ));
        }
    }

    // Find accessory mode device
    let context = Context::new()?;
    let Some(handle) = wait_for_aoa_device(&context, Duration::from_secs(10))? else {
        return Ok(failed(
            "Couldn't open the phone in Accessory Mode, even after binding it to WinUSB.",
        ));
    };
    log::info!("Accessory Mode device found and opened!");

    let Some((interface_num, bulk_out_ep, bulk_in_ep)) = find_aoa_endpoints(&handle.device())?
    else {
        return Ok(failed(
            "The phone's Accessory Mode device has no usable bulk endpoints.",
        ));
    };
    handle.claim_interface(interface_num)?;
    log::info!("Pipes established! OUT: 0x{bulk_out_ep:02x}, IN: 0x{bulk_in_ep:02x}");

    let phone = PhoneClient::start(handle, interface_num, bulk_out_ep, bulk_in_ep);

    // Prove the link works before mounting anything.
    let device_name = match wait_for_app(&phone, stop, on_event) {
        Some(name) => name,
        None if stop.requested() => return Ok(Ended::Stopped),
        None => return Ok(Ended::PhoneLost),
    };
    log::info!("🏓 Phone responded to Ping! USB bridge is active.");
    let speed = phone.usb_speed();
    log::info!("⚡ Connected over {}.", speed_name(speed));
    if let Some(advice) = speed_advice(speed) {
        log::warn!("🐢 {advice}");
    }

    Ok(serve(phone, &device_name, options, stop, on_event))
}

/// Ping the phone until its app answers, returning the phone's name. `None` if stopped, or if the phone is unplugged first.
fn wait_for_app(phone: &PhoneClient, stop: &Stop, on_event: &dyn Fn(Event)) -> Option<String> {
    let mut asked = false;
    loop {
        match phone.ping() {
            Ok(name) => return Some(name),
            Err(_) if !phone.is_connected() => return None,
            Err(e) => {
                if !asked {
                    asked = true;
                    log::info!("📱 Waiting for the WinDroidFS app on the phone ({e}).");
                    log::info!(
                        "   -> Open it on the phone (and update it if it's an older build)."
                    );
                    on_event(Event::WaitingForApp);
                }
            }
        }
        if stop.sleep(Duration::from_millis(500)) {
            return None;
        }
    }
}

/// Mount the phone and keep it mounted until stopped or the phone goes away. WinFsp when it's available, otherwise the WebDAV fallback.
fn serve(
    phone: PhoneClient,
    device_name: &str,
    options: &Options,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> Ended {
    let mount_options = winfsp_fs::MountOptions {
        label: winfsp_fs::volume_label(device_name),
        drive: options.drive,
        read_only: options.read_only,
        hide_dotfiles: options.hide_dotfiles,
    };
    let label = &mount_options.label;
    if !options.force_webdav {
        match winfsp_fs::mount(phone.clone(), &mount_options) {
            Ok(mount) => {
                if let DriveChoice::Letter(wanted) = options.drive
                    && wanted != mount.drive
                {
                    log::info!(
                        "ℹ️  {wanted}: is in use, so using {}: instead.",
                        mount.drive
                    );
                }
                log::info!("💽 \"{label}\" is mounted at {}:\\.", mount.drive);
                on_event(Event::Mounted {
                    drive: mount.drive,
                    device_name: device_name.to_string(),
                    webdav: false,
                    usb_speed: phone.usb_speed(),
                });
                let ended = wait_while_mounted(&phone, stop);
                log::info!("Unmounting {}:...", mount.drive);
                mount.unmount();
                say_goodbye(&phone, &ended);
                return ended;
            }
            Err(winfsp_fs::MountError::WinFspMissing) => {
                log::info!("ℹ️  WinFsp isn't installed, so using the slower WebDAV mount instead.");
                log::info!("   -> For a proper drive (and files over ~50 MB), install WinFsp:");
                log::info!("      {WINFSP_DOWNLOAD}");
            }
            Err(e) => {
                log::warn!("⚠️  WinFsp is installed but the mount failed ({e}).");
                log::info!("   -> Falling back to the WebDAV mount.");
            }
        }
    }
    mount_webdav(phone, device_name, options.drive, stop, on_event)
}

/// If this side ended the connection, tell the phone, so its app shows it as ended at once.
fn say_goodbye(phone: &PhoneClient, ended: &Ended) {
    if let Ended::Stopped = ended {
        phone.goodbye();
    }
}

/// Block until a stop is requested or the phone goes away, and say which.
fn wait_while_mounted(phone: &PhoneClient, stop: &Stop) -> Ended {
    while phone.is_connected() {
        if stop.sleep(Duration::from_millis(250)) {
            return Ended::Stopped;
        }
    }
    log::info!("📴 Lost the phone (unplugged, or the link was closed on the phone).");
    Ended::PhoneLost
}

/// The fallback mount: a local WebDAV server mapped to a drive letter with `net use`.
fn mount_webdav(
    phone: PhoneClient,
    device_name: &str,
    drive: DriveChoice,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> Ended {
    let letter = match drive {
        DriveChoice::Letter(letter) => letter,
        DriveChoice::Auto => 'Z',
    };
    let target = format!("{letter}:");
    let addr = SocketAddr::from(WEBDAV_ADDR);
    let url = format!("http://{addr}");
    // Kept alive until this function returns: dropping it stops the server.
    let rt = tokio::runtime::Runtime::new().expect("failed to start the async runtime");

    log::info!("🌐 Starting WebDAV server on {url}...");
    rt.spawn(webdav::serve_webdav(phone.clone(), addr));

    let net_use = |args: &[&str]| {
        Command::new("net")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    };

    log::info!("💽 Mounting Android to {target}\\ drive...");
    let _ = net_use(&["use", &target, "/delete"]);
    match net_use(&["use", &target, &url, "/persistent:no"]) {
        Ok(out) if out.status.success() => {
            log::info!("SUCCESS! Your phone is now mounted at {target}\\");
        }
        Ok(out) => {
            return failed(format!(
                "Couldn't mount {target}\\ ({})",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Err(e) => return failed(format!("Couldn't run `net use`: {e}")),
    }
    on_event(Event::Mounted {
        drive: letter,
        device_name: device_name.to_string(),
        webdav: true,
        usb_speed: phone.usb_speed(),
    });

    let ended = wait_while_mounted(&phone, stop);

    log::info!("Unmounting {target} drive...");
    let _ = net_use(&["use", &target, "/delete"]);
    drop(rt);
    say_goodbye(&phone, &ended);
    ended
}

fn handshake_with_driver_swap(vid: u16, pid: u16) -> rusb::Result<()> {
    let Some(instance_id) = driver::find_connected_device(vid, pid) else {
        log::error!("❌ Found {vid:04X}:{pid:04X} over USB, but not in Windows' device list.");
        return Ok(());
    };

    match send(&Request::BeginSwap {
        instance_id: instance_id.clone(),
    }) {
        Ok(Response::Ok { detail }) => log::info!("🔧 {detail}"),
        Ok(Response::Err { message }) => {
            log::error!("❌ Could not switch {instance_id} to WinUSB: {message}");
            return Ok(());
        }
        Ok(_) | Err(_) => {
            log::error!("❌ Lost contact with the driver service before the swap.");
            return Ok(());
        }
    }

    let handshake_sent = handshake_with_retries(vid, pid, Duration::from_secs(20))?;

    if handshake_sent {
        log::info!("Handshake successful. Waiting for phone to reconnect in Accessory Mode...");
        if !driver::wait_until_disconnected(&instance_id, Duration::from_secs(10)) {
            log::warn!("⚠️  The phone kept its normal identity; restoring its driver anyway.");
        }
    }

    match send(&Request::Rollback {
        instance_id: instance_id.clone(),
    }) {
        Ok(Response::Ok { detail }) => {
            log::info!("↩️  Restored original driver for {instance_id}: {detail}")
        }
        Ok(Response::Err { message }) => log::warn!(
            "⚠️  Could not restore the original driver: {message}\n   -> Run `windows-daemon restore-drivers`."
        ),
        Ok(_) | Err(_) => log::warn!(
            "⚠️  Lost contact with the driver service during rollback.\n   -> The service will finish it from its journal, or run `windows-daemon restore-drivers`."
        ),
    }
    Ok(())
}

fn find_android_device(context: &Context) -> rusb::Result<Option<Device<Context>>> {
    let devices = context.devices()?;
    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        if is_normal_android_identity(desc.vendor_id(), desc.product_id()) {
            return Ok(Some(device));
        }
    }
    Ok(None)
}

fn find_device(context: &Context, vid: u16, pid: u16) -> rusb::Result<Option<Device<Context>>> {
    for device in context.devices()?.iter() {
        let desc = device.device_descriptor()?;
        if desc.vendor_id() == vid && desc.product_id() == pid {
            return Ok(Some(device));
        }
    }
    Ok(None)
}

/// Keep attempting the AOA handshake until it takes or `timeout` runs out.
fn handshake_with_retries(vid: u16, pid: u16, timeout: Duration) -> rusb::Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut last_problem;

    loop {
        let attempt = {
            let context = Context::new()?;
            match find_device(&context, vid, pid)? {
                Some(device) => try_aoa_handshake(&device)?,
                None => Attempt::NotReady("device not visible to libusb yet".into()),
            }
        };

        match attempt {
            Attempt::Started => return Ok(true),
            Attempt::NotReady(why) => last_problem = why,
        }

        if Instant::now() >= deadline {
            log::error!("❌ The phone never accepted the AOA handshake: {last_problem}");
            log::info!("   -> Unlock the screen and leave it on, then try again.");
            return Ok(false);
        }
        sleep(Duration::from_millis(500));
    }
}

/// Outcome of 1 handshake attempt.
enum Attempt {
    Started,
    NotReady(String),
}

/// Poll `find_and_open_aoa_device` until it succeeds or `timeout` passes.
fn wait_for_aoa_device(
    context: &Context,
    timeout: Duration,
) -> rusb::Result<Option<DeviceHandle<Context>>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(handle) = find_and_open_aoa_device(context)? {
            return Ok(Some(handle));
        }
        if Instant::now() >= deadline {
            log::error!(
                "❌ Access Denied to Accessory Mode device, even after binding it to WinUSB."
            );
            return Ok(None);
        }
        sleep(Duration::from_millis(250));
    }
}

fn find_and_open_aoa_device(context: &Context) -> rusb::Result<Option<DeviceHandle<Context>>> {
    let devices = context.devices()?;
    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        // 0x18D1 is Google, 0x2D00 - 0x2D05 are AOA product IDs
        if is_aoa_identity(desc.vendor_id(), desc.product_id()) {
            match device.open() {
                Ok(handle) => return Ok(Some(handle)),
                // The WinUSB driver may still be loading, the caller retries
                Err(rusb::Error::Access) | Err(rusb::Error::NotSupported) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
    Ok(None)
}

/// One attempt at the AOA handshake. Never prints: the caller retries and reports once.
fn try_aoa_handshake(device: &Device<Context>) -> rusb::Result<Attempt> {
    let handle = match device.open() {
        Ok(h) => h,
        Err(e) => {
            return Ok(Attempt::NotReady(format!(
                "cannot open the device yet ({e:?})"
            )));
        }
    };

    let claim = handle.claim_interface(0);
    let claimed = claim.is_ok();

    let timeout = Duration::from_secs(1);
    let mut protocol_buffer = [0u8; 2];
    let req_type_in = rusb::request_type(Direction::In, RequestType::Vendor, Recipient::Device);

    let queried = handle.read_control(
        req_type_in,
        AOA_GET_PROTOCOL,
        0,
        0,
        &mut protocol_buffer,
        timeout,
    );
    if let Err(e) = queried {
        if claimed {
            let _ = handle.release_interface(0);
        }
        return Ok(Attempt::NotReady(format!(
            "no answer to the AOA protocol query ({e:?}); claim_interface(0) = {claim:?}"
        )));
    }

    let protocol_version = u16::from_le_bytes(protocol_buffer);
    if protocol_version < 1 {
        if claimed {
            let _ = handle.release_interface(0);
        }
        return Ok(Attempt::NotReady(format!(
            "the phone reported AOA protocol version {protocol_version}, which is too old"
        )));
    }

    send_string(&handle, 0, "Wilson Oon")?;
    send_string(&handle, 1, "WinDroidFS")?;
    send_string(&handle, 2, "High-Speed USB File System")?;
    send_string(&handle, 3, "1.0")?;
    send_string(&handle, 4, "https://wilsoon.dev")?;
    send_string(&handle, 5, "WD-001")?;

    let req_type_out = rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
    if claimed {
        let _ = handle.release_interface(0);
    }
    handle.write_control(req_type_out, AOA_START, 0, 0, &[], timeout)?;
    Ok(Attempt::Started)
}

fn send_string(handle: &DeviceHandle<Context>, index: u16, string: &str) -> rusb::Result<()> {
    let req_type_out = rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
    let mut data = String::from(string);
    data.push('\0');
    handle.write_control(
        req_type_out,
        AOA_SEND_STRING,
        0,
        index,
        data.as_bytes(),
        Duration::from_secs(1),
    )?;
    Ok(())
}

fn find_aoa_endpoints(device: &Device<Context>) -> rusb::Result<Option<(u8, u8, u8)>> {
    let config_desc = device.active_config_descriptor()?;
    for interface in config_desc.interfaces() {
        for interface_desc in interface.descriptors() {
            if interface_desc.class_code() == 0xFF {
                let mut ep_out = None;
                let mut ep_in = None;
                for endpoint_desc in interface_desc.endpoint_descriptors() {
                    if endpoint_desc.transfer_type() == TransferType::Bulk {
                        if endpoint_desc.direction() == Direction::Out {
                            ep_out = Some(endpoint_desc.address());
                        } else if endpoint_desc.direction() == Direction::In {
                            ep_in = Some(endpoint_desc.address());
                        }
                    }
                }

                if let (Some(out), Some(in_ep)) = (ep_out, ep_in) {
                    return Ok(Some((interface.number(), out, in_ep)));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advises_only_below_usb_3() {
        assert_eq!(speed_name(UsbSpeed::High), "USB 2.0");
        assert_eq!(
            speed_advice(UsbSpeed::High).as_deref(),
            Some("Maxes out at 60 MB/s, use a USB 3.0+ port or supported cable to increase this.")
        );
        assert!(speed_advice(UsbSpeed::Full).is_some());
        assert_eq!(speed_advice(UsbSpeed::Super), None);
        assert_eq!(speed_advice(UsbSpeed::SuperPlus), None);
        assert_eq!(speed_advice(UsbSpeed::Unknown), None);
    }
}
