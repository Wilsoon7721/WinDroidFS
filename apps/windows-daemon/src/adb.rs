use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The phone app's local socket. Must match SOCKET_NAME in Adb.kt.
const SOCKET: &str = "localabstract:windroidfs";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A phone adb can talk to: plugged in, USB debugging on, and this PC allowed on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbPhone {
    exe: PathBuf,
    pub serial: String,
}

/// adb.exe on PATH, else where Android Studio's SDK keeps it.
fn find_adb() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let sdks = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .chain(
            std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Android").join("Sdk")),
        )
        .map(|sdk| sdk.join("platform-tools"));
    on_path
        .into_iter()
        .chain(sdks)
        .map(|dir| dir.join("adb.exe"))
        .find(|exe| exe.is_file())
}

fn run(exe: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The first authorized phone in `adb devices`
fn first_ready(devices: &str) -> Option<String> {
    devices.lines().skip(1).find_map(|line| {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(serial), Some("device")) => Some(serial.to_string()),
            _ => None,
        }
    })
}

pub fn phone() -> Option<AdbPhone> {
    let exe = find_adb()?;
    let serial = first_ready(&run(&exe, &["devices"])?)?;
    Some(AdbPhone { exe, serial })
}

/// An `adb forward` from a free local port to the phone app. Removed when dropped.
pub struct Forward {
    phone: AdbPhone,
    port: u16,
}

impl Forward {
    pub fn phone(&self) -> &AdbPhone {
        &self.phone
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let rule = format!("tcp:{}", self.port);
        let _ = run(
            &self.phone.exe,
            &["-s", &self.phone.serial, "forward", "--remove", &rule],
        );
    }
}

/// The phone app's link service and main screen. Must match the manifest.
const APP_SERVICE: &str = "dev.wilsoon.windroidfs/.link.LinkService";
const APP_ADB_ACTION: &str = "dev.wilsoon.windroidfs.action.ADB";
const APP_SCREEN: &str = "dev.wilsoon.windroidfs/.MainActivity";

/// Get the phone app listening without anyone opening it. Quietly as a service if Android allows it, else by opening the app.
pub fn start_app(phone: &AdbPhone) {
    // `am` reports a refusal as "Error: ..." rather than through its exit code.
    let am = |args: &[&str]| {
        let mut full = vec!["-s", &phone.serial, "shell", "am"];
        full.extend_from_slice(args);
        Command::new(&phone.exe)
            .args(&full)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|out| {
                let said = [out.stdout, out.stderr].concat();
                out.status.success() && !String::from_utf8_lossy(&said).contains("Error")
            })
    };
    if am(&[
        "start-foreground-service",
        "-n",
        APP_SERVICE,
        "-a",
        APP_ADB_ACTION,
    ]) {
        log::info!("📱 Started the WinDroidFS app on the phone through adb.");
    } else if am(&["start", "-n", APP_SCREEN]) {
        log::info!("📱 Opened the WinDroidFS app on the phone through adb.");
    }
}

/// Connect to the phone app through adb
pub fn connect(phone: &AdbPhone) -> Option<(TcpStream, Forward)> {
    let out = run(
        &phone.exe,
        &["-s", &phone.serial, "forward", "tcp:0", SOCKET],
    )?;
    let forward = Forward {
        phone: phone.clone(),
        port: out.trim().parse().ok()?,
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, forward.port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    Some((stream, forward))
}

#[cfg(test)]
mod tests {
    use super::first_ready;

    #[test]
    fn picks_the_first_authorized_phone() {
        let out = "List of devices attached\nR5CY204BABT\tunauthorized\nemulator-5554\toffline\nR5CY999\tdevice\n\n";
        assert_eq!(first_ready(out).as_deref(), Some("R5CY999"));
        assert_eq!(first_ready("List of devices attached\n\n"), None);
    }
}
