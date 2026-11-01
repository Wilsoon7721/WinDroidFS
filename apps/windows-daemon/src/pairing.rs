use std::time::{Duration, Instant};

use rustls::crypto::ring;
use sha2::{Digest, Sha256};

use crate::phone::{PhoneClient, PhoneError};
use crate::wifi::{self, Identity, PairedPhone};
use crate::windroid::packet::Payload;
use crate::windroid::{PairConfirm, PairHello, PairNonce};

/// What the phone advertises while it waits to pair by code. Must match PAIRING_SERVICE_TYPE in CodePairing.kt.
const PAIRING_SERVICE: &str = "_windroidfs-pair._tcp.local.";
/// Looks again every round, for up to a minute, so the phone's button can be tapped after this starts.
const FIND_ROUND: Duration = Duration::from_secs(3);
const FIND_FOR: Duration = Duration::from_secs(60);
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the phone's owner gets to compare and tap.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(150);

/// Pair with a phone waiting in pairing mode on this network, by comparing a 6-digit code on both screens.
/// `confirm` is shown the phone's name and the code, and says whether the phone shows the same code.
pub fn pair_by_code(
    identity: &Identity,
    confirm: &dyn Fn(&str, &str) -> bool,
) -> Result<PairedPhone, String> {
    let started = Instant::now();
    let advert = loop {
        if let Some(advert) = wifi::browse(PAIRING_SERVICE, FIND_ROUND)
            .into_iter()
            .find(|a| !a.addresses.is_empty())
        {
            break advert;
        }
        if started.elapsed() >= FIND_FOR {
            return Err(
                "No phone was waiting to pair. On the phone, open Local Wi-Fi and tap Pair a new PC."
                    .into(),
            );
        }
    };
    let addr = advert.addresses[0];
    let stream = wifi::connect_to(identity, None, addr)
        .map_err(|e| format!("Couldn't reach {} at {addr}: {e}", advert.name))?;
    let phone_cert = stream
        .conn
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| cert.to_vec())
        .ok_or("The phone sent no certificate.")?;
    let phone = PhoneClient::over_tls(stream);
    let broke = |e: PhoneError| format!("Pairing broke off: {e}");

    let hello = PairHello {
        host_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
    };
    let Payload::PairCommit(commit) = phone
        .request(Payload::PairHello(hello), STEP_TIMEOUT)
        .map_err(broke)?
    else {
        return Err(broke(PhoneError::UnexpectedReply));
    };

    let mut ours = [0u8; 32];
    ring::default_provider()
        .secure_random
        .fill(&mut ours)
        .map_err(|_| "No randomness available.".to_string())?;
    let Payload::PairReveal(reveal) = phone
        .request(
            Payload::PairNonce(PairNonce {
                nonce: ours.to_vec(),
            }),
            STEP_TIMEOUT,
        )
        .map_err(broke)?
    else {
        return Err(broke(PhoneError::UnexpectedReply));
    };
    if commitment(&phone_cert, &identity.certificate, &reveal.nonce) != commit.commitment {
        return Err(
            "The phone's answer didn't match what it promised. Something may be interfering, so nothing was paired."
                .into(),
        );
    }

    let code = code(&identity.certificate, &phone_cert, &ours, &reveal.nonce);
    let accepted = confirm(&commit.device_name, &code);
    let result = phone.request(
        Payload::PairConfirm(PairConfirm { accepted }),
        CONFIRM_TIMEOUT,
    );
    if !accepted {
        return Err("Pairing cancelled.".into());
    }
    let Payload::PairResult(result) = result.map_err(broke)? else {
        return Err(broke(PhoneError::UnexpectedReply));
    };
    if !result.success {
        return Err(if result.error_message.is_empty() {
            "The phone didn't confirm.".into()
        } else {
            result.error_message
        });
    }

    let paired = PairedPhone {
        name: commit.device_name,
        certificate: phone_cert,
        port: result.wifi_port as u16,
        addresses: vec![addr.ip().to_string()],
    };
    wifi::save_paired(paired.clone()).map_err(|e| format!("Paired, but couldn't save it: {e}"))?;
    Ok(paired)
}

/// SHA-256 of `label`, then each part as a 4-byte big-endian length and its bytes. Must match digest in CodePairing.kt.
fn digest(label: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(label.as_bytes());
    for part in parts {
        hash.update((part.len() as u32).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

fn commitment(phone_cert: &[u8], pc_cert: &[u8], phone_nonce: &[u8]) -> Vec<u8> {
    digest("windroidfs-commit", &[phone_cert, pc_cert, phone_nonce]).to_vec()
}

/// Six digits as "123 456".
fn code(pc_cert: &[u8], phone_cert: &[u8], pc_nonce: &[u8], phone_nonce: &[u8]) -> String {
    let hash = digest(
        "windroidfs-code",
        &[pc_cert, phone_cert, pc_nonce, phone_nonce],
    );
    let n = u32::from_be_bytes(hash[..4].try_into().unwrap()) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same inputs and answers are in CodePairingTest.kt, so the two sides can't drift apart.
    #[test]
    fn matches_the_phone() {
        let (pc_cert, phone_cert) = (
            b"pc certificate".as_slice(),
            b"phone certificate".as_slice(),
        );
        let (pc_nonce, phone_nonce) = ([1u8; 32], [2u8; 32]);
        let commit: String = commitment(phone_cert, pc_cert, &phone_nonce)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let code = code(pc_cert, phone_cert, &pc_nonce, &phone_nonce);
        assert_eq!(
            commit,
            "dcc2a9b7ac2a4e9a5f0d7f98823ae76179282c59a868ed402d94fe00d029555f"
        );
        assert_eq!(code, "565 043");
    }
}
