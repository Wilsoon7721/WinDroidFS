use std::net::{Ipv4Addr, SocketAddr};

use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

/// Where the phone app listens on the tethering link. Must match TETHER_PORT in LinkService.kt.
pub const PORT: u16 = 9999;

const ERROR_BUFFER_OVERFLOW: u32 = 111;

/// How Windows names the network adapter a USB-tethered phone brings up.
fn is_tethering_adapter(description: &str) -> bool {
    let description = description.to_lowercase();
    ["remote ndis", "rndis", "usbncm"]
        .iter()
        .any(|name| description.contains(name))
}

/// The phone's address if it's sharing its connection with this PC over USB: the gateway of the tethering adapter.
pub fn phone_address() -> Option<SocketAddr> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    // u64s, so the adapter structs inside are aligned
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0; (size as usize).div_ceil(8)];
        let result = unsafe {
            GetAdaptersAddresses(
                AF_INET as u32,
                flags,
                std::ptr::null(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        match result {
            0 => break,
            ERROR_BUFFER_OVERFLOW => continue,
            _ => return None,
        }
    }

    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !adapter.is_null() {
        let a = unsafe { &*adapter };
        if a.OperStatus == IfOperStatusUp
            && is_tethering_adapter(&wide_to_string(a.Description))
            && let Some(ip) = gateway(a)
        {
            return Some(SocketAddr::from((ip, PORT)));
        }
        adapter = a.Next;
    }
    None
}

fn gateway(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<Ipv4Addr> {
    let gateway = adapter.FirstGatewayAddress;
    if gateway.is_null() {
        return None;
    }
    let sockaddr = unsafe { (*gateway).Address.lpSockaddr };
    if sockaddr.is_null() || unsafe { (*sockaddr).sa_family } != AF_INET {
        return None;
    }
    let sin = unsafe { &*(sockaddr as *const SOCKADDR_IN) };
    let addr = unsafe { sin.sin_addr.S_un.S_addr };
    Some(Ipv4Addr::from(u32::from_be(addr)))
}

fn wide_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = (0..).take_while(|&i| unsafe { *ptr.add(i) } != 0).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

#[cfg(test)]
mod tests {
    use super::is_tethering_adapter;

    #[test]
    fn recognises_tethering_adapters() {
        assert!(is_tethering_adapter(
            "Remote NDIS based Internet Sharing Device"
        ));
        assert!(is_tethering_adapter(
            "SAMSUNG Mobile USB Remote NDIS Network Device"
        ));
        assert!(is_tethering_adapter("UsbNcm Host Device"));
        assert!(!is_tethering_adapter("Realtek PCIe GbE Family Controller"));
        assert!(!is_tethering_adapter("Apple Mobile Device Ethernet"));
    }
}
