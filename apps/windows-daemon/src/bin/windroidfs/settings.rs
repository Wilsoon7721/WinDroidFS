//! The tray app's settings are kept in `%APPDATA%\WinDroidFS\settings.ini` as `key=value` lines.

use std::path::PathBuf;

use windroidfs::session::DriveChoice;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Mount phones when they're plugged in. Off leaves phones alone (MTP works as usual).
    pub enabled: bool,
    pub drive: DriveChoice,
    /// Open the drive in Explorer once it's mounted, like a USB stick.
    pub open_on_mount: bool,
    /// Mount without write access.
    pub read_only: bool,
    /// Show dot-files (`.thumbnails`, `.nomedia`, ...) as hidden as the phone does.
    pub hide_dotfiles: bool,
    /// Use the WebDAV mount even when WinFsp is installed. For troubleshooting.
    pub use_webdav: bool,
    /// Keep a log file (see `windroidfs::logging`).
    pub log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            drive: DriveChoice::Auto,
            open_on_mount: true,
            read_only: false,
            hide_dotfiles: true,
            use_webdav: false,
            log: true,
        }
    }
}

impl Settings {
    /// The saved settings, and whether there were any.
    pub fn load() -> (Settings, bool) {
        match path().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) => (Settings::parse(&text), true),
            None => (Settings::default(), false),
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, self.to_text()) {
            log::warn!("⚠️  Couldn't save settings to {}: {e}", path.display());
        }
    }

    fn parse(text: &str) -> Settings {
        let mut settings = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            // A missing or unreadable value keeps the default.
            let flag = |default: bool| match value {
                "true" => true,
                "false" => false,
                _ => default,
            };
            match key.trim() {
                "enabled" => settings.enabled = flag(settings.enabled),
                "drive" => settings.drive = parse_drive(value),
                "open_on_mount" => settings.open_on_mount = flag(settings.open_on_mount),
                "read_only" => settings.read_only = flag(settings.read_only),
                "hide_dotfiles" => settings.hide_dotfiles = flag(settings.hide_dotfiles),
                "use_webdav" => settings.use_webdav = flag(settings.use_webdav),
                "log" => settings.log = flag(settings.log),
                _ => {}
            }
        }
        settings
    }

    fn to_text(&self) -> String {
        let drive = match self.drive {
            DriveChoice::Auto => "auto".to_string(),
            DriveChoice::Letter(letter) => letter.to_string(),
        };
        format!(
            "enabled={}\ndrive={drive}\nopen_on_mount={}\nread_only={}\nhide_dotfiles={}\nuse_webdav={}\nlog={}\n",
            self.enabled,
            self.open_on_mount,
            self.read_only,
            self.hide_dotfiles,
            self.use_webdav,
            self.log,
        )
    }
}

/// "Z" (or "z", or "Z:") is that letter. Anything else is automatic.
fn parse_drive(value: &str) -> DriveChoice {
    let mut chars = value.trim_end_matches(':').chars();
    match (chars.next(), chars.next()) {
        (Some(letter), None) if ('D'..='Z').contains(&letter.to_ascii_uppercase()) => {
            DriveChoice::Letter(letter.to_ascii_uppercase())
        }
        _ => DriveChoice::Auto,
    }
}

fn path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("WinDroidFS")
            .join("settings.ini"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for settings in [
            Settings::default(),
            Settings {
                enabled: false,
                drive: DriveChoice::Letter('P'),
                open_on_mount: false,
                read_only: true,
                hide_dotfiles: false,
                use_webdav: true,
                log: false,
            },
        ] {
            assert_eq!(Settings::parse(&settings.to_text()), settings);
        }
    }

    #[test]
    fn tolerates_junk() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::parse("nonsense\n=\ndrive=Q:\nread_only=yes please\n"),
            Settings {
                drive: DriveChoice::Letter('Q'),
                ..Settings::default()
            }
        );

        assert_eq!(parse_drive("C"), DriveChoice::Auto);
        assert_eq!(parse_drive("ZZ"), DriveChoice::Auto);
        assert_eq!(parse_drive("z"), DriveChoice::Letter('Z'));
    }

    #[test]
    fn an_older_file_keeps_the_new_defaults() {
        let settings = Settings::parse("enabled=false\ndrive=auto\n");
        assert!(!settings.enabled);
        assert!(settings.open_on_mount && settings.hide_dotfiles && settings.log);
    }
}
