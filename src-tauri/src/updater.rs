//! Plouk updating itself. Every release carries a `latest.json` naming the newest
//! version. On computers, Tauri's updater downloads the build for this install
//! and checks it against Plouk's update key before touching anything. An
//! AppImage or Mac app is swapped in place right away and the next launch is the
//! new version; a .deb needs the system password and the Windows installer
//! closes Plouk, so those wait for the user to say when. On Android the new APK
//! is downloaded and handed to the system installer, which only accepts it if
//! it's signed with the same key.

use serde::Serialize;
#[cfg(target_os = "android")]
use tauri::AppHandle;

/// Same file as `plugins.updater.endpoints` in tauri.conf.json, which the desktop updater reads.
#[cfg(target_os = "android")]
const MANIFEST: &str = "https://github.com/wasselnl77-hash/plouk/releases/latest/download/latest.json";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub version: String,
    /// Already swapped in; restarting finishes it.
    pub installed: bool,
}

#[cfg(desktop)]
pub use desktop::*;

#[cfg(desktop)]
mod desktop {
    use std::sync::Mutex;

    use tauri::{AppHandle, Manager};
    use tauri_plugin_updater::{Update, UpdaterExt};

    use super::UpdateStatus;

    /// A downloaded update that still needs installing (the .deb case).
    #[derive(Default)]
    pub struct Pending(Mutex<Option<(Update, Vec<u8>)>>);

    pub async fn check(app: &AppHandle) -> Result<Option<UpdateStatus>, String> {
        // A bare binary (install.sh, cargo run) doesn't know what it was installed from.
        #[cfg(target_os = "linux")]
        if tauri::utils::platform::bundle_type().is_none() {
            return Ok(None);
        }
        let updater = app.updater().map_err(|e| e.to_string())?;
        let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
        let version = update.version.clone();
        // An AppImage or Mac app can be swapped while running. A .deb needs the password and the
        // Windows installer closes Plouk, so those wait for the user.
        let in_place = cfg!(target_os = "macos")
            || matches!(tauri::utils::platform::bundle_type(), Some(tauri::utils::config::BundleType::AppImage));
        if in_place {
            update.install(&bytes).map_err(|e| e.to_string())?;
        } else {
            *app.state::<Pending>().0.lock().unwrap() = Some((update, bytes));
        }
        Ok(Some(UpdateStatus { version, installed: in_place }))
    }

    /// Installs a waiting .deb (this is where the password prompt shows up), then restarts.
    pub fn apply(app: &AppHandle) -> Result<(), String> {
        if let Some((update, bytes)) = app.state::<Pending>().0.lock().unwrap().take() {
            update.install(&bytes).map_err(|e| e.to_string())?;
        }
        app.restart();
    }
}

#[cfg(target_os = "android")]
pub async fn check(app: &AppHandle) -> Result<Option<UpdateStatus>, String> {
    let current = app.package_info().version.to_string();
    let version = crate::android::check_update(MANIFEST, &current).await?;
    Ok(version.map(|version| UpdateStatus { version, installed: false }))
}

#[cfg(target_os = "android")]
pub fn apply(_app: &AppHandle) -> Result<(), String> {
    crate::android::install_update()
}
