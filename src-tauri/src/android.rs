//! Android bridge. The Kotlin half (PloukPlugin.kt) finds songs with the
//! NewPipe extractor and downloads their audio, runs the media session
//! (notification, lock screen, headset buttons) and receives "Share → Plouk"
//! from the Spotify app. Calls into Kotlin block until it answers, so they
//! always run off the main thread.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    plugin::{Builder, PluginHandle, TauriPlugin},
    AppHandle, Emitter, Wry,
};

use crate::{
    media::{MediaCmd, MediaKey},
    resolver::TrackReq,
};

static PLUGIN: OnceLock<PluginHandle<Wry>> = OnceLock::new();

#[derive(Deserialize)]
struct NativeEvent {
    kind: String,
    action: Option<String>,
    value: Option<f64>,
    text: Option<String>,
}

pub fn init() -> TauriPlugin<Wry> {
    Builder::<Wry, ()>::new("plouk")
        .setup(|app, api| {
            let handle = api.register_android_plugin("com.plouk.player", "PloukPlugin")?;
            let events = app.clone();
            let channel: Channel<Value> = Channel::new(move |body| {
                if let InvokeResponseBody::Json(raw) = body {
                    if let Ok(ev) = serde_json::from_str::<NativeEvent>(&raw) {
                        on_native_event(&events, ev);
                    }
                }
                Ok(())
            });
            let wire = handle.clone();
            std::thread::spawn(move || {
                if let Err(e) = wire.run_mobile_plugin::<Value>("setEventChannel", json!({ "channel": channel })) {
                    eprintln!("[plouk] native events unavailable: {e}");
                }
            });
            let _ = PLUGIN.set(handle);
            Ok(())
        })
        .build()
}

fn on_native_event(app: &AppHandle, ev: NativeEvent) {
    let key = match ev.kind.as_str() {
        "media-key" => MediaKey { action: ev.action.unwrap_or_default(), value: ev.value, uri: None },
        "shared" => MediaKey { action: "open".into(), value: None, uri: ev.text },
        _ => return,
    };
    let _ = app.emit("media-key", key);
}

fn plugin() -> Result<&'static PluginHandle<Wry>, String> {
    PLUGIN.get().ok_or_else(|| "Android engine isn't ready".to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FetchArgs {
    title: String,
    artist: String,
    duration_ms: u64,
    dest: String,
}

#[derive(Deserialize)]
struct FetchResult {
    path: String,
}

/// Finds the song on YouTube Music (NewPipe) and downloads its audio into the cache.
pub async fn fetch(t: &TrackReq, cache_dir: &Path) -> Result<PathBuf, String> {
    let artist = t.artist.split(',').next().unwrap_or(&t.artist).trim().to_string();
    let args = FetchArgs {
        title: t.title.clone(),
        artist,
        duration_ms: t.duration_ms,
        dest: cache_dir.join(format!("{}.m4a", t.id)).to_string_lossy().into(),
    };
    let handle = plugin()?;
    let res = tauri::async_runtime::spawn_blocking(move || handle.run_mobile_plugin::<FetchResult>("fetch", args))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let path = PathBuf::from(res.path);
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(format!("Couldn't download “{}”", t.title));
    }
    Ok(path)
}

/// Mirrors what's playing into the Android media session.
pub fn media(_app: &AppHandle, cmd: MediaCmd) {
    let (command, payload) = match cmd {
        MediaCmd::Meta { title, artist, album, cover, duration } => (
            "setMetadata",
            json!({ "title": title, "artist": artist, "album": album, "cover": cover, "durationMs": (duration * 1000.0) as i64 }),
        ),
        MediaCmd::Playback { playing, pos } => ("setPlayback", json!({ "playing": playing, "positionMs": (pos * 1000.0) as i64 })),
        MediaCmd::Volume(_) => return,
    };
    let Ok(handle) = plugin() else { return };
    std::thread::spawn(move || {
        if let Err(e) = handle.run_mobile_plugin::<Value>(command, payload) {
            eprintln!("[plouk] media session: {e}");
        }
    });
}

#[derive(Deserialize)]
struct Shared {
    text: Option<String>,
}

/// A link shared to Plouk before the page was ready (cold start from "Share → Plouk").
pub async fn take_shared() -> Option<String> {
    let handle = plugin().ok()?;
    tauri::async_runtime::spawn_blocking(move || handle.run_mobile_plugin::<Shared>("takeShared", json!({})).ok())
        .await
        .ok()
        .flatten()
        .and_then(|s| s.text)
}

#[derive(Deserialize)]
struct UpdateFound {
    version: Option<String>,
}

/// Downloads a newer APK if latest.json names one; returns its version.
pub async fn check_update(manifest: &str, current: &str) -> Result<Option<String>, String> {
    let handle = plugin()?;
    let args = json!({ "manifest": manifest, "current": current });
    tauri::async_runtime::spawn_blocking(move || handle.run_mobile_plugin::<UpdateFound>("checkUpdate", args))
        .await
        .map_err(|e| e.to_string())?
        .map(|f| f.version)
        .map_err(|e| e.to_string())
}

/// Opens the system installer on the downloaded APK.
pub fn install_update() -> Result<(), String> {
    let handle = plugin()?;
    std::thread::spawn(move || {
        let _ = handle.run_mobile_plugin::<Value>("installUpdate", json!({}));
    });
    Ok(())
}
