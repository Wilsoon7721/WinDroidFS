use tauri_winrt_notification::{Duration, Toast};

/// Must match `AppUserModelID` on the Start menu shortcut in installer/windroidfs.iss.
pub const APP_ID: &str = "WilsonOon.WinDroidFS";

pub fn show(title: &str, text: &str) {
    let app_id = if cfg!(debug_assertions) {
        Toast::POWERSHELL_APP_ID
    } else {
        APP_ID
    };
    let (title, text) = (title.to_string(), text.to_string());
    std::thread::spawn(move || {
        let shown = Toast::new(app_id)
            .title(&title)
            .text1(&text)
            .duration(Duration::Short)
            .show();
        if let Err(e) = shown {
            log::warn!("⚠️  Couldn't show a notification: {e:?}");
        }
    });
}
