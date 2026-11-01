use std::time::{Duration, Instant};

use rusb::UsbContext;
use windroidfs::android::{is_aoa_identity, is_normal_android_identity};
use windroidfs::ipc::transport::{self, send};
use windroidfs::ipc::{Request, Response};
use windroidfs::phone::{MAX_READ, MAX_WRITE, PhoneClient, PhoneError};
use windroidfs::session::{self, Ended, Event, LinkChoice, Options, Route, Stop};
use windroidfs::{instance, logging, pairing, wifi};

/// After the phone disconnects, how often and how many times to look for it coming back before giving up and exiting.
const RETURN_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const RETURN_CHECKS: u32 = 3;

fn main() -> rusb::Result<()> {
    logging::init(true);

    if std::env::args().nth(1).as_deref() == Some("restore-drivers") {
        restore_drivers_command();
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("pair") {
        pair_command();
        return Ok(());
    }

    let flag = |name: &str| std::env::args().any(|a| a == name);
    let options = Options {
        force_webdav: flag("--webdav"),
        link: if flag("--tethering") {
            LinkChoice::Tethering
        } else if flag("--direct-usb") {
            LinkChoice::DirectUsb
        } else if flag("--adb") {
            LinkChoice::Adb
        } else if flag("--wifi") {
            LinkChoice::Wifi
        } else {
            LinkChoice::Auto
        },
        ..Options::default()
    };

    let Some(_instance) = instance::claim() else {
        println!("❌ WinDroidFS is already running (look for its icon in the taskbar's tray).");
        println!(
            "   -> Exit the existing instance first: multiple instances would fight over the phone."
        );
        return Ok(());
    };

    if session::needs_service(options.link) && !transport::service_available() {
        println!("❌ The WinDroidFS driver service isn't reachable.");
        println!("   -> Make sure WinDroidFS is installed, or start it with:");
        println!("        windroidfs-service start   (Run as Administrator)");
        return Ok(());
    }

    if std::env::args().nth(1).as_deref() == Some("bench") {
        bench_command(options.link);
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
            Ended::Stopped | Ended::Failed(_) | Ended::ClosedByPhone => return Ok(()),
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

/// Where `bench` puts its test file on the phone. Deleted afterwards.
const BENCH_PATH: &str = "/Download/.windroidfs-bench.tmp";
const BENCH_PINGS: usize = 50;
/// Default size of the test file in MiB; `windows-daemon bench 512` overrides it.
const BENCH_DEFAULT_MIB: usize = 128;

/// Measure the link without mounting: request round-trip time, then write and read speed.
fn bench_command(link: LinkChoice) {
    // The first number after `bench`, wherever flags like --wifi sit.
    let mib: usize = std::env::args()
        .skip(2)
        .find_map(|a| a.parse().ok())
        .unwrap_or(BENCH_DEFAULT_MIB);

    let stop = Stop::new();
    let Ok(connected) = session::connect(link, &stop, &|_| {}) else {
        return; // already explained by session::connect
    };
    let phone = connected.phone;
    let over = match connected.route {
        Route::DirectUsb => session::speed_name(phone.usb_speed()),
        route => route.name(),
    };
    println!(
        "\n📏 Benchmarking {} over {over}, {mib} MiB test file\n",
        connected.device_name
    );

    if let Err(e) = bench(&phone, mib) {
        println!("❌ Benchmark failed: {e}");
    }
    let _ = phone.delete(BENCH_PATH);
    phone.goodbye();
}

fn bench(phone: &PhoneClient, mib: usize) -> Result<(), PhoneError> {
    // Round trip with (almost) no data: the fixed cost every request pays.
    let mut pings = Vec::with_capacity(BENCH_PINGS);
    for _ in 0..BENCH_PINGS {
        let started = Instant::now();
        phone.ping()?;
        pings.push(started.elapsed());
    }
    pings.sort();
    println!(
        "Ping      min {:>6.2} ms   median {:>6.2} ms   max {:>6.2} ms",
        ms(pings[0]),
        ms(pings[pings.len() / 2]),
        ms(pings[pings.len() - 1])
    );

    // Pseudo-random, so nothing along the way can compress it.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let chunk: Vec<u8> = (0..MAX_WRITE)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();

    let _ = phone.delete(BENCH_PATH);
    phone.create(BENCH_PATH, false)?;
    let total = (mib * 1024 * 1024) as u64;
    let requests = total.div_ceil(MAX_WRITE as u64);

    let started = Instant::now();
    let mut offset = 0u64;
    while offset < total {
        let len = (total - offset).min(MAX_WRITE as u64) as usize;
        let (written, _) = phone.write(BENCH_PATH, offset, &chunk[..len])?;
        offset += written;
    }
    let writing = started.elapsed();
    let started = Instant::now();
    phone.flush(BENCH_PATH)?;
    let syncing = started.elapsed();
    report("Write", total, writing, requests);
    println!(
        "          then {:.2} s for the phone to sync it to storage",
        syncing.as_secs_f64()
    );

    let started = Instant::now();
    let mut offset = 0u64;
    let mut reads = 0u64;
    while offset < total {
        let len = (total - offset).min(MAX_READ as u64) as u32;
        let got = phone.read(BENCH_PATH, offset, len)?;
        if got.data.is_empty() {
            break;
        }
        offset += got.data.len() as u64;
        reads += 1;
    }
    report("Read", offset, started.elapsed(), reads);
    Ok(())
}

fn report(what: &str, bytes: u64, took: Duration, requests: u64) {
    let secs = took.as_secs_f64();
    println!(
        "{what:<9} {:>6.1} MB/s   ({:.2} s, {:.1} ms per 1 MiB request)",
        bytes as f64 / 1_048_576.0 / secs,
        secs,
        ms(took) / requests.max(1) as f64
    );
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Pair with a phone over Wi-Fi by comparing a code, for phones that can't be plugged in.
fn pair_command() {
    let identity = match wifi::identity() {
        Ok(identity) => identity,
        Err(e) => {
            println!("❌ No Wi-Fi identity for this PC: {e}");
            return;
        }
    };
    println!(
        "🔎 Looking for a phone waiting to pair (Local Wi-Fi → Pair a new PC on the phone)..."
    );
    let confirm = |name: &str, code: &str| {
        println!("\n📱 {name} wants to pair. Its screen should show:\n\n      {code}\n");
        print!("Same code on the phone? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        answer.trim().eq_ignore_ascii_case("y")
    };
    match pairing::pair_by_code(&identity, &confirm) {
        Ok(phone) => println!(
            "📶 Paired with {}. Connect with `windows-daemon --wifi`.",
            phone.name
        ),
        Err(why) => println!("❌ {why}"),
    }
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
