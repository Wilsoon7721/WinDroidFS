use std::time::Duration;

use rusb::UsbContext;
use windroidfs::android::{is_aoa_identity, is_normal_android_identity};
use windroidfs::ipc::transport::{self, send};
use windroidfs::ipc::{Request, Response};
use windroidfs::session::{self, Ended, Event, Options, Stop};
use windroidfs::{instance, logging};

/// After the phone disconnects, how often and how many times to look for it coming back before giving up and exiting.
const RETURN_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const RETURN_CHECKS: u32 = 3;

fn main() -> rusb::Result<()> {
    logging::init(true);

    if std::env::args().nth(1).as_deref() == Some("restore-drivers") {
        restore_drivers_command();
        return Ok(());
    }

    let options = Options {
        force_webdav: std::env::args().any(|a| a == "--webdav"),
        ..Options::default()
    };

    let Some(_instance) = instance::claim() else {
        println!("❌ WinDroidFS is already running (look for its icon in the taskbar's tray).");
        println!(
            "   -> Exit the existing instance first: multiple instances would fight over the phone."
        );
        return Ok(());
    };

    if !transport::service_available() {
        println!("❌ The WinDroidFS driver service isn't reachable.");
        println!("   -> Make sure WinDroidFS is installed, or start it with:");
        println!("        windroidfs-service start   (Run as Administrator)");
        return Ok(());
    }

    let stop = Stop::new();
    // Ctrl+C is only caught once the phone is mounted. Before that, it should just end the process as usual
    let on_event = |event: Event| {
        if let Event::Mounted { .. } = event {
            catch_ctrl_c(&stop);
            println!("Press Ctrl+C in this terminal to safely unmount and exit.");
        }
    };

    loop {
        match session::run(&options, &stop, &on_event) {
            Ended::Stopped | Ended::Failed(_) => return Ok(()),
            Ended::PhoneLost => {
                println!(
                    "🔌 Waiting for the phone to come back (checking every {}s, {} times)...",
                    RETURN_CHECK_INTERVAL.as_secs(),
                    RETURN_CHECKS
                );
                if !wait_for_phone_to_return(&stop)? {
                    println!("👋 The phone didn't come back. Exiting.");
                    return Ok(());
                }
                println!("🔌 Phone found again. Reconnecting...\n");
            }
        }
    }
}

/// Look for the phone `RETURN_CHECKS` times, `RETURN_CHECK_INTERVAL` apart. True as soon as one shows up. False on Ctrl+C.
fn wait_for_phone_to_return(stop: &Stop) -> rusb::Result<bool> {
    for _ in 0..RETURN_CHECKS {
        if stop.sleep(RETURN_CHECK_INTERVAL) {
            return Ok(false);
        }
        let context = rusb::Context::new()?;
        for device in context.devices()?.iter() {
            let desc = device.device_descriptor()?;
            let (vid, pid) = (desc.vendor_id(), desc.product_id());
            if is_normal_android_identity(vid, pid) || is_aoa_identity(vid, pid) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Turn Ctrl+C into a stop request. Once caught it stays caught (Windows has no way to put the default back), which is why waiting for the phone to return checks `stop` too.
fn catch_ctrl_c(stop: &Stop) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    let stop = stop.clone();
    INSTALL.call_once(move || {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to start the async runtime");
            rt.block_on(async {
                while tokio::signal::ctrl_c().await.is_ok() {
                    println!();
                    stop.request();
                }
            });
        });
    });
}

fn restore_drivers_command() {
    if !transport::service_available() {
        println!("❌ The WinDroidFS driver service isn't reachable; can't restore drivers.");
        return;
    }
    match send(&Request::RestoreAll) {
        Ok(Response::Ok { detail }) => println!("↩️  {detail}"),
        Ok(Response::Err { message }) => println!("❌ Could not restore drivers: {message}"),
        Ok(_) | Err(_) => println!("❌ Lost contact with the driver service."),
    }
}
