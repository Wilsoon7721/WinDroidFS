use std::net::{SocketAddr, TcpStream};
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
use crate::global_drive;
use crate::ipc::transport::send;
use crate::ipc::{Request, Response};
use crate::phone::PhoneClient;
use crate::phone_versions::{self, Verdict};
use crate::windroid::Pong;
use crate::{adb, tether, webdav, wifi, winfsp_fs};

pub use crate::windroid::UsbSpeed;
pub use crate::winfsp_fs::DriveChoice;

const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;

/// Where the WebDAV fallback listens. Loopback only: nothing off this machine can reach it.
const WEBDAV_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 8080);
pub const WINFSP_DOWNLOAD: &str = "https://winfsp.dev/rel/";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long to look for the phone. It drops off the bus for a moment while leaving Accessory Mode after the last link.
const FIND_PHONE_TIMEOUT: Duration = Duration::from_secs(10);
/// Windows sometimes refuses a driver swap while the phone is still settling after re-enumerating.
const SWAP_ATTEMPTS: u32 = 3;

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
    /// Which link to use.
    pub link: LinkChoice,
}

/// Which link to use with a phone that could do either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkChoice {
    /// USB tethering if it's on, then adb if it sees the phone, otherwise Direct USB. Wi-Fi when no phone is plugged in.
    #[default]
    Auto,
    /// Direct USB (Accessory Mode) even when tethering is on, which it turns off.
    DirectUsb,
    /// Only USB tethering: never switches the phone into Accessory Mode, just waits for tethering.
    Tethering,
    /// Only through adb: needs USB debugging on and adb on this PC. No driver swap, and MTP keeps working.
    Adb,
    /// Only over Wi-Fi, to a phone paired while it was plugged in.
    Wifi,
}

/// Whether connecting may need the driver service. Only Direct USB swaps drivers.
pub fn needs_service(link: LinkChoice) -> bool {
    match link {
        LinkChoice::Auto => tether::phone_address().is_none() && adb::phone().is_none(),
        LinkChoice::DirectUsb => true,
        LinkChoice::Tethering | LinkChoice::Adb | LinkChoice::Wifi => false,
    }
}

impl Default for Options {
    fn default() -> Self {
        Options {
            force_webdav: false,
            drive: DriveChoice::Auto,
            read_only: false,
            hide_dotfiles: true,
            link: LinkChoice::Auto,
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
    /// Explorer opened Android\data or Android\obb, which Android keeps every app out of. Once per mount.
    RestrictedFolder,
    /// The phone app is answering but waits for its owner's fingerprint before sharing anything.
    WaitingForUnlock,
    /// The phone app is older than this PC app would like. If `required`, it doesn't mount (the session ends with [`Ended::Failed`])
    PhoneOutdated {
        device_name: String,
        version: String,
        code: u32,
        latest: &'static str,
        required: bool,
        phone_shows_it: bool,
    },
    PhoneNewer,
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
    /// The phone's Disconnect button was pressed. Don't reconnect until it's replugged.
    ClosedByPhone,
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

/// What `link` can see of the phone right now
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presence {
    link: LinkChoice,
    /// Whether an Android phone is on USB at ALL (ignores accessory vs regular USB)
    usb: bool,
    tether: Option<SocketAddr>,
    adb: bool,
    wifi: bool,
}

impl Presence {
    /// Whether there's a phone `link` can reach.
    pub fn any(&self) -> bool {
        match self.link {
            LinkChoice::Tethering => self.tether.is_some(),
            LinkChoice::Adb => self.adb,
            LinkChoice::Wifi => self.wifi,
            LinkChoice::DirectUsb => self.usb,
            LinkChoice::Auto => self.tether.is_some() || self.usb || self.wifi,
        }
    }

    /// Whether `link` would use tethering, which is there now. Waiting on it is only a TCP connection attempt a second on a private link, so unlike Accessory Mode there's no harm in going straight back to waiting for the app.
    pub fn tethered(&self) -> bool {
        self.tether.is_some() && matches!(self.link, LinkChoice::Auto | LinkChoice::Tethering)
    }

    pub fn adopt_usb(&mut self, now: &Presence) {
        self.usb = now.usb;
    }
}

/// Look for a phone that `link` can reach: plugged in (normal identity or Accessory Mode), sharing its connection, through adb, or on Wi-Fi. The slow checks (adb, Wi-Fi) only run for the links that use them
pub fn presence(link: LinkChoice) -> Presence {
    let tether = tether::phone_address();
    let usb = !usb_phones().is_empty();
    let adb = link == LinkChoice::Adb && adb::phone().is_some();
    let wifi = match link {
        LinkChoice::Wifi => wifi::reachable(),
        LinkChoice::Auto => tether.is_none() && !usb && wifi::reachable(),
        _ => false,
    };
    Presence {
        link,
        usb,
        tether,
        adb,
        wifi,
    }
}

/// Whether a phone is plugged in that `link` can reach: in its normal identity, in Accessory Mode, or sharing its connection.
pub fn phone_present(link: LinkChoice) -> bool {
    presence(link).any()
}

/// Android phones on USB, in their normal identity or Accessory Mode, as sorted `(vid, pid)` pairs.
fn usb_phones() -> Vec<(u16, u16)> {
    let Ok(context) = Context::new() else {
        return Vec::new();
    };
    let Ok(devices) = context.devices() else {
        return Vec::new();
    };
    let mut phones: Vec<(u16, u16)> = devices
        .iter()
        .filter_map(|device| {
            let desc = device.device_descriptor().ok()?;
            let (vid, pid) = (desc.vendor_id(), desc.product_id());
            (is_normal_android_identity(vid, pid) || is_aoa_identity(vid, pid))
                .then_some((vid, pid))
        })
        .collect();
    phones.sort_unstable();
    phones
}

/// Connect to the phone, mount it, and stay mounted until `stop` is requested or the phone goes away.
pub fn run(options: &Options, stop: &Stop, on_event: &dyn Fn(Event)) -> Ended {
    match connect_and_serve(options, stop, on_event) {
        Ok(ended) => ended,
        Err(e) => failed(format!("USB error: {e}")),
    }
}

/// How the PC reached the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    DirectUsb,
    Tethering,
    Adb,
    Wifi,
}

impl Route {
    pub fn name(self) -> &'static str {
        match self {
            Route::DirectUsb => "Direct USB",
            Route::Tethering => "USB tethering",
            Route::Adb => "ADB",
            Route::Wifi => "Wi-Fi",
        }
    }
}

/// A phone that's linked and answering, but not mounted.
pub struct Connected {
    pub phone: PhoneClient,
    pub device_name: String,
    pub route: Route,
    /// The phone shares only some folders, so nothing may go around it.
    pub limited_folders: bool,
    /// Held as long as the link: dropping it removes the adb forward.
    pub forward: Option<adb::Forward>,
}

/// Connect to the phone without mounting it, for tools like `windows-daemon bench`.
pub fn connect(
    link: LinkChoice,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> Result<Connected, Ended> {
    match connect_phone(link, stop, on_event) {
        Ok(connected) => connected,
        Err(e) => Err(failed(format!("USB error: {e}"))),
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
    Ok(match connect_phone(options.link, stop, on_event)? {
        Ok(Connected {
            phone,
            device_name,
            forward,
            limited_folders,
            ..
        }) => {
            // adb's Android/data access goes around the phone app, so not when the phone limits what's shared
            let adb = forward
                .as_ref()
                .filter(|_| !limited_folders)
                .map(|f| f.phone().clone());
            let ended = serve(phone, &device_name, adb, options, stop, on_event);
            drop(forward);
            ended
        }
        Err(ended) => ended,
    })
}

/// Driver swap, handshake, opening the accessory, and waiting for the phone app to answer.
fn connect_phone(
    link: LinkChoice,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> rusb::Result<Result<Connected, Ended>> {
    log::info!("Scanning for an Android device...");
    on_event(Event::Connecting);

    if link == LinkChoice::Wifi {
        return Ok(connect_wifi(stop, on_event));
    }

    // Before USB: the AOA handshake would take the phone out of tethering.
    let mut tethered = match link {
        LinkChoice::DirectUsb | LinkChoice::Adb | LinkChoice::Wifi => None,
        LinkChoice::Auto | LinkChoice::Tethering => tether::phone_address(),
    };
    if link == LinkChoice::Auto && tethered.is_none() && phone_is_tethering() {
        log::info!(
            "The phone is starting USB tethering. Waiting for Windows to set up its network..."
        );
        tethered = wait_for_tether_network(stop);
        if tethered.is_none() {
            return Ok(Err(if stop.requested() {
                Ended::Stopped
            } else {
                failed(
                    "The phone is sharing its connection over USB, but Windows hasn't set up a network for it.",
                )
            }));
        }
    }
    if link == LinkChoice::Tethering && tethered.is_none() {
        return Ok(Err(failed(
            "USB tethering isn't on. Turn it on in the phone's Hotspot & Tethering settings.",
        )));
    }
    if let Some(addr) = tethered {
        log::info!(
            "Found a phone sharing its connection over USB (at {}).",
            addr.ip()
        );
        let Some(stream) = connect_tethered(addr, stop, on_event) else {
            return Ok(Err(if stop.requested() {
                Ended::Stopped
            } else {
                Ended::PhoneLost
            }));
        };
        let phone = match PhoneClient::over_tcp(stream) {
            Ok(phone) => phone,
            Err(e) => return Ok(Err(failed(format!("Couldn't set up the link: {e}")))),
        };
        log::info!("⚡ Connected over USB tethering.");
        return Ok(answering(phone, stop, on_event, Route::Tethering));
    }

    // Also before Direct USB, which needs the adb server to let go of the phone
    if matches!(link, LinkChoice::Auto | LinkChoice::Adb) {
        match adb::phone() {
            Some(phone) => {
                log::info!("Found {} through adb.", phone.serial);
                return Ok(connect_adb(&phone, stop, on_event));
            }
            None if link == LinkChoice::Adb => {
                return Ok(Err(failed(
                    "adb doesn't see the phone. Turn on USB debugging, allow this PC on the phone, and install Android's platform-tools.",
                )));
            }
            None => {}
        }
    }

    if link == LinkChoice::Auto && usb_phones().is_empty() && wifi::reachable() {
        return Ok(connect_wifi(stop, on_event));
    }

    // Push the phone into AOA mode
    match find_phone(stop, FIND_PHONE_TIMEOUT)? {
        Some(Found::Normal(vid, pid)) => {
            log::info!("Found Android device. Sending AOA Handshake...");
            if let Err(why) = handshake_with_driver_swap(vid, pid)? {
                return Ok(Err(failed(why)));
            }
        }
        Some(Found::Accessory) => log::info!("Found the phone, already in Accessory Mode."),
        None if stop.requested() => return Ok(Err(Ended::Stopped)),
        None => return Ok(Err(failed("No Android phone found. Is it plugged in?"))),
    }

    // Wait for the phone to come back as the accessory, and have the service bind it to WinUSB
    let Some(aoa_instance) = driver::wait_for_device(&aoa_identities(), Duration::from_secs(10))
    else {
        return Ok(Err(failed(
            "The phone didn't switch to Accessory Mode. Was its prompt dismissed?",
        )));
    };
    match send(&Request::EnsureWinusb {
        instance_id: aoa_instance.clone(),
    }) {
        Ok(Response::Ok { detail }) => log::info!("Accessory on WinUSB: {detail}"),
        Ok(Response::Err { message }) => {
            return Ok(Err(failed(format!(
                "Couldn't bind the accessory to WinUSB: {message}"
            ))));
        }
        Ok(_) | Err(_) => {
            return Ok(Err(failed(
                "Lost contact with the driver service while binding the accessory.",
            )));
        }
    }

    // Find accessory mode device
    let context = Context::new()?;
    let Some(handle) = wait_for_aoa_device(&context, Duration::from_secs(10))? else {
        return Ok(Err(failed(
            "Couldn't open the phone in Accessory Mode, even after binding it to WinUSB.",
        )));
    };
    log::info!("Accessory Mode device found and opened!");

    let Some((interface_num, bulk_out_ep, bulk_in_ep)) = find_aoa_endpoints(&handle.device())?
    else {
        return Ok(Err(failed(
            "The phone's Accessory Mode device has no usable bulk endpoints.",
        )));
    };
    handle.claim_interface(interface_num)?;
    log::info!("Pipes established! OUT: 0x{bulk_out_ep:02x}, IN: 0x{bulk_in_ep:02x}");

    let phone = PhoneClient::start(handle, interface_num, bulk_out_ep, bulk_in_ep);
    let speed = phone.usb_speed();
    log::info!("⚡ Connected over {}.", speed_name(speed));
    if let Some(advice) = speed_advice(speed) {
        log::warn!("🐢 {advice}");
    }
    Ok(answering(phone, stop, on_event, Route::DirectUsb))
}

/// Prove the link works before mounting anything. Over USB, also `pair` for Wi-Fi.
fn answering(
    phone: PhoneClient,
    stop: &Stop,
    on_event: &dyn Fn(Event),
    route: Route,
) -> Result<Connected, Ended> {
    match wait_for_app(&phone, stop, on_event) {
        Some(pong) => {
            log::info!("🏓 Phone responded to Ping! The link is active.");
            check_app_version(&phone, &pong.device_name, &pong, on_event)?;
            let device_name = pong.device_name;
            if route != Route::Wifi {
                pair_for_wifi(&phone, &device_name);
            }
            Ok(Connected {
                phone,
                device_name,
                route,
                limited_folders: pong.limited_folders,
                forward: None,
            })
        }
        None if stop.requested() => Err(Ended::Stopped),
        None if phone.closed_by_phone() => Err(Ended::ClosedByPhone),
        None => Err(Ended::PhoneLost),
    }
}

fn check_app_version(
    phone: &PhoneClient,
    device_name: &str,
    pong: &Pong,
    on_event: &dyn Fn(Event),
) -> Result<(), Ended> {
    let code = pong.app_version_code;
    let version = phone_versions::name_of(code, &pong.app_version_name);
    let (required, latest) = match phone_versions::judge(code) {
        Verdict::Current => return Ok(()),
        Verdict::Newer => {
            log::info!(
                "ℹ️  The phone's app ({version}) is newer than this PC app knows of: there may be an update for this PC."
            );
            on_event(Event::PhoneNewer);
            return Ok(());
        }
        Verdict::Outdated { latest } => (false, latest),
        Verdict::Unsupported { latest, .. } => (true, latest),
    };

    let phone_shows_it = code >= phone_versions::SHOWS_UPDATE_SCREEN;
    if phone_shows_it && let Err(e) = phone.update_notice(required, latest.name) {
        log::debug!("Couldn't show the phone its update screen: {e}");
    }
    on_event(Event::PhoneOutdated {
        device_name: device_name.to_string(),
        version: version.clone(),
        code: code.max(1),
        latest: latest.name,
        required,
        phone_shows_it,
    });
    if required {
        return Err(failed(format!(
            "The WinDroidFS app on {device_name} ({version}) is too old for this PC. Update it to {} or later.",
            latest.name
        )));
    }
    log::info!(
        "⬆️  WinDroidFS {} is out for the phone ({device_name} has {version}).",
        latest.name
    );
    Ok(())
}

/// Swap certificates over this USB link, so the phone can be reached over Wi-Fi later. Older phone apps can't, which is fine.
fn pair_for_wifi(phone: &PhoneClient, device_name: &str) {
    let identity = match wifi::identity() {
        Ok(identity) => identity,
        Err(e) => {
            log::warn!("⚠️  No Wi-Fi identity for this PC, so no pairing: {e}");
            return;
        }
    };
    let resp = match phone.pair(&identity.certificate) {
        Ok(resp) if !resp.certificate.is_empty() => resp,
        Ok(_) => return,
        Err(e) => {
            log::debug!("Wi-Fi pairing skipped: {e}");
            return;
        }
    };
    let paired = wifi::PairedPhone {
        name: device_name.to_string(),
        certificate: resp.certificate,
        port: resp.wifi_port as u16,
        addresses: resp.wifi_addresses,
    };
    match wifi::save_paired(paired) {
        Ok(true) => log::info!("📶 Paired with {device_name} for Wi-Fi."),
        Ok(false) => {}
        Err(e) => log::warn!("⚠️  Couldn't save the Wi-Fi pairing: {e}"),
    }
}

/// Over Wi-Fi to a paired phone. TLS both ways, each side pinned to the certificate swapped over USB.
fn connect_wifi(stop: &Stop, on_event: &dyn Fn(Event)) -> Result<Connected, Ended> {
    let identity = wifi::identity()
        .map_err(|e| failed(format!("Couldn't load this PC's Wi-Fi identity: {e}")))?;
    let Some((paired, addr, stream)) = wifi::connect(&identity) else {
        return Err(failed(
            "No paired phone answered over Wi-Fi. Pair it by connecting once over USB, and keep the app open on the phone.",
        ));
    };
    log::info!("⚡ Connected over Wi-Fi to {} at {addr}.", paired.name);
    answering(PhoneClient::over_tls(stream), stop, on_event, Route::Wifi)
}

/// Connect to the phone app through adb, waiting for it to be opened. Retries, as adb takes the connection even when the app isn't listening.
fn connect_adb(
    phone: &adb::AdbPhone,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> Result<Connected, Ended> {
    let mut asked = false;
    loop {
        if let Some((stream, forward)) = adb::connect(phone)
            && let Ok(client) = PhoneClient::over_tcp(stream)
            && client.ping().is_ok()
        {
            log::info!("⚡ Connected over ADB.");
            let mut connected = answering(client, stop, on_event, Route::Adb)?;
            connected.forward = Some(forward);
            return Ok(connected);
        }
        if !asked {
            asked = true;
            adb::start_app(phone);
            log::info!("📱 Waiting for the WinDroidFS app on the phone.");
            log::info!(
                "   -> If it doesn't start by itself, open it on the phone (and update it if it's an older build)."
            );
            on_event(Event::WaitingForApp);
        }
        if stop.sleep(Duration::from_secs(1)) {
            return Err(Ended::Stopped);
        }
        if adb::phone().as_ref() != Some(phone) {
            return Err(Ended::PhoneLost);
        }
    }
}

/// How long the tethering network may take to appear once the phone's USB says it's tethering, and how long it may vanish before the link is gone
const TETHER_NETWORK_WAIT: Duration = Duration::from_secs(20);
const TETHER_BLINK: Duration = Duration::from_secs(5);

/// Whether an Android phone on USB is in its tethering configuration: an RNDIS or CDC NCM/ECM network interface
fn phone_is_tethering() -> bool {
    let Ok(context) = Context::new() else {
        return false;
    };
    let Ok(devices) = context.devices() else {
        return false;
    };
    devices.iter().any(|device| {
        device
            .device_descriptor()
            .is_ok_and(|desc| is_normal_android_identity(desc.vendor_id(), desc.product_id()))
            && device.active_config_descriptor().is_ok_and(|config| {
                config.interfaces().any(|interface| {
                    interface.descriptors().any(|d| {
                        is_network_interface(d.class_code(), d.sub_class_code(), d.protocol_code())
                    })
                })
            })
    })
}

/// RNDIS, CDC NCM or CDC ECM
fn is_network_interface(class: u8, subclass: u8, protocol: u8) -> bool {
    matches!(
        (class, subclass, protocol),
        (0xE0, 0x01, 0x03) | (0xEF, 0x04, 0x01) | (0x02, 0x0D, _) | (0x02, 0x06, _)
    )
}

/// The phone's tethering address once Windows has set up the network, while the phone stays in tethering mode
fn wait_for_tether_network(stop: &Stop) -> Option<SocketAddr> {
    let deadline = Instant::now() + TETHER_NETWORK_WAIT;
    while Instant::now() < deadline && phone_is_tethering() {
        if let Some(addr) = tether::phone_address() {
            return Some(addr);
        }
        if stop.sleep(Duration::from_millis(500)) {
            return None;
        }
    }
    None
}

/// Connect to the phone app over the tethering link, waiting for it to be opened - `None` if stopped or if tethering ends first.
fn connect_tethered(addr: SocketAddr, stop: &Stop, on_event: &dyn Fn(Event)) -> Option<TcpStream> {
    let mut asked = false;
    let mut gone_since: Option<Instant> = None;
    loop {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(stream) => return Some(stream),
            Err(e) if !asked => {
                asked = true;
                log::info!("📱 Waiting for the WinDroidFS app on the phone ({e}).");
                log::info!("   -> Open it on the phone (and update it if it's an older build).");
                on_event(Event::WaitingForApp);
            }
            Err(_) => {}
        }
        if stop.sleep(Duration::from_secs(1)) {
            return None;
        }
        if tether::phone_address() == Some(addr) {
            gone_since = None;
        } else if gone_since.get_or_insert_with(Instant::now).elapsed() >= TETHER_BLINK {
            return None;
        }
    }
}

/// Ping the phone until its app answers, returning the phone's name - `None` if stopped or if the phone is unplugged first
fn wait_for_app(phone: &PhoneClient, stop: &Stop, on_event: &dyn Fn(Event)) -> Option<Pong> {
    let mut asked = false;
    let mut locked = false;
    // Right after the handshake the app is often still opening, so the first ping failing is normal.
    let patience = Instant::now() + Duration::from_secs(4);
    loop {
        match phone.pong() {
            Ok(pong) if pong.locked => {
                if !locked {
                    locked = true;
                    log::info!("🔒 Unlock WinDroidFS on the phone to finish connecting.");
                    on_event(Event::WaitingForUnlock);
                }
            }
            Ok(pong) => return Some(pong),
            Err(_) if !phone.is_connected() => return None,
            Err(e) => {
                log::debug!("No answer from the app yet: {e}");
                if !asked && Instant::now() >= patience {
                    asked = true;
                    log::info!("📱 Waiting for the WinDroidFS app on the phone.");
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

/// Asks the driver service to publish the drive globally
fn publish_drive(drive: char) -> bool {
    match send(&Request::PublishDrive { letter: drive }) {
        Ok(Response::Ok { detail }) => {
            log::info!("🛡️  {detail}.");
            true
        }
        Ok(Response::Err { message }) => {
            log::warn!("⚠️  Programs run as administrator won't see {drive}: ({message}).");
            false
        }
        Ok(Response::Pong) => false,
        Err(e) => {
            log::warn!(
                "⚠️  Programs run as administrator won't see {drive}: (the driver service isn't reachable: {e})."
            );
            false
        }
    }
}

fn unpublish_drive(drive: char) {
    match send(&Request::UnpublishDrive { letter: drive }) {
        Ok(Response::Err { message }) => log::warn!("⚠️  {message}"),
        Err(e) => log::warn!("⚠️  Couldn't take back {drive}: from elevated programs: {e}"),
        Ok(_) => {}
    }
}

/// Mount the phone and keep it mounted until stopped or the phone goes away. WinFsp when available, WebDAV as fallback
fn serve(
    phone: PhoneClient,
    device_name: &str,
    adb: Option<adb::AdbPhone>,
    options: &Options,
    stop: &Stop,
    on_event: &dyn Fn(Event),
) -> Ended {
    let mount_options = winfsp_fs::MountOptions {
        label: winfsp_fs::volume_label(device_name),
        drive: options.drive,
        read_only: options.read_only,
        hide_dotfiles: options.hide_dotfiles,
        adb,
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
                global_drive::label_in_explorer(label);
                let published = publish_drive(mount.drive);
                on_event(Event::Mounted {
                    drive: mount.drive,
                    device_name: device_name.to_string(),
                    webdav: false,
                    usb_speed: phone.usb_speed(),
                });
                let ended = wait_while_mounted(&phone, stop, || {
                    if mount.take_restricted_hit() {
                        on_event(Event::RestrictedFolder);
                    }
                });
                log::info!("Unmounting {}:...", mount.drive);
                if published {
                    unpublish_drive(mount.drive);
                }
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

/// Block until a stop is requested or the phone goes away, and say which. `tick` runs every quarter second meanwhile.
fn wait_while_mounted(phone: &PhoneClient, stop: &Stop, tick: impl Fn()) -> Ended {
    while phone.is_connected() {
        tick();
        if stop.sleep(Duration::from_millis(250)) {
            return Ended::Stopped;
        }
    }
    if phone.closed_by_phone() {
        log::info!("📴 The phone ended the connection.");
        return Ended::ClosedByPhone;
    }
    log::info!("📴 Lost the phone (unplugged, or the link was closed on the phone).");
    Ended::PhoneLost
}

/// The fallback mount: a local WebDAV server mapped to a drive letter with `net use`
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

    let ended = wait_while_mounted(&phone, stop, || {});

    log::info!("Unmounting {target} drive...");
    let _ = net_use(&["use", &target, "/delete"]);
    drop(rt);
    say_goodbye(&phone, &ended);
    ended
}

/// The phone's identity on the bus
enum Found {
    Normal(u16, u16),
    Accessory,
}

/// Look for the phone until `timeout` - `None` if it never shows up or if stopped
fn find_phone(stop: &Stop, timeout: Duration) -> rusb::Result<Option<Found>> {
    let deadline = Instant::now() + timeout;
    loop {
        let context = Context::new()?;
        for device in context.devices()?.iter() {
            let desc = device.device_descriptor()?;
            let (vid, pid) = (desc.vendor_id(), desc.product_id());
            if is_normal_android_identity(vid, pid) {
                return Ok(Some(Found::Normal(vid, pid)));
            }
            if is_aoa_identity(vid, pid) {
                return Ok(Some(Found::Accessory));
            }
        }
        if Instant::now() >= deadline || stop.sleep(Duration::from_millis(250)) {
            return Ok(None);
        }
    }
}

/// Swap the phone onto WinUSB, send the AOA handshake, then put its driver back. `Err` says why for the user.
fn handshake_with_driver_swap(vid: u16, pid: u16) -> rusb::Result<Result<(), String>> {
    let mut attempt = 1;
    let instance_id = loop {
        let Some(instance_id) = driver::find_connected_device(vid, pid) else {
            return Ok(Err(format!(
                "Found {vid:04X}:{pid:04X} over USB, but not in Windows' device list."
            )));
        };
        match send(&Request::BeginSwap {
            instance_id: instance_id.clone(),
        }) {
            Ok(Response::Ok { detail }) => {
                log::info!("🔧 {detail}");
                break instance_id;
            }
            Ok(Response::Err { message }) => {
                // A failed swap can still leave WinUSB installed, and the journal knows what to put back.
                rollback_driver(&instance_id);
                if attempt == SWAP_ATTEMPTS {
                    return Ok(Err(format!(
                        "Could not switch {instance_id} to WinUSB: {message}"
                    )));
                }
                log::warn!("⚠️  Could not switch {instance_id} to WinUSB ({message}). Retrying...");
                attempt += 1;
                sleep(Duration::from_secs(2));
            }
            Ok(_) | Err(_) => {
                return Ok(Err(
                    "Lost contact with the driver service before the swap.".to_string()
                ));
            }
        }
    };

    let handshake = handshake_with_retries(vid, pid, Duration::from_secs(20))?;
    if handshake.is_ok() {
        log::info!("Handshake successful. Waiting for phone to reconnect in Accessory Mode...");
        if !driver::wait_until_disconnected(&instance_id, Duration::from_secs(10)) {
            log::warn!("⚠️  The phone kept its normal identity; restoring its driver anyway.");
        }
    }
    rollback_driver(&instance_id);
    Ok(handshake.map_err(|problem| {
        format!(
            "The phone never accepted the AOA handshake ({problem}). Unlock the screen and leave it on, then try again."
        )
    }))
}

/// Put the phone's own driver back after a swap.
fn rollback_driver(instance_id: &str) {
    match send(&Request::Rollback {
        instance_id: instance_id.to_string(),
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

/// Keep attempting the AOA handshake until it takes or `timeout` runs out. `Err` is the last problem seen.
fn handshake_with_retries(
    vid: u16,
    pid: u16,
    timeout: Duration,
) -> rusb::Result<Result<(), String>> {
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
            Attempt::Started => return Ok(Ok(())),
            Attempt::NotReady(why) => last_problem = why,
        }

        if Instant::now() >= deadline {
            return Ok(Err(last_problem));
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

    fn seen(link: LinkChoice, usb: bool, tether: bool) -> Presence {
        Presence {
            link,
            usb,
            tether: tether.then(|| "192.168.42.129:9999".parse().unwrap()),
            ..Presence::default()
        }
    }

    #[test]
    fn each_link_only_counts_what_it_can_use() {
        let accessory_only = seen(LinkChoice::Auto, true, false);
        assert!(accessory_only.any());
        assert!(
            !Presence {
                link: LinkChoice::Tethering,
                ..accessory_only.clone()
            }
            .any()
        );
        assert!(
            !Presence {
                link: LinkChoice::Adb,
                ..accessory_only.clone()
            }
            .any()
        );
        assert!(
            Presence {
                link: LinkChoice::DirectUsb,
                ..accessory_only
            }
            .any()
        );
        assert!(seen(LinkChoice::Tethering, true, true).any());
        assert!(!seen(LinkChoice::Auto, false, false).any());
    }

    #[test]
    fn turning_tethering_on_or_off_counts_as_a_change() {
        let ended = seen(LinkChoice::Auto, true, false);
        let tethering = seen(LinkChoice::Auto, true, true);
        assert_ne!(ended, tethering);
        assert_ne!(tethering, ended);
        assert_eq!(ended, seen(LinkChoice::Auto, true, false));
        assert_ne!(
            ended,
            Presence {
                link: LinkChoice::DirectUsb,
                ..ended.clone()
            }
        );
    }

    #[test]
    fn recognises_tethering_interfaces() {
        assert!(is_network_interface(0xE0, 0x01, 0x03)); // RNDIS, as Android describes it
        assert!(is_network_interface(0xEF, 0x04, 0x01)); // RNDIS, as Windows' class driver matches it
        assert!(is_network_interface(0x02, 0x0D, 0x00)); // CDC NCM
        assert!(is_network_interface(0x02, 0x06, 0x00)); // CDC ECM
        assert!(!is_network_interface(0x06, 0x01, 0x01)); // MTP
        assert!(!is_network_interface(0xFF, 0x42, 0x01)); // adb
        assert!(!is_network_interface(0xFF, 0xFF, 0x00)); // Accessory Mode
    }

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
