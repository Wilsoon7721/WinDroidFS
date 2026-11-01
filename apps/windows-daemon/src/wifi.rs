use std::fs;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use sha2::{Digest, Sha256};

pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IDENTITY_KEY: &str = "wifi-identity.key";
const IDENTITY_CERT: &str = "wifi-identity.der";
const PAIRED_FILE: &str = "paired-phones.txt";
/// What the phone app advertises while waiting on Wi-Fi. Must match SERVICE_TYPE in Wifi.kt.
const PHONE_SERVICE: &str = "_windroidfs._tcp.local.";
/// How long to listen for phones answering on the network.
const BROWSE_WAIT: Duration = Duration::from_millis(1500);

/// This PC's TLS identity, made once. A phone paired with it accepts only this certificate.
pub struct Identity {
    pub certificate: Vec<u8>,
    key: Vec<u8>,
}

/// A phone paired over USB, trusted over Wi-Fi from then on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedPhone {
    pub name: String,
    pub certificate: Vec<u8>,
    pub port: u16,
    /// Where it was on Wi-Fi when last seen.
    pub addresses: Vec<String>,
}

fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("WinDroidFS"))
}

/// The identity, made and saved the first time it's needed.
pub fn identity() -> io::Result<Identity> {
    let dir = dir().ok_or_else(|| io::Error::other("APPDATA isn't set"))?;
    let (key_path, cert_path) = (dir.join(IDENTITY_KEY), dir.join(IDENTITY_CERT));
    if let (Ok(key), Ok(certificate)) = (fs::read(&key_path), fs::read(&cert_path)) {
        return Ok(Identity { certificate, key });
    }
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    let made = rcgen::generate_simple_self_signed(vec![format!("WinDroidFS {host}")])
        .map_err(io::Error::other)?;
    let identity = Identity {
        certificate: made.cert.der().to_vec(),
        key: made.signing_key.serialize_der(),
    };
    fs::create_dir_all(&dir)?;
    fs::write(&key_path, &identity.key)?;
    fs::write(&cert_path, &identity.certificate)?;
    Ok(identity)
}

/// Short and stable, for logs and for telling certificates apart.
pub fn fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One line per phone: name, port, addresses (comma-separated), certificate (hex).
pub fn paired_phones() -> Vec<PairedPhone> {
    let Some(text) = dir().and_then(|d| fs::read_to_string(d.join(PAIRED_FILE)).ok()) else {
        return Vec::new();
    };
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<PairedPhone> {
    let mut fields = line.split('\t');
    let name = fields.next()?.to_string();
    let port = fields.next()?.parse().ok()?;
    let addresses = fields
        .next()?
        .split(',')
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();
    let certificate = from_hex(fields.next()?)?;
    Some(PairedPhone {
        name,
        certificate,
        port,
        addresses,
    })
}

fn to_line(phone: &PairedPhone) -> String {
    let hex: String = phone
        .certificate
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let name = phone.name.replace(['\t', '\n', '\r'], " ");
    format!(
        "{name}\t{}\t{}\t{hex}",
        phone.port,
        phone.addresses.join(",")
    )
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Remember `phone`, replacing an earlier pairing with the same certificate. True if it's new.
pub fn save_paired(mut phone: PairedPhone) -> io::Result<bool> {
    let mut phones = paired_phones();
    let earlier = phones.iter().find(|p| p.certificate == phone.certificate);
    let new = earlier.is_none();
    // Paired while off Wi-Fi: keep where it was last seen.
    if phone.addresses.is_empty()
        && let Some(earlier) = earlier
    {
        phone.addresses = earlier.addresses.clone();
    }
    phones.retain(|p| p.certificate != phone.certificate);
    phones.push(phone);
    write_paired(&phones)?;
    Ok(new)
}

pub fn forget_all() -> io::Result<()> {
    write_paired(&[])
}

fn write_paired(phones: &[PairedPhone]) -> io::Result<()> {
    let dir = dir().ok_or_else(|| io::Error::other("APPDATA isn't set"))?;
    fs::create_dir_all(&dir)?;
    let text: String = phones.iter().map(|p| to_line(p) + "\n").collect();
    fs::write(dir.join(PAIRED_FILE), text)
}

/// A phone answering on the network.
#[derive(Clone, Debug)]
pub struct Advert {
    /// [`fingerprint`] of its certificate.
    pub id: String,
    pub name: String,
    pub addresses: Vec<SocketAddr>,
}

/// Phones advertising `service` (mDNS / DNS-SD), listened for for `wait`.
pub fn browse(service: &str, wait: Duration) -> Vec<Advert> {
    let Ok(mdns) = mdns_sd::ServiceDaemon::new() else {
        return Vec::new();
    };
    let Ok(events) = mdns.browse(service) else {
        let _ = mdns.shutdown();
        return Vec::new();
    };
    let deadline = Instant::now() + wait;
    let mut found: Vec<Advert> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            let id = info
                .get_property_val_str("id")
                .unwrap_or_default()
                .to_string();
            let name = info
                .get_property_val_str("name")
                .unwrap_or("Phone")
                .to_string();
            let addresses = info
                .get_addresses_v4()
                .into_iter()
                .map(|ip| SocketAddr::from((ip, info.get_port())))
                .collect();
            found.retain(|f| f.id != id);
            found.push(Advert {
                id,
                name,
                addresses,
            });
        }
    }
    let _ = mdns.shutdown();
    found
}

/// Whether some paired phone is listening on Wi-Fi right now. Only knocks: no TLS.
pub fn reachable() -> bool {
    let seen = browse(PHONE_SERVICE, BROWSE_WAIT);
    paired_phones().iter().any(|phone| {
        addresses(phone, &seen)
            .iter()
            .any(|a| TcpStream::connect_timeout(a, CONNECT_TIMEOUT).is_ok())
    })
}

/// Where to try `phone`: where it's advertising now, then where it was last seen.
fn addresses(phone: &PairedPhone, seen: &[Advert]) -> Vec<SocketAddr> {
    let id = fingerprint(&phone.certificate);
    let mut all: Vec<SocketAddr> = seen
        .iter()
        .filter(|a| a.id == id)
        .flat_map(|a| a.addresses.iter().copied())
        .collect();
    for addr in phone
        .addresses
        .iter()
        .filter_map(|a| (a.as_str(), phone.port).to_socket_addrs().ok())
        .flatten()
    {
        if !all.contains(&addr) {
            all.push(addr);
        }
    }
    all
}

/// The first paired phone that answers over Wi-Fi, with the TLS handshake done: each side proved it holds the paired key.
pub fn connect(identity: &Identity) -> Option<(PairedPhone, SocketAddr, TlsStream)> {
    let seen = browse(PHONE_SERVICE, BROWSE_WAIT);
    for mut phone in paired_phones() {
        for addr in addresses(&phone, &seen) {
            match connect_to(identity, Some(&phone.certificate), addr) {
                Ok(stream) => {
                    // Found somewhere new: remember it for next time.
                    let ip = addr.ip().to_string();
                    if phone.addresses.first() != Some(&ip) {
                        phone.addresses.retain(|a| *a != ip);
                        phone.addresses.insert(0, ip);
                        let _ = save_paired(phone.clone());
                    }
                    return Some((phone, addr, stream));
                }
                Err(e) => log::debug!("Wi-Fi: {} at {addr}: {e}", phone.name),
            }
        }
    }
    None
}

/// TLS 1.3 to `addr`, showing this PC's certificate. `pinned`: the only certificate the phone may show; any, while pairing by code.
pub(crate) fn connect_to(
    identity: &Identity,
    pinned: Option<&[u8]>,
    addr: SocketAddr,
) -> io::Result<TlsStream> {
    let provider = Arc::new(ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            certificate: pinned.map(<[u8]>::to_vec),
            provider,
        }))
        .with_client_auth_cert(
            vec![CertificateDer::from(identity.certificate.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone())),
        )
        .map_err(io::Error::other)?;
    // The pinned certificate is the check, so the name only fills in SNI.
    let name = ServerName::try_from("windroidfs.phone").map_err(io::Error::other)?;
    let mut conn = ClientConnection::new(Arc::new(config), name).map_err(io::Error::other)?;
    let mut sock = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    sock.set_nodelay(true)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock)?;
    }
    Ok(StreamOwned::new(conn, sock))
}

/// Trusts one certificate, the one swapped when pairing, and nothing else. No CA, no names.
/// `None` only while pairing by code, where comparing the code is the check.
#[derive(Debug)]
struct Pinned {
    certificate: Option<Vec<u8>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self
            .certificate
            .as_ref()
            .is_none_or(|pinned| end_entity.as_ref() == pinned.as_slice())
        {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("not the paired phone".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 isn't used".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_phones_round_trip() {
        let phone = PairedPhone {
            name: "Wilson's\tS25+".into(),
            certificate: vec![0, 1, 0xab, 0xff],
            port: 9999,
            addresses: vec!["192.168.1.23".into(), "fe80::1".into()],
        };
        let back = parse_line(&to_line(&phone)).unwrap();
        assert_eq!(back.name, "Wilson's S25+");
        assert_eq!(back.certificate, phone.certificate);
        assert_eq!(back.addresses, phone.addresses);
        assert_eq!(back.port, 9999);
    }
}
