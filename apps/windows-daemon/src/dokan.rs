//! Finding Dokan - which stops programs that need administrator rights (installers) from running off the phone.

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, FILETIME};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RRF_RT_REG_SZ,
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW,
};

const PROVIDER_ORDER: &str = r"SYSTEM\CurrentControlSet\Control\NetworkProvider\Order";
const UNINSTALL_KEYS: [(HKEY, &str); 3] = [
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
    (
        HKEY_CURRENT_USER,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
];

/// Estimating similar install times
const SAME_INSTALL: u64 = 2 * 60 * 10_000_000; // 2 min in nanosecs

/// Dokan's network provider
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dokan {
    /// When Dokan was installed if recorded
    pub installed_on: Option<String>,
    /// The programs that may have brought it, or that most likely did (see `sure`).
    pub installed_by: Vec<String>,
    pub sure: bool,
}

impl Dokan {
    /// What to tell the user - with the phone mounted on `drive`
    pub fn notice(&self, drive: char) -> String {
        let names = self.installed_by.join(" or ");
        let origin = match (self.installed_by.is_empty(), self.sure, &self.installed_on) {
            (true, _, Some(date)) => format!(", a driver another program installed on {date}"),
            (true, _, None) => ", a driver another program installed".to_string(),
            (false, true, Some(date)) => format!(", a driver {names} installed on {date}"),
            (false, true, None) => format!(", a driver {names} installed"),
            (false, false, Some(date)) => {
                format!(", a driver another program installed, most likely {names} on {date}")
            }
            (false, false, None) => {
                format!(", a driver another program installed, most likely {names}")
            }
        };
        let culprit = match self.installed_by.as_slice() {
            [] => "the program that installed it".to_string(),
            _ => names,
        };
        format!(
            "Installers on {drive}: may not run when you double-click them.\n\nThis PC has Dokan{origin}. Dokan claims every network drive, including the phone's, so Explorer can't start programs that need administrator rights from the phone.\n\n\
             Everything else works. To run an installer, copy it to this PC first. If you no longer use {culprit}, uninstalling it and \"Dokan Library\" fixes this.

             This was assumed based on installation timestamps and may not correctly reflect the actual installer of Dokan."
        )
    }
}

/// Whether Dokan's network provider is in Windows' list
pub fn find() -> Option<Dokan> {
    let order = read_string(HKEY_LOCAL_MACHINE, PROVIDER_ORDER, "ProviderOrder")?;
    let has_dokan = order.split(',').any(|name| {
        read_string(
            HKEY_LOCAL_MACHINE,
            &format!(
                r"SYSTEM\CurrentControlSet\Services\{}\NetworkProvider",
                name.trim()
            ),
            "ProviderPath",
        )
        .is_some_and(|path| is_dokan_provider(&path))
    });
    has_dokan.then(|| origin(&installed_programs()))
}

/// When Dokan was installed and what brought it
fn origin(programs: &[Program]) -> Dokan {
    let dokan: Vec<&Program> = programs.iter().filter(|p| is_dokan_name(&p.name)).collect();
    let date = dokan.iter().find_map(|p| p.date.clone());
    let times: Vec<u64> = dokan.iter().filter_map(|p| p.written).collect();
    let same_install = candidates(programs, |p| {
        p.written
            .is_some_and(|w| times.iter().any(|&t| w.abs_diff(t) <= SAME_INSTALL))
    });
    if !same_install.is_empty() {
        return Dokan {
            installed_on: date.as_deref().and_then(readable_date),
            installed_by: same_install,
            sure: true,
        };
    }

    let same_day = date
        .as_deref()
        .map(|d| candidates(programs, |p| p.date.as_deref() == Some(d)))
        .unwrap_or_default();
    Dokan {
        installed_on: date.as_deref().and_then(readable_date),
        installed_by: same_day,
        sure: false,
    }
}

fn is_dokan_provider(path: &str) -> bool {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.to_ascii_lowercase().starts_with("dokannp")
}

fn is_dokan_name(name: &str) -> bool {
    name.to_ascii_lowercase().contains("dokan")
}

#[derive(Clone, Debug)]
struct Program {
    name: String,
    publisher: String,
    /// YYYYMMDD as uninstall entry writes
    date: Option<String>,
    /// When the uninstall entry was last written
    written: Option<u64>,
}

fn candidates(programs: &[Program], keep: impl Fn(&Program) -> bool) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for program in programs {
        let microsoft =
            program.publisher.starts_with("Microsoft") || program.name.starts_with("Microsoft");
        if microsoft || is_dokan_name(&program.name) || !keep(program) {
            continue;
        }
        let name = program
            .name
            .trim_end_matches(" (x64)")
            .trim_end_matches(" (x86)")
            .trim()
            .to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// "20260510" as "10 May 2026".
fn readable_date(date: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month: usize = date[4..6].parse().ok()?;
    let day: u32 = date[6..8].parse().ok()?;
    let name = MONTHS.get(month.checked_sub(1)?)?;
    Some(format!("{day} {name} {}", &date[..4]))
}

fn installed_programs() -> Vec<Program> {
    let mut programs = Vec::new();
    for (root, path) in UNINSTALL_KEYS {
        for key in subkeys(root, path) {
            let entry = format!(r"{path}\{key}");
            let Some(name) = read_string(root, &entry, "DisplayName") else {
                continue;
            };
            programs.push(Program {
                name,
                publisher: read_string(root, &entry, "Publisher").unwrap_or_default(),
                date: read_string(root, &entry, "InstallDate"),
                written: last_written(root, &entry),
            });
        }
    }
    programs
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A REG_SZ value
fn read_string(root: HKEY, path: &str, value: &str) -> Option<String> {
    let (path, value) = (wide(path), wide(value));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            path.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | 0x0001_0000, // RRF_SUBKEY_WOW6464KEY
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// When the key at `path` was last written - returned as a FILETIME
fn last_written(root: HKEY, path: &str) -> Option<u64> {
    let path = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut key) }
        != ERROR_SUCCESS
    {
        return None;
    }
    let mut time = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let status = unsafe {
        RegQueryInfoKeyW(
            key,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut time,
        )
    };
    unsafe { RegCloseKey(key) };
    (status == ERROR_SUCCESS)
        .then(|| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime))
}

fn subkeys(root: HKEY, path: &str) -> Vec<String> {
    let path = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut key) }
        != ERROR_SUCCESS
    {
        return Vec::new();
    }
    let mut names = Vec::new();
    for index in 0.. {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                buf.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    unsafe { RegCloseKey(key) };
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = 60 * 10_000_000;

    fn program(name: &str, publisher: &str, date: &str) -> Program {
        Program {
            name: name.into(),
            publisher: publisher.into(),
            date: Some(date.into()),
            written: None,
        }
    }

    fn at(mut program: Program, minute: u64) -> Program {
        program.written = Some(minute * MINUTE);
        program
    }

    #[test]
    fn names_what_was_installed_with_it() {
        let programs = [
            at(
                program(
                    "Dokan Library 2.3.1.1000 Bundle",
                    "Dokany Project",
                    "20260510",
                ),
                1000,
            ),
            at(
                program("Disk Drill 6.2.1278.0", "CleverFiles", "20260510"),
                1001,
            ),
            at(
                program("Disk Drill 6.2.1278.0 (x64)", "CleverFiles", "20260510"),
                1001,
            ),
            // Installed later that day, so not the one.
            at(program("VLC media player", "VideoLAN", "20260510"), 1300),
        ];
        let dokan = origin(&programs);
        assert!(dokan.sure);
        assert_eq!(dokan.installed_by, ["Disk Drill 6.2.1278.0"]);
        assert_eq!(dokan.installed_on.as_deref(), Some("10 May 2026"));
    }

    #[test]
    fn falls_back_to_the_same_day() {
        let programs = [
            at(
                program(
                    "Dokan Library 2.3.1.1000 Bundle",
                    "Dokany Project",
                    "20260510",
                ),
                1000,
            ),
            at(
                program("Disk Drill 6.2.1278.0", "CleverFiles", "20260510"),
                9000,
            ),
        ];
        let dokan = origin(&programs);
        assert!(!dokan.sure);
        assert_eq!(dokan.installed_by, ["Disk Drill 6.2.1278.0"]);
    }

    #[test]
    #[ignore]
    fn this_pc() {
        println!("{:#?}", origin(&installed_programs()));
    }

    #[test]
    fn recognises_dokans_provider() {
        assert!(is_dokan_provider(r"C:\Windows\System32\dokannp2.dll"));
        assert!(is_dokan_provider(r"%SystemRoot%\System32\DokanNP1.dll"));
        assert!(!is_dokan_provider(
            r"C:\Program Files (x86)\WinFsp\bin\winfsp-x64.dll"
        ));
        assert!(!is_dokan_provider(r"C:\Windows\System32\ntlanman.dll"));
    }

    #[test]
    fn names_what_came_the_same_day() {
        let programs = [
            program(
                "Dokan Library 2.3.1.1000 (x64)",
                "Dokany Project",
                "20260510",
            ),
            program(
                "Dokan Library 2.3.1.1000 Bundle",
                "Dokany Project",
                "20260510",
            ),
            program("Disk Drill 6.2.1278.0", "CleverFiles", "20260510"),
            program("Disk Drill 6.2.1278.0 (x64)", "CleverFiles", "20260510"),
            program(
                "Microsoft .NET Runtime - 9.0.12 (x64)",
                "Microsoft Corporation",
                "20260510",
            ),
            program("7-Zip 24.08 (x64)", "Igor Pavlov", "20260101"),
        ];
        assert_eq!(
            candidates(&programs, |p| p.date.as_deref() == Some("20260510")),
            ["Disk Drill 6.2.1278.0"]
        );
    }

    #[test]
    fn names_the_culprit_when_known() {
        let mut known = Dokan {
            installed_on: Some("10 May 2026".into()),
            installed_by: vec!["Disk Drill 6.2.1278.0".into()],
            sure: true,
        };
        let text = known.notice('Z');
        assert!(text.starts_with("Installers on Z: may not run"));
        assert!(text.contains(
            "This PC has Dokan, a driver Disk Drill 6.2.1278.0 installed on 10 May 2026."
        ));
        assert!(text.contains("If you no longer use Disk Drill 6.2.1278.0, uninstalling it"));

        known.sure = false;
        assert!(known.notice('Z').contains(
            "a driver another program installed, most likely Disk Drill 6.2.1278.0 on 10 May 2026."
        ));

        let unknown = Dokan {
            installed_on: None,
            installed_by: vec![],
            sure: false,
        };
        let text = unknown.notice('Y');
        assert!(text.contains("another program installed. Dokan"));
        assert!(text.contains("If you no longer use the program that installed it"));
    }

    #[test]
    fn writes_dates_for_people() {
        assert_eq!(readable_date("20260510").as_deref(), Some("10 May 2026"));
        assert_eq!(readable_date("20261301"), None);
        assert_eq!(readable_date("2026-05-10"), None);
        assert_eq!(readable_date(""), None);
    }
}
