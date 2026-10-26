#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod notify;
mod settings;

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostQuitMessage, PostThreadMessageW, TranslateMessage,
    WM_APP,
};
use windows_sys::core::BOOL;
use windroidfs::ipc::transport;
use windroidfs::session::{self, DriveChoice, Ended, Event, Options, Stop, UsbSpeed};
use windroidfs::usb_watch::UsbWatch;
use windroidfs::{instance, logging};

use settings::Settings;

/// Posted to the UI thread when the status changes to wake its message loop.
const WM_STATUS_CHANGED: u32 = WM_APP + 1;

/// Posted to the UI thread to quit as if Quit had been clicked (see `on_console_event`).
const WM_QUIT_REQUESTED: u32 = WM_APP + 2;

/// The UI thread for `on_console_event`, which Windows calls on a thread of its own.
static UI_THREAD: AtomicU32 = AtomicU32::new(0);

/// Set once shutting down has finished (the phone unmounted) for `on_console_event`.
static SHUT_DOWN: AtomicBool = AtomicBool::new(false);

/// How often to look for a phone anyway, in case a plug-in notification is missed (or Windows refused to send them).
/// Normally `UsbWatch` wakes straight away.
const FALLBACK_POLL: Duration = Duration::from_secs(30);

/// How long to wait before checking the driver service again when it isn't running.
const SERVICE_RETRY: Duration = Duration::from_secs(3);

/// The `ICON` resource embedded by build.rs (assets/icon.rc).
const ICON_RESOURCE: u16 = 1;

/// What's happening, as the tray shows it.
#[derive(Clone, Debug, PartialEq)]
enum Status {
    Off,
    /// The privileged driver service isn't running, so no phone can be switched to WinUSB.
    NoService,
    WaitingForPhone,
    Connecting,
    /// Linked over USB, but the phone app isn't answering yet.
    WaitingForApp,
    Mounted {
        drive: char,
        device_name: String,
        webdav: bool,
        usb_speed: UsbSpeed,
    },
    /// The last connection ended or failed; waiting for the phone to be unplugged before trying again (see `supervise`).
    Ended(String),
}

impl Status {
    fn describe(&self) -> String {
        match self {
            Status::Off => "Off: phones are left alone".into(),
            Status::NoService => "The WinDroidFS driver service isn't running".into(),
            Status::WaitingForPhone => "Waiting for a phone".into(),
            Status::Connecting => "Connecting to the phone...".into(),
            Status::WaitingForApp => "Open WinDroidFS on your phone to finish connecting".into(),
            Status::Mounted {
                drive,
                device_name,
                webdav,
                usb_speed,
            } => {
                let name = if device_name.is_empty() {
                    "Phone"
                } else {
                    device_name.as_str()
                };
                let via = if *webdav { ", WebDAV" } else { "" };
                format!(
                    "{name} is mounted at {drive}: ({}{via})",
                    session::speed_name(*usb_speed)
                )
            }
            Status::Ended(why) => why.clone(),
        }
    }
}

/// State both threads share.
struct Shared {
    settings: Mutex<Settings>,
    status: Mutex<Status>,
    /// Bumped on every status change.
    status_version: AtomicU64,
    /// Stops the connection in progress (if any).
    session_stop: Mutex<Stop>,
    /// Set with a stop that should reconnect straight away (a setting changed), rather than count as the user turning things off.
    restart: AtomicBool,
    quit: AtomicBool,
    /// Wakes when a USB device comes or goes, or when told to look again.
    watch: UsbWatch,
    /// The slow link speed already notified about, so reconnecting over the same cable doesn't repeat it. Cleared when a fast connection comes along.
    warned_speed: Mutex<Option<UsbSpeed>>,
    ui_thread: u32,
}

impl Shared {
    fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    fn set_status(&self, status: Status) {
        let mut current = lock(&self.status);
        if *current == status {
            return;
        }
        *current = status;
        self.status_version.fetch_add(1, Ordering::SeqCst);
        unsafe { PostThreadMessageW(self.ui_thread, WM_STATUS_CHANGED, 0, 0) };
    }

    /// End the connection in progress. With `restart`, reconnect at once afterwards.
    fn stop_session(&self, restart: bool) {
        if restart {
            self.restart.store(true, Ordering::SeqCst);
        }
        lock(&self.session_stop).request();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() {
    logging::init(cfg!(debug_assertions));

    let Some(_instance) = instance::claim() else {
        // Do nothing if already running.
        return;
    };

    let (settings, existed) = Settings::load();
    if !existed {
        // Start with Windows by default. The menu turns it off, and it's never forced back on after that.
        if !cfg!(debug_assertions) {
            autostart::set_enabled(true);
        }
        settings.save();
    }
    logging::set_file_logging(settings.log);
    log::info!("WinDroidFS {} started.", env!("CARGO_PKG_VERSION"));

    UI_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) };

    let shared = Arc::new(Shared {
        settings: Mutex::new(settings),
        status: Mutex::new(Status::WaitingForPhone),
        status_version: AtomicU64::new(1),
        session_stop: Mutex::new(Stop::new()),
        restart: AtomicBool::new(false),
        quit: AtomicBool::new(false),
        watch: UsbWatch::new(),
        warned_speed: Mutex::new(None),
        ui_thread: unsafe { GetCurrentThreadId() },
    });

    let supervisor = {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || supervise(&shared))
            .expect("failed to start the supervisor thread")
    };

    match Tray::new(&shared) {
        Ok(tray) => tray.run(&shared),
        Err(e) => eprintln!("❌ Couldn't create the tray icon: {e}"),
    }

    shutdown(&shared, supervisor);
    SHUT_DOWN.store(true, Ordering::SeqCst);
}

/// Ctrl+C, Ctrl+Break, or the console window being closed. Unmount first and close.
unsafe extern "system" fn on_console_event(event: u32) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            unsafe {
                PostThreadMessageW(UI_THREAD.load(Ordering::SeqCst), WM_QUIT_REQUESTED, 0, 0)
            };
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            unsafe {
                PostThreadMessageW(UI_THREAD.load(Ordering::SeqCst), WM_QUIT_REQUESTED, 0, 0)
            };

            let deadline = Instant::now() + Duration::from_millis(4500);
            while !SHUT_DOWN.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            1
        }
        _ => 0,
    }
}

/// End the connection (which unmounts) and wait for that to finish before exiting.
fn shutdown(shared: &Shared, supervisor: JoinHandle<()>) {
    shared.quit.store(true, Ordering::SeqCst);
    shared.stop_session(false);
    shared.watch.wake();
    let _ = supervisor.join();
    log::info!("WinDroidFS quit.");
    log::logger().flush();
}

/// Wait for a phone, connect, repeat, until told to quit.
fn supervise(shared: &Shared) {
    let mut wait_for_unplug = false;
    let mut remounting = false;

    while !shared.quit.load(Ordering::SeqCst) {
        let settings = shared.settings();
        if !settings.enabled {
            wait_for_unplug = false;
            shared.set_status(Status::Off);
            shared.watch.wait(FALLBACK_POLL);
            continue;
        }

        if !session::phone_present() {
            wait_for_unplug = false;
            shared.set_status(Status::WaitingForPhone);
            shared.watch.wait(FALLBACK_POLL);
            continue;
        }
        if wait_for_unplug {
            shared.watch.wait(FALLBACK_POLL);
            continue;
        }

        if !transport::service_available() {
            shared.set_status(Status::NoService);
            shared.watch.wait(SERVICE_RETRY);
            continue;
        }

        let stop = Stop::new();
        *lock(&shared.session_stop) = stop.clone();
        if shared.quit.load(Ordering::SeqCst) || !shared.settings().enabled {
            continue;
        }

        let options = Options {
            force_webdav: settings.use_webdav,
            drive: settings.drive,
            read_only: settings.read_only,
            hide_dotfiles: settings.hide_dotfiles,
        };
        let open_when_mounted = settings.open_on_mount && !remounting;
        remounting = false;
        let on_event = |event: Event| match event {
            Event::Connecting => shared.set_status(Status::Connecting),
            Event::WaitingForApp => shared.set_status(Status::WaitingForApp),
            Event::Mounted {
                drive,
                device_name,
                webdav,
                usb_speed,
            } => {
                shared.set_status(Status::Mounted {
                    drive,
                    device_name,
                    webdav,
                    usb_speed,
                });
                warn_if_slow(shared, usb_speed);
                if open_when_mounted {
                    open_drive(shared);
                }
            }
        };

        match session::run(&options, &stop, &on_event) {
            Ended::Stopped => {
                remounting = shared.restart.swap(false, Ordering::SeqCst);
            }
            Ended::PhoneLost => {
                wait_for_unplug = true;
                shared.set_status(Status::Ended(
                    "Disconnected. Unplug and replug the phone to reconnect.".into(),
                ));
            }
            Ended::Failed(why) => {
                wait_for_unplug = true;
                shared.set_status(Status::Ended(format!(
                    "{why} Unplug and replug the phone to try again."
                )));
            }
        }
    }
}

/// A menu item for 1 on/off setting
struct Toggle {
    item: CheckMenuItem,
    /// The setting it controls.
    field: fn(&mut Settings) -> &mut bool,
    /// The setting only applies to a new mount, so remount a mounted phone when it changes.
    remount: bool,
    /// Takes effect some other way, right away.
    applied: Option<fn(bool)>,
}

struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    open: MenuItem,
    enabled: CheckMenuItem,
    drives: Vec<(DriveChoice, CheckMenuItem)>,
    toggles: Vec<Toggle>,
    open_log: MenuItem,
    autostart: CheckMenuItem,
    quit: MenuItem,
    /// The status version last shown
    shown_version: u64,
}

impl Tray {
    fn new(shared: &Shared) -> Result<Tray, Box<dyn std::error::Error>> {
        let settings = shared.settings();

        let status = MenuItem::new("", false, None);
        let open = MenuItem::new("Open drive", false, None);
        let enabled =
            CheckMenuItem::new("Mount phones when plugged in", true, settings.enabled, None);

        let mut drives = vec![(
            DriveChoice::Auto,
            CheckMenuItem::new("Automatic (first free from Z:)", true, false, None),
        )];
        for letter in ('D'..='Z').rev() {
            let item = CheckMenuItem::new(format!("{letter}:"), true, false, None);
            drives.push((DriveChoice::Letter(letter), item));
        }
        let drive_menu = Submenu::new("Drive letter", true);
        for (_, item) in &drives {
            drive_menu.append(item)?;
        }

        let toggle = |text: &str, field: fn(&mut Settings) -> &mut bool, remount: bool| {
            let mut copy = settings.clone();
            let checked = *field(&mut copy);
            Toggle {
                item: CheckMenuItem::new(text, true, checked, None),
                field,
                remount,
                applied: None,
            }
        };
        let toggles = vec![
            // Options
            toggle(
                "Open in Explorer when mounted",
                |s| &mut s.open_on_mount,
                false,
            ),
            toggle("Read-only", |s| &mut s.read_only, true),
            toggle("Hide dot-files", |s| &mut s.hide_dotfiles, true),
            // Troubleshooting
            toggle("Use WebDAV instead of WinFsp", |s| &mut s.use_webdav, true),
            Toggle {
                applied: Some(logging::set_file_logging),
                ..toggle("Keep a log", |s| &mut s.log, false)
            },
        ];
        let options_menu = Submenu::new("Options", true);
        for toggle in &toggles[..3] {
            options_menu.append(&toggle.item)?;
        }
        let open_log = MenuItem::new("Open log folder", true, None);
        let troubleshooting_menu = Submenu::new("Troubleshooting", true);
        for toggle in &toggles[3..] {
            troubleshooting_menu.append(&toggle.item)?;
        }
        troubleshooting_menu.append(&open_log)?;

        let autostart =
            CheckMenuItem::new("Start with Windows", true, autostart::is_enabled(), None);
        let quit = MenuItem::new("Quit WinDroidFS", true, None);

        let menu = Menu::new();
        let items: &[&dyn IsMenuItem] = &[
            &status,
            &open,
            &PredefinedMenuItem::separator(),
            &enabled,
            &drive_menu,
            &options_menu,
            &troubleshooting_menu,
            &autostart,
            &PredefinedMenuItem::separator(),
            &quit,
        ];
        menu.append_items(items)?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(Icon::from_resource(ICON_RESOURCE, None)?)
            .with_tooltip("WinDroidFS")
            .build()?;

        let mut tray = Tray {
            icon,
            status,
            open,
            enabled,
            drives,
            toggles,
            open_log,
            autostart,
            quit,
            shown_version: 0,
        };
        tray.show_drive_choice(settings.drive);
        tray.refresh(shared);
        Ok(tray)
    }

    /// Run the message loop until Quit
    fn run(mut self, shared: &Shared) {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            if msg.message == WM_QUIT_REQUESTED {
                unsafe { PostQuitMessage(0) };
                continue;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                self.on_menu(shared, &event);
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                self.on_icon(shared, &event);
            }
            self.refresh(shared);
        }
    }

    fn on_menu(&mut self, shared: &Shared, event: &MenuEvent) {
        let id = event.id();
        if id == self.quit.id() {
            unsafe { PostQuitMessage(0) };
        } else if id == self.open.id() {
            open_drive(shared);
        } else if id == self.open_log.id() {
            open_log_folder();
        } else if id == self.enabled.id() {
            let enabled = self.enabled.is_checked();
            self.update_settings(shared, |s| s.enabled = enabled);
            if !enabled {
                shared.stop_session(false);
            }
            shared.watch.wake();
        } else if let Some(toggle) = self.toggles.iter().find(|t| id == t.item.id()) {
            let on = toggle.item.is_checked();
            let (field, remount, applied) = (toggle.field, toggle.remount, toggle.applied);
            self.update_settings(shared, |s| *field(s) = on);
            if let Some(apply) = applied {
                apply(on);
            }
            if remount && matches!(shared.status(), Status::Mounted { .. }) {
                shared.stop_session(true);
            }
        } else if id == self.autostart.id() {
            autostart::set_enabled(self.autostart.is_checked());
            self.autostart.set_checked(autostart::is_enabled());
        } else if let Some(choice) = self
            .drives
            .iter()
            .find(|(_, item)| id == item.id())
            .map(|(choice, _)| *choice)
        {
            self.show_drive_choice(choice);
            let changed = shared.settings().drive != choice;
            self.update_settings(shared, |s| s.drive = choice);
            if changed && matches!(shared.status(), Status::Mounted { .. }) {
                shared.stop_session(true);
            }
        }
    }

    fn on_icon(&self, shared: &Shared, event: &TrayIconEvent) {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            open_drive(shared);
        }
    }

    fn update_settings(&self, shared: &Shared, change: impl FnOnce(&mut Settings)) {
        let mut settings = lock(&shared.settings);
        change(&mut settings);
        settings.save();
    }

    fn show_drive_choice(&self, choice: DriveChoice) {
        for (item_choice, item) in &self.drives {
            item.set_checked(*item_choice == choice);
        }
    }

    /// Bring the menu and tooltip up to date with the status if it has changed.
    fn refresh(&mut self, shared: &Shared) {
        let version = shared.status_version.load(Ordering::SeqCst);
        if version == self.shown_version {
            return;
        }
        self.shown_version = version;

        let status = shared.status();
        let line = status.describe();
        self.status.set_text(line.replace('&', "&&"));
        let tooltip: String = format!("WinDroidFS: {line}").chars().take(127).collect();
        let _ = self.icon.set_tooltip(Some(tooltip));

        match status {
            Status::Mounted { drive, .. } => {
                self.open.set_text(format!("Open {drive}:"));
                self.open.set_enabled(true);
            }
            _ => {
                self.open.set_text("Open drive");
                self.open.set_enabled(false);
            }
        }
        self.enabled.set_checked(shared.settings().enabled);
    }
}

/// Tell the user once that the cable or port is holding transfers back.
fn warn_if_slow(shared: &Shared, speed: UsbSpeed) {
    let mut warned = lock(&shared.warned_speed);
    let Some(advice) = session::speed_advice(speed) else {
        *warned = None;
        return;
    };
    if *warned == Some(speed) {
        return;
    }
    *warned = Some(speed);
    notify::show(
        &format!("USB Type: {}", session::speed_name(speed)),
        &advice,
    );
}

/// Open the folder holding the log file in Explorer.
fn open_log_folder() {
    let Some(dir) = logging::log_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
}

/// Open the mounted drive in Explorer if there is one.
fn open_drive(shared: &Shared) {
    if let Status::Mounted { drive, .. } = shared.status() {
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("{drive}:\\"))
            .spawn();
    }
}
