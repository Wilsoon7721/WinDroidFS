use rusb::{Context, Device, DeviceHandle, Direction, Recipient, RequestType, UsbContext};
use std::time::Duration;

// AOA Protocol Constants
const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;

// AOA String IDs
const STRING_MANUFACTURER: u16 = 0;
const STRING_MODEL: u16 = 1;
const STRING_DESCRIPTION: u16 = 2;
const STRING_VERSION: u16 = 3;
const STRING_URI: u16 = 4;
const STRING_SERIAL: u16 = 5;

pub mod windroid {
    include!(concat!(env!("OUT_DIR"), "/windroid.rs"));
}

fn main() -> rusb::Result<()> {
    let context = Context::new()?;
    let devices = context.devices()?;

    // 1. Find the specific Samsung device
    let mut target_device: Option<Device<Context>> = None;
    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        if desc.vendor_id() == 0x04e8 && desc.product_id() == 0x6860 {
            target_device = Some(device);
            break;
        }
    }

    // Rust's match statement unwraps the Option.
    // If target_device is None, we exit gracefully.
    let device = match target_device {
        Some(dev) => dev,
        None => {
            println!("Phone not found. Is it plugged in?");
            return Ok(());
        }
    };

    println!("Found Samsung phone. Attempting AOA Handshake...");

    // 2. Open the device to get a handle
    let handle = device
        .open()
        .expect("FAILED AT: device.open(). Is Windows blocking raw access?");
    let timeout = Duration::from_secs(1);

    // 3. Request 51: Get Protocol
    // We expect the phone to write exactly 2 bytes (a 16-bit integer) back into our buffer.
    let mut protocol_buffer = [0u8; 2];
    let req_type_in = rusb::request_type(Direction::In, RequestType::Vendor, Recipient::Device);

    let bytes_read = handle
        .read_control(
            req_type_in,
            AOA_GET_PROTOCOL,
            0,
            0,
            &mut protocol_buffer,
            timeout,
        )
        .expect("FAILED AT: read_control()");

    if bytes_read == 2 {
        // Convert the 2 raw bytes into a Rust u16 integer (Little Endian format)
        let protocol_version = u16::from_le_bytes(protocol_buffer);
        println!("Device supports AOA Protocol Version: {}", protocol_version);

        if protocol_version >= 1 {
            // 4. Request 52: Send Identity Strings
            send_string(&handle, STRING_MANUFACTURER, "Wilson Oon")?;
            send_string(&handle, STRING_MODEL, "WinDroidFS")?;
            send_string(&handle, STRING_DESCRIPTION, "High-Speed USB File System")?;
            send_string(&handle, STRING_VERSION, "1.0")?;
            send_string(&handle, STRING_URI, "https://windroid.wilsoon.dev")?;
            send_string(&handle, STRING_SERIAL, "WD-001")?;

            // 5. Request 53: Start Accessory Mode
            // This defines a request in the OUT direction (computer -> phone), the request type is manufacturer-specfic (Vendor), and the Recipient is the entire device itself
            let req_type_out =
                rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Device);

            println!("Sending Start command...");
            handle.write_control(req_type_out, AOA_START, 0, 0, &[], timeout)?;

            println!(
                "Handshake sent! The phone should now disconnect and reconnect in Accessory Mode."
            );
        }
    } else {
        println!("Failed to read protocol version.");
    }

    Ok(())
}

// A helper function to send strings
fn send_string(handle: &DeviceHandle<Context>, index: u16, string: &str) -> rusb::Result<()> {
    let req_type_out = rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
    let timeout = Duration::from_secs(1);

    // We convert the Rust string to bytes, and add a null terminator (\0)
    let mut data = String::from(string);
    data.push('\0');

    handle.write_control(
        req_type_out,
        AOA_SEND_STRING,
        0,
        index,
        data.as_bytes(),
        timeout,
    )?;
    Ok(())
}
