use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::ERROR_CANCELLED;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    PostThreadMessageW,
};
use windroidfs::updater::{self, Update};

use crate::{Shared, UI_THREAD, WM_QUIT_REQUESTED, lock, message_box, notify};

/// Wait this long after starting before the 1st check
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);

/// Check frequency while running
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Check triggers
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Now and then, when automatic checks are on. Says nothing unless there's an update not offered before.
    Automatic,
    /// A phone's app is newer than this PC app knows of, so this PC app is probably behind. Like `Automatic`.
    PhoneNewer,
    /// Menu triggered
    Manual,
}

pub fn spawn(shared: Arc<Shared>, checks: mpsc::Receiver<Check>) {
    std::thread::Builder::new()
        .name("updates".into())
        .spawn(move || {
            let mut next = Instant::now() + FIRST_CHECK_AFTER;
            loop {
                let check =
                    match checks.recv_timeout(next.saturating_duration_since(Instant::now())) {
                        Ok(check) => check,
                        Err(RecvTimeoutError::Timeout) => {
                            next = Instant::now() + CHECK_EVERY;
                            Check::Automatic
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                if check != Check::Manual && !shared.settings().check_for_updates {
                    continue;
                }
                run(&shared, check);
            }
        })
        .expect("failed to start the updates thread");
}

fn run(shared: &Shared, check: Check) {
    let current = env!("CARGO_PKG_VERSION");
    match updater::check() {
        Err(e) => {
            log::warn!("⚠️  Couldn't check for updates: {e}");
            if check == Check::Manual {
                say(&format!("Couldn't check for updates: {e}"), MB_ICONERROR);
            }
        }
        Ok(None) => {
            log::info!("WinDroidFS {current} is the latest version.");
            if check == Check::Manual {
                say(
                    &format!("You have the latest version of WinDroidFS ({current})."),
                    MB_ICONINFORMATION,
                );
            }
        }
        Ok(Some(update)) => {
            log::info!(
                "⬆️  WinDroidFS {} is out (this is {current}).",
                update.version
            );
            {
                let mut settings = lock(&shared.settings);
                if check != Check::Manual && settings.update_offered == update.version {
                    return;
                }
                settings.update_offered = update.version.clone();
                settings.save();
            }
            offer(&update, current);
        }
    }
}

fn offer(update: &Update, current: &str) {
    let version = &update.version;
    let Some(setup) = &update.setup else {
        let text =
            format!("WinDroidFS {version} is out (you have {current}).\n\nOpen its download page?");
        if ask(&text) {
            let _ = updater::open_in_browser(&update.page);
        }
        return;
    };

    let text = format!(
        "WinDroidFS {version} is out (you have {current}).\n\nInstall it now? A connected phone's drive closes while it \
         installs, and opens again after. Windows will ask for permission first.\n\nWhat's new: {}",
        update.page
    );
    if !ask(&text) {
        return;
    }

    notify::show(
        &format!("Downloading WinDroidFS {version}"),
        "It installs once it's downloaded.",
    );
    let path = match updater::download(setup) {
        Ok(path) => path,
        Err(e) => {
            log::warn!("⚠️  Couldn't download WinDroidFS {version}: {e}");
            say(
                &format!(
                    "Couldn't download WinDroidFS {version}: {e}\n\nYou can download it from {}",
                    update.page
                ),
                MB_ICONERROR,
            );
            return;
        }
    };
    match updater::install(&path) {
        Ok(()) => {
            log::info!(
                "⬆️  Installing WinDroidFS {version}; quitting so setup can replace this copy."
            );
            unsafe {
                PostThreadMessageW(
                    UI_THREAD.load(std::sync::atomic::Ordering::SeqCst),
                    WM_QUIT_REQUESTED,
                    0,
                    0,
                )
            };
        }
        Err(e) if e.raw_os_error() == Some(ERROR_CANCELLED as i32) => {
            log::info!("The update to {version} wasn't allowed to run (UAC declined).");
        }
        Err(e) => {
            log::warn!("⚠️  Couldn't start setup for {version}: {e}");
            say(
                &format!("Couldn't start setup for WinDroidFS {version}: {e}"),
                MB_ICONERROR,
            );
        }
    }
}

fn ask(text: &str) -> bool {
    message_box(
        text,
        "WinDroidFS update",
        MB_YESNO | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
    ) == IDYES
}

fn say(text: &str, icon: u32) {
    message_box(
        text,
        "WinDroidFS update",
        MB_OK | icon | MB_SETFOREGROUND | MB_TOPMOST,
    );
}
