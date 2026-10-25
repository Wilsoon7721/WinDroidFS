use prost::Message;
use rusb::{
    Context, Device, DeviceHandle, Direction, Recipient, RequestType, TransferType, UsbContext,
};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windroid::{DirectoryRequest, DirectoryResponse};
use windroidfs::android::{aoa_identities, is_aoa_identity};
use windroidfs::driver;
use windroidfs::ipc::transport::{self, send};
use windroidfs::ipc::{Request, Response};
mod webdav;

use tokio::sync::{mpsc, oneshot};

pub struct UsbJob {
    pub packet: Packet,
    pub reply: oneshot::Sender<Result<Packet, String>>,
}

pub mod windroid {
    include!(concat!(env!("OUT_DIR"), "/windroid.rs"));
}
use windroid::{Packet, Ping, packet::Payload};

const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;

fn main() -> rusb::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("restore-drivers") {
        restore_drivers_command();
        return Ok(());
    }

    if !transport::service_available() {
        println!("❌ The WinDroidFS driver service isn't reachable.");
        println!("   -> Make sure WinDroidFS is installed, or start it with:");
        println!("        windroidfs-service start   (as Administrator)");
        return Ok(());
    }

    println!("Scanning for an Android device...");

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
            println!("Found Android device. Sending AOA Handshake...");
            handshake_with_driver_swap(vid, pid)?;
        }
        None => println!("No Android devices found (or it's already in Accessory Mode)."),
    }

    // Wait for the phone to come back as the accessory, and have the service bind it to WinUSB
    let Some(aoa_instance) = driver::wait_for_device(&aoa_identities(), Duration::from_secs(10))
    else {
        println!("Accessory Mode device not found. Did the phone prompt get dismissed?");
        return Ok(());
    };
    match send(&Request::EnsureWinusb {
        instance_id: aoa_instance.clone(),
    }) {
        Ok(Response::Ok { detail }) => println!("Accessory on WinUSB: {detail}"),
        Ok(Response::Err { message }) => {
            println!("❌ Could not bind the accessory to WinUSB: {message}");
            return Ok(());
        }
        Ok(_) | Err(_) => {
            println!("❌ Lost contact with the driver service while binding the accessory.");
            return Ok(());
        }
    }

    // Find accessory mode device
    let context = Context::new()?;
    if let Some(handle) = wait_for_aoa_device(&context, Duration::from_secs(10))? {
        println!("Accessory Mode device found and opened!");

        if let Some((interface_num, bulk_out_ep, bulk_in_ep)) =
            find_aoa_endpoints(&handle.device())?
        {
            handle.claim_interface(interface_num)?;
            println!(
                "Pipes established! OUT: 0x{:02x}, IN: 0x{:02x}",
                bulk_out_ep, bulk_in_ep
            );

            let (tx, mut rx) = mpsc::channel::<UsbJob>(32);

            // Run USB loop in background
            std::thread::spawn(move || {
                let timeout = Duration::from_secs(15);
                let mut write_buf = Vec::new();
                let mut read_buf = vec![0u8; 1024 * 1024];

                // WebDAV
                while let Some(job) = rx.blocking_recv() {
                    write_buf.clear();
                    if job.packet.encode(&mut write_buf).is_err() { continue; }

                    // Send request
                    if let Err(e) = handle.write_bulk(bulk_out_ep, &write_buf, timeout) {
                        println!("⚠️ USB Write error: {:?}", e);
                        let _ = job.reply.send(Err("USB Write failed".into()));
                        continue;
                    }

                    // Wait for response
                    match handle.read_bulk(bulk_in_ep, &mut read_buf, timeout) {
                        Ok(bytes) => {
                            match Packet::decode(&read_buf[..bytes]) {
                                Ok(p) => { let _ = job.reply.send(Ok(p)); }
                                Err(e) => { 
                                    println!("⚠️ Protobuf decode error: {:?}", e);
                                    let _ = job.reply.send(Err("Protobuf Decode failed".into())); 
                                }
                            }
                        }
                        Err(e) => {
                            println!("⚠️ USB Read error: {:?}", e);
                            let _ = job.reply.send(Err("USB Read failed".into()));
                            continue;
                        }
                    }
                }
                println!("USB thread shutting down.");
                let _ = handle.release_interface(interface_num);
            });

            // Host the WebDAV Server
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // Ping the device to ensure connection
                let ping_req = Packet { payload: Some(Payload::Ping(Ping { timestamp_ms: 0 })) };
                let (ping_tx, ping_rx) = oneshot::channel();
                tx.send(UsbJob { packet: ping_req, reply: ping_tx }).await.unwrap();
                
                match ping_rx.await {
                    Ok(Ok(_)) => println!("🏓 Phone responded to Ping! USB Bridge is active."),
                    _ => println!("❌ Ping failed! The USB thread might be out of sync, but keeping server alive..."),
                }

                println!("🌐 Starting WebDAV server on http://127.0.0.1:8080...");
                let webdav_tx = tx.clone();
                tokio::spawn(async move {
                    webdav::serve_webdav(webdav_tx).await;
                });

                println!("💽 Mounting Android to Z:\\ drive...");
                let _ = std::process::Command::new("net")
                    .args(["use", "Z:", "/delete"])
                    .output();
                let mount = std::process::Command::new("net")
                    .args(["use", "Z:", "http://127.0.0.1:8080", "/persistent:no"])
                    .output()
                    .unwrap();

                if mount.status.success() {
                    println!("SUCCESS! Your phone is now mounted at Z:\\");
                    println!("Press Ctrl+C in this terminal to safely unmount and exit.");
                } else {
                    println!(
                        "Failed to mount Z:\\ - {}",
                        String::from_utf8_lossy(&mount.stderr)
                    );
                }

                // Terminate on CTRL+C
                tokio::signal::ctrl_c().await.unwrap();

                println!("\nUnmounting Z: drive...");
                let _ = std::process::Command::new("net")
                    .args(["use", "Z:", "/delete"])
                    .output();
            });
        } else {
            println!("❌ Could not find valid IN and OUT Bulk endpoints on the Accessory device.");
        }
    } else {
        println!("Accessory Mode device not found. Did the phone prompt get dismissed?");
    }

    Ok(())
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

fn handshake_with_driver_swap(vid: u16, pid: u16) -> rusb::Result<()> {
    let Some(instance_id) = driver::find_connected_device(vid, pid) else {
        println!("❌ Found {vid:04X}:{pid:04X} over USB, but not in Windows' device list.");
        return Ok(());
    };

    match send(&Request::BeginSwap {
        instance_id: instance_id.clone(),
    }) {
        Ok(Response::Ok { detail }) => println!("🔧 {detail}"),
        Ok(Response::Err { message }) => {
            println!("❌ Could not switch {instance_id} to WinUSB: {message}");
            return Ok(());
        }
        Ok(_) | Err(_) => {
            println!("❌ Lost contact with the driver service before the swap.");
            return Ok(());
        }
    }

    let handshake_sent = handshake_with_retries(vid, pid, Duration::from_secs(20))?;

    if handshake_sent {
        println!("Handshake successful. Waiting for phone to reconnect in Accessory Mode...");
        if !driver::wait_until_disconnected(&instance_id, Duration::from_secs(10)) {
            println!("⚠️  The phone kept its normal identity; restoring its driver anyway.");
        }
    }

    match send(&Request::Rollback {
        instance_id: instance_id.clone(),
    }) {
        Ok(Response::Ok { detail }) => {
            println!("↩️  Restored original driver for {instance_id}: {detail}")
        }
        Ok(Response::Err { message }) => println!(
            "⚠️  Could not restore the original driver: {message}\n   -> Run `windows-daemon restore-drivers`."
        ),
        Ok(_) | Err(_) => println!(
            "⚠️  Lost contact with the driver service during rollback.\n   -> The service will finish it from its journal, or run `windows-daemon restore-drivers`."
        ),
    }
    Ok(())
}

fn find_android_device(context: &Context) -> rusb::Result<Option<Device<Context>>> {
    let devices = context.devices()?;
    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        if windroidfs::android::is_normal_android_identity(desc.vendor_id(), desc.product_id()) {
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
            println!("❌ The phone never accepted the AOA handshake: {last_problem}");
            println!("   -> Unlock the screen and leave it on, then try again.");
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
            println!("❌ Access Denied to Accessory Mode device, even after binding it to WinUSB.");
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
        52,
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
