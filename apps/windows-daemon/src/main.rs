use prost::Message;
use rusb::{
    Context, Device, DeviceHandle, Direction, Recipient, RequestType, TransferType, UsbContext,
};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// 1. Include the generated Protobuf code!
pub mod windroid {
    include!(concat!(env!("OUT_DIR"), "/windroid.rs"));
}
use windroid::{Packet, Ping, packet::Payload};

const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;

// 2. A list of standard Android Manufacturer Vendor IDs
const ANDROID_VIDS: &[u16] = &[
    0x04e8, // Samsung
    0x18d1, // Google / Nexus / Pixel
    0x22d9, // OnePlus / Oppo
    0x2717, // Xiaomi
    0x1004, // LG
    0x0fce, // Sony
    0x22b8, // Motorola
    0x2a70, // OnePlus
];

fn main() -> rusb::Result<()> {
    let context = Context::new()?;

    println!("Scanning for an Android device...");

    // Find Android phone and push it into AOA mode
    if let Some(device) = find_android_device(&context)? {
        println!("Found Android device. Sending AOA Handshake...");
        if send_aoa_handshake(&device)? {
            println!("Handshake successful. Waiting for phone to reconnect in Accessory Mode...");
        }
    } else {
        println!("No Android devices found (or it's already in Accessory Mode).");
    }

    sleep(Duration::from_secs(2));

    // Find accessory mode device
    if let Some(mut handle) = find_and_open_aoa_device(&context)? {
        println!("Accessory Mode device found and opened!");

        // We must claim the USB interface before we can send raw data
        let interface_num = 0;
        handle.claim_interface(interface_num)?;

        // Find the bulk endpoints (we need the OUT endpoint to send data to the phone)
        let bulk_out_ep = find_bulk_out_endpoint(&handle.device(), interface_num)?;

        println!(
            "Pipe established on Endpoint 0x{:02x}. Creating Protobuf Ping...",
            bulk_out_ep
        );

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let ping_msg = Ping { timestamp_ms: now };

        let packet = Packet {
            payload: Some(Payload::Ping(ping_msg)),
        };

        let mut byte_buffer = Vec::new();
        packet.encode(&mut byte_buffer).unwrap();

        let timeout = Duration::from_secs(15);
        println!("Waiting up to 15 seconds for the Android app to open and accept the data...");

        match handle.write_bulk(bulk_out_ep, &byte_buffer, timeout) {
            Ok(bytes_written) => println!(
                "✅ Success! Sent {} bytes of Protobuf data to the phone.",
                bytes_written
            ),
            Err(e) => println!("❌ Failed to write to bulk endpoint: {:?}", e),
        }

        handle.release_interface(interface_num)?;
    } else {
        println!("Accessory Mode device not found. Did the phone prompt get dismissed?");
    }

    Ok(())
}

fn find_android_device(context: &Context) -> rusb::Result<Option<Device<Context>>> {
    let devices = context.devices()?;
    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        if ANDROID_VIDS.contains(&desc.vendor_id())
            && desc.product_id() != 0x2d00
            && desc.product_id() != 0x2d01
        {
            return Ok(Some(device));
        }
    }
    Ok(None)
}

fn find_and_open_aoa_device(context: &Context) -> rusb::Result<Option<DeviceHandle<Context>>> {
    let devices = context.devices()?;
    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        // 0x18D1 is Google, 0x2D00 and 0x2D01 are AOA product IDs
        if desc.vendor_id() == 0x18d1
            && (desc.product_id() == 0x2d00 || desc.product_id() == 0x2d01)
        {
            match device.open() {
                Ok(handle) => return Ok(Some(handle)),
                Err(rusb::Error::Access) | Err(rusb::Error::NotSupported) => {
                    println!("❌ Access Denied to Accessory Mode device!");
                    println!(
                        "   -> The phone successfully switched to Accessory Mode, but Windows is blocking the new connection."
                    );
                    println!(
                        "   -> Open Zadig NOW, find the Android Accessory (VID 18D1), and install WinUSB."
                    );
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(None)
}

fn send_aoa_handshake(device: &Device<Context>) -> rusb::Result<bool> {
    let handle = match device.open() {
        Ok(h) => h,
        Err(rusb::Error::Access) | Err(rusb::Error::NotSupported) => {
            println!(
                "❌ Access Denied: Windows is protecting this device with a proprietary driver (e.g., MTP/Samsung)."
            );
            println!("   -> Please use Zadig to temporarily swap the driver to WinUSB.");
            return Ok(false);
        }
        Err(e) => {
            println!("❌ Hardware Error while opening device: {:?}", e);
            return Ok(false);
        }
    };

    let timeout = Duration::from_secs(1);
    let mut protocol_buffer = [0u8; 2];
    let req_type_in = rusb::request_type(Direction::In, RequestType::Vendor, Recipient::Device);

    if handle
        .read_control(
            req_type_in,
            AOA_GET_PROTOCOL,
            0,
            0,
            &mut protocol_buffer,
            timeout,
        )
        .is_ok()
    {
        let protocol_version = u16::from_le_bytes(protocol_buffer);
        if protocol_version >= 1 {
            send_string(&handle, 0, "Wilson Oon")?;
            send_string(&handle, 1, "WinDroidFS")?;
            send_string(&handle, 2, "High-Speed USB File System")?;
            send_string(&handle, 3, "1.0")?;
            send_string(&handle, 4, "https://wilsoon.dev")?;
            send_string(&handle, 5, "WD-001")?;

            let req_type_out =
                rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
            handle.write_control(req_type_out, AOA_START, 0, 0, &[], timeout)?;
            return Ok(true);
        }
    }
    Ok(false)
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

fn find_bulk_out_endpoint(device: &Device<Context>, interface_num: u8) -> rusb::Result<u8> {
    let config_desc = device.active_config_descriptor()?;
    for interface in config_desc.interfaces() {
        if interface.number() == interface_num {
            for interface_desc in interface.descriptors() {
                for endpoint_desc in interface_desc.endpoint_descriptors() {
                    if endpoint_desc.transfer_type() == TransferType::Bulk
                        && endpoint_desc.direction() == Direction::Out
                    {
                        return Ok(endpoint_desc.address());
                    }
                }
            }
        }
    }
    Err(rusb::Error::NotFound)
}
