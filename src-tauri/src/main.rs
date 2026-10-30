#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod decode;
mod loudness;
mod lyrics;
mod media;
mod resolver;
mod snapshot;
mod spotify;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use audio::{Cmd, Engine, Loaded};
use media::{Media, MediaCmd, MediaKey};
use resolver::{CacheInfo, EngineState, Prepared, Resolver, TrackReq};
use serde::Serialize;
use serde_json::Value;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tokio::sync::oneshot;

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// A Spotify link passed on the command line (`plouk <link>`), waiting for the page to pick it up.
struct LaunchUri(Mutex<Option<String>>);

fn spotify_arg(args: &[String]) -> Option<String> {
    args.iter().skip(1).find(|a| !spotify::parse_refs(a).is_empty()).cloned()
}

#[tauri::command]
fn take_launch_uri(launch: State<'_, LaunchUri>) -> Option<String> {
    launch.0.lock().unwrap().take()
}

// ------------------------------------------------------------- options

struct Prefs {
    close_to_tray: AtomicBool,
    /// Started with `--hidden` (start at login, straight to the tray).
    hidden_launch: bool,
}

#[tauri::command]
fn launch_hidden(prefs: State<'_, Prefs>) -> bool {
    prefs.hidden_launch
}

#[tauri::command]
fn set_close_to_tray(prefs: State<'_, Prefs>, on: bool) {
    prefs.close_to_tray.store(on, Ordering::Relaxed);
}

#[derive(Serialize)]
struct Autostart {
    enabled: bool,
    hidden: bool,
}

fn autostart_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().config_dir().map_err(|e| e.to_string())?.join("autostart").join("plouk.desktop"))
}

#[tauri::command]
fn autostart_state(app: AppHandle) -> Autostart {
    let text = autostart_file(&app).ok().and_then(|f| std::fs::read_to_string(f).ok());
    Autostart { enabled: text.is_some(), hidden: text.is_some_and(|t| t.contains("--hidden")) }
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool, hidden: bool) -> Result<(), String> {
    let file = autostart_file(&app)?;
    if !enabled {
        if file.exists() {
            std::fs::remove_file(&file).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let icon = app.path().data_dir().map(|d| d.join("icons/hicolor/512x512/apps/plouk.png")).ok().filter(|p| p.exists());
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Plouk\nComment=Dark, minimal vinyl music player\nExec=\"{}\"{}\n{}Terminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display(),
        if hidden { " --hidden" } else { "" },
        icon.map(|i| format!("Icon={}\n", i.display())).unwrap_or_default(),
    );
    std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&file, entry).map_err(|e| e.to_string())
}

#[tauri::command]
fn cache_info(resolver: State<'_, Arc<Resolver>>) -> CacheInfo {
    resolver.cache_info()
}

#[tauri::command]
fn clear_cache(resolver: State<'_, Arc<Resolver>>) -> u64 {
    resolver.clear_cache()
}

#[tauri::command]
fn set_cache_limit(resolver: State<'_, Arc<Resolver>>, bytes: u64) {
    resolver.set_cache_limit(bytes);
}

#[tauri::command]
fn open_cache_dir(resolver: State<'_, Arc<Resolver>>) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(resolver.cache_dir()).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_engine(app: AppHandle, resolver: State<'_, Arc<Resolver>>) -> Result<String, String> {
    resolver.update_now(&app).await
}

/// Plouk's own window as raw pixels, for the options warp.
#[tauri::command]
async fn capture_window() -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(snapshot::capture)
        .await
        .map_err(|e| e.to_string())?
        .map(tauri::ipc::Response::new)
}

#[derive(Serialize)]
struct AppInfo {
    version: String,
    engine: EngineState,
}

#[tauri::command]
fn app_info(app: AppHandle, resolver: State<'_, Arc<Resolver>>) -> AppInfo {
    AppInfo { version: app.package_info().version.to_string(), engine: resolver.state() }
}

// ------------------------------------------------------------- spotify & youtube

#[tauri::command]
async fn fetch_collection(input: String) -> Result<spotify::Collection, String> {
    spotify::fetch(&input).await
}

#[tauri::command]
async fn track_art(id: String) -> Result<Option<String>, String> {
    spotify::track_art(&id).await
}

#[tauri::command]
async fn prepare_track(resolver: State<'_, Arc<Resolver>>, track: TrackReq) -> Result<Prepared, String> {
    resolver.inner().clone().prepare(track).await
}

#[tauri::command]
fn engine_state(resolver: State<'_, Arc<Resolver>>) -> EngineState {
    resolver.state()
}

#[tauri::command]
async fn lyrics(title: String, artist: String, duration: f64) -> Result<lyrics::Lyrics, String> {
    lyrics::find(&title, &artist, duration).await
}

// ------------------------------------------------------------- audio

async fn ask(engine: &Engine, make: impl FnOnce(oneshot::Sender<Result<Loaded, String>>) -> Cmd) -> Result<Loaded, String> {
    let (tx, rx) = oneshot::channel();
    engine.send(make(tx));
    rx.await.map_err(|_| "Audio engine stopped".to_string())?
}

#[tauri::command]
async fn audio_load(engine: State<'_, Engine>, path: String, start: f64, autoplay: bool) -> Result<Loaded, String> {
    ask(&engine, |reply| Cmd::Load { path, start, autoplay, reply }).await
}

#[tauri::command]
async fn audio_crossfade(engine: State<'_, Engine>, path: String, secs: f64) -> Result<Loaded, String> {
    ask(&engine, |reply| Cmd::Crossfade { path, secs, reply }).await
}

#[tauri::command]
fn audio_play(engine: State<'_, Engine>) {
    engine.send(Cmd::Play);
}

#[tauri::command]
fn audio_pause(engine: State<'_, Engine>) {
    engine.send(Cmd::Pause);
}

#[tauri::command]
fn audio_seek(engine: State<'_, Engine>, secs: f64) {
    engine.send(Cmd::Seek(secs));
}

#[tauri::command]
fn audio_volume(engine: State<'_, Engine>, volume: f32) {
    engine.send(Cmd::Volume(volume));
}

#[tauri::command]
fn audio_stop(engine: State<'_, Engine>) {
    engine.send(Cmd::Stop);
}

#[tauri::command]
fn audio_normalize(engine: State<'_, Engine>, on: bool) {
    engine.send(Cmd::Normalize(on));
}

// ------------------------------------------------------------- system

#[tauri::command]
fn media_meta(media: State<'_, Media>, title: String, artist: String, album: String, cover: Option<String>, duration: f64) {
    media.send(MediaCmd::Meta { title, artist, album, cover, duration });
}

#[tauri::command]
fn media_playback(media: State<'_, Media>, playing: bool, pos: f64) {
    media.send(MediaCmd::Playback { playing, pos });
}

#[tauri::command]
fn media_volume(media: State<'_, Media>, volume: f64) {
    media.send(MediaCmd::Volume(volume));
}

#[tauri::command]
fn load_state(app: AppHandle) -> Option<Value> {
    let path = app.path().app_data_dir().ok()?.join("state.json");
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[tauri::command]
fn save_state(app: AppHandle, state: Value) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("state.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&state).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, dir.join("state.json")).map_err(|e| e.to_string())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Plouk", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Play / Pause", true, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "Next", true, None::<&str>)?;
    let prev = MenuItem::with_id(app, "prev", "Previous", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Plouk", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &PredefinedMenuItem::separator(app)?, &toggle, &next, &prev, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("plouk")
        .icon(icon)
        .tooltip("Plouk")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            action => {
                let _ = app.emit("media-key", MediaKey::new(action));
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Launching Plouk again with a link plays it in the running window.
            if let Some(uri) = spotify_arg(&args) {
                let _ = app.emit("media-key", MediaKey { uri: Some(uri), ..MediaKey::new("open") });
            }
            show_main(app);
        }))
        .manage(LaunchUri(Mutex::new(spotify_arg(&std::env::args().collect::<Vec<_>>()))))
        .manage(Prefs { close_to_tray: AtomicBool::new(true), hidden_launch: std::env::args().any(|a| a == "--hidden") })
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(Engine::spawn(handle.clone()));
            app.manage(Media::spawn(handle.clone()));
            let resolver = Resolver::new(&handle)?;
            app.manage(resolver.clone());
            tauri::async_runtime::spawn(resolver.ensure(handle.clone()));
            if let Err(e) = build_tray(app) {
                eprintln!("[plouk] tray unavailable: {e}");
            }
            // The page shows the window once it has painted; this is only a safety net.
            let hidden = app.state::<Prefs>().hidden_launch;
            tauri::async_runtime::spawn(async move {
                if hidden {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(4)).await;
                if let Some(w) = handle.get_webview_window("main") {
                    if !w.is_visible().unwrap_or(true) {
                        let _ = w.show();
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides to the tray (music keeps playing) unless the user chose "Quit".
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.state::<Prefs>().close_to_tray.load(Ordering::Relaxed) {
                    let _ = window.hide();
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            take_launch_uri,
            fetch_collection,
            track_art,
            prepare_track,
            engine_state,
            lyrics,
            audio_load,
            audio_crossfade,
            audio_play,
            audio_pause,
            audio_seek,
            audio_volume,
            audio_stop,
            media_meta,
            media_playback,
            media_volume,
            load_state,
            save_state,
            quit_app,
            launch_hidden,
            set_close_to_tray,
            autostart_state,
            set_autostart,
            cache_info,
            clear_cache,
            set_cache_limit,
            open_cache_dir,
            update_engine,
            app_info,
            audio_normalize,
            capture_window,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Plouk");
}
