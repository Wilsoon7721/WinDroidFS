use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use windows_sys::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const LATEST_RELEASE: &str = "https://api.github.com/repos/Wilsoon7721/WinDroidFS/releases/latest";

/// Where people can download it themselves.
pub const RELEASES_PAGE: &str = "https://github.com/Wilsoon7721/WinDroidFS/releases/latest";

/// What setup is called in a release.
const SETUP_PREFIX: &str = "WinDroidFS-Setup-";

/// Setup's switches for an update: no wizard pages (progress only), close and replace the running app. It starts the tray again afterwards (see installer/windroidfs.iss).
const SETUP_ARGS: &str = "/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS";

/// A newer release of the PC app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub page: String,
    pub setup: Option<Setup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub name: String,
    pub url: String,
    pub sha256: String,
}

/// The latest release if it's newer than this build
pub fn check() -> io::Result<Option<Update>> {
    let body = get(LATEST_RELEASE, "application/vnd.github+json")?;
    let release: serde_json::Value = serde_json::from_slice(&body).map_err(io::Error::other)?;
    Ok(newer_release(&release, env!("CARGO_PKG_VERSION")))
}

/// `release` (GitHub's JSON) as an update, if it's newer than `current`.
fn newer_release(release: &serde_json::Value, current: &str) -> Option<Update> {
    if release["draft"].as_bool() == Some(true) || release["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = release["tag_name"].as_str()?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if !is_newer(&version, current) {
        return None;
    }
    let setup = release["assets"].as_array().and_then(|assets| {
        assets.iter().find_map(|asset| {
            let name = asset["name"].as_str()?;
            if !(name.starts_with(SETUP_PREFIX) && name.ends_with(".exe")) {
                return None;
            }
            let sha256 = asset["digest"].as_str()?.strip_prefix("sha256:")?;
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Setup {
                name: name.to_string(),
                url: asset["browser_download_url"].as_str()?.to_string(),
                sha256: sha256.to_ascii_lowercase(),
            })
        })
    });
    Some(Update {
        page: release["html_url"]
            .as_str()
            .unwrap_or(RELEASES_PAGE)
            .to_string(),
        version,
        setup,
    })
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse(version: &str) -> Option<(u32, u32, u32)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    parts.next().is_none().then_some((major, minor, patch))
}

pub fn download(setup: &Setup) -> io::Result<PathBuf> {
    let bytes = get(&setup.url, "application/octet-stream")?;
    let actual = sha256_hex(&bytes);
    if actual != setup.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the download doesn't match the release (SHA-256 {actual}, expected {})",
                setup.sha256
            ),
        ));
    }
    let dir = std::env::temp_dir().join(format!("WinDroidFS-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let name = Path::new(&setup.name)
        .file_name()
        .ok_or_else(|| io::Error::other("the setup has no file name"))?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

pub fn install(setup: &Path) -> io::Result<()> {
    shell_open(&setup.to_string_lossy(), Some(SETUP_ARGS))
}

pub fn open_in_browser(url: &str) -> io::Result<()> {
    shell_open(url, None)
}

fn shell_open(file: &str, args: Option<&str>) -> io::Result<()> {
    let (verb, file, args) = (wide("open"), wide(file), args.map(wide));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = args.as_ref().map_or(std::ptr::null(), |a| a.as_ptr());
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Closes a WinHTTP handle when dropped
struct Handle(*mut std::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn open(handle: *mut std::ffi::c_void) -> io::Result<Handle> {
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(handle))
    }
}

fn check_ok(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// An HTTPS GET with WinHTTP, which uses Windows' own proxy settings and certificate store, and follows redirects (GitHub's downloads redirect to its file host).
fn get(url: &str, accept: &str) -> io::Result<Vec<u8>> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| io::Error::other("only https:// is fetched"))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let agent = wide(concat!("WinDroidFS/", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = open(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ))?;
        check_ok(WinHttpSetTimeouts(
            session.0, 10_000, 10_000, 30_000, 60_000,
        ))?;
        let connection = open(WinHttpConnect(
            session.0,
            wide(host).as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT as u16,
            0,
        ))?;
        let request = open(WinHttpOpenRequest(
            connection.0,
            wide("GET").as_ptr(),
            wide(&path).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let headers = wide(&format!("Accept: {accept}\r\n"));
        check_ok(WinHttpSendRequest(
            request.0,
            headers.as_ptr(),
            u32::MAX,
            std::ptr::null(),
            0,
            0,
            0,
        ))?;
        check_ok(WinHttpReceiveResponse(request.0, std::ptr::null_mut()))?;

        let mut status: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        check_ok(WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut size,
            std::ptr::null_mut(),
        ))?;
        if status != 200 {
            return Err(io::Error::other(format!(
                "{host} answered with HTTP {status}"
            )));
        }

        let mut body = Vec::new();
        loop {
            let mut available: u32 = 0;
            check_ok(WinHttpQueryDataAvailable(request.0, &mut available))?;
            if available == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read: u32 = 0;
            check_ok(WinHttpReadData(
                request.0,
                body[start..].as_mut_ptr().cast(),
                available,
                &mut read,
            ))?;
            body.truncate(start + read as usize);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str, assets: serde_json::Value) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/Wilsoon7721/WinDroidFS/releases/tag/{tag}"),
            "draft": false,
            "prerelease": false,
            "assets": assets,
        })
    }

    const DIGEST: &str = "9cd35f93f94d91211c5ba98b523468f5c4728497b5870c31f1a2ce24622dfd05";

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("1.2.0", "1.1.0"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(is_newer("2", "1.9.9"));
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(!is_newer("1.0.9", "1.1.0"));
        assert!(!is_newer("1.2.0-beta", "1.2.0"));
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn finds_setup_and_its_digest() {
        let latest = release(
            "v1.2.0",
            json!([
                {"name": "WinDroidFS-1.2.0.apk", "digest": format!("sha256:{DIGEST}"),
                 "browser_download_url": "https://example.invalid/apk"},
                {"name": "WinDroidFS-Setup-1.2.0.exe", "digest": format!("sha256:{}", DIGEST.to_uppercase()),
                 "browser_download_url": "https://example.invalid/setup"},
            ]),
        );
        let update = newer_release(&latest, "1.1.0").unwrap();
        assert_eq!(update.version, "1.2.0");
        assert_eq!(
            update.setup,
            Some(Setup {
                name: "WinDroidFS-Setup-1.2.0.exe".into(),
                url: "https://example.invalid/setup".into(),
                sha256: DIGEST.into(),
            })
        );
    }

    #[test]
    fn no_digest_means_by_hand_only() {
        let latest = release(
            "v1.2.0",
            json!([{"name": "WinDroidFS-Setup-1.2.0.exe", "browser_download_url": "https://example.invalid/setup"}]),
        );
        let update = newer_release(&latest, "1.1.0").unwrap();
        assert_eq!(update.setup, None);
        assert!(update.page.ends_with("/v1.2.0"));
    }

    #[test]
    fn ignores_older_drafts_and_prereleases() {
        assert_eq!(newer_release(&release("v1.0.0", json!([])), "1.1.0"), None);
        let mut pre = release("v2.0.0", json!([]));
        pre["prerelease"] = json!(true);
        assert_eq!(newer_release(&pre, "1.1.0"), None);
    }

    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Asks GitHub for real: `cargo test updater::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live() {
        let body = get(LATEST_RELEASE, "application/vnd.github+json").unwrap();
        let release: serde_json::Value = serde_json::from_slice(&body).unwrap();
        println!("latest: {}", release["tag_name"]);
        let update = newer_release(&release, "0.9.0").unwrap();
        println!("as an update to 0.9.0: {update:#?}");
        // Follows GitHub's redirect to its file host, and checks the digest.
        let path = download(update.setup.as_ref().unwrap()).unwrap();
        println!(
            "downloaded and verified: {} ({} bytes)",
            path.display(),
            std::fs::metadata(&path).unwrap().len()
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
