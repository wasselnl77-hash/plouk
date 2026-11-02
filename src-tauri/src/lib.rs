mod audio;
mod decode;
mod lastfm;
mod loudness;
mod lyrics;
mod media;
mod pitch;
mod resolver;
mod snapshot;
mod spotify;
mod updater;

#[cfg(target_os = "android")]
mod android;
#[cfg(desktop)]
mod discord;
#[cfg(desktop)]
mod ytdlp;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use audio::{Cmd, Engine, Loaded};
use media::{Media, MediaCmd};
use resolver::{CacheInfo, EngineState, Prepared, Resolver, TrackReq};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        #[cfg(desktop)]
        {
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }
}

/// A Spotify link handed to Plouk at launch (`plouk <link>`), waiting for the page to pick it up.
struct LaunchUri(Mutex<Option<String>>);

#[cfg(desktop)]
fn spotify_arg(args: &[String]) -> Option<String> {
    args.iter().skip(1).find(|a| !spotify::parse_refs(a).is_empty()).cloned()
}

#[tauri::command]
async fn take_launch_uri(launch: State<'_, LaunchUri>) -> Result<Option<String>, String> {
    #[cfg(target_os = "android")]
    if let Some(shared) = android::take_shared().await {
        return Ok(Some(shared));
    }
    Ok(launch.0.lock().unwrap().take())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Platform {
    os: &'static str,
    mobile: bool,
}

#[tauri::command]
fn platform() -> Platform {
    Platform { os: std::env::consts::OS, mobile: cfg!(mobile) }
}

// ------------------------------------------------------------- options

struct Prefs {
    close_to_tray: AtomicBool,
    /// Started by "start at login" (`--autostarted`, or `--hidden` from older versions).
    autostarted: bool,
}

#[tauri::command]
fn launch_hidden(prefs: State<'_, Prefs>) -> bool {
    prefs.autostarted
}

#[tauri::command]
fn set_close_to_tray(prefs: State<'_, Prefs>, on: bool) {
    prefs.close_to_tray.store(on, Ordering::Relaxed);
}

#[derive(Serialize)]
struct Autostart {
    enabled: bool,
    supported: bool,
}

#[tauri::command]
fn autostart_state(app: AppHandle) -> Autostart {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        Autostart { enabled: app.autolaunch().is_enabled().unwrap_or(false), supported: true }
    }
    #[cfg(mobile)]
    {
        let _ = app;
        Autostart { enabled: false, supported: false }
    }
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        if enabled { launcher.enable() } else { launcher.disable() }.map_err(|e| e.to_string())
    }
    #[cfg(mobile)]
    {
        let _ = (app, enabled);
        Err("Starting at login isn't available on this device".into())
    }
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
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(mobile) {
        return Err("Not available on this device".into());
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(resolver.cache_dir()).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_engine(app: AppHandle, resolver: State<'_, Arc<Resolver>>) -> Result<String, String> {
    resolver.update_now(&app).await
}

/// Looks for a newer Plouk and downloads it (see updater.rs). None when up to date.
#[tauri::command]
async fn update_check(app: AppHandle) -> Result<Option<updater::UpdateStatus>, String> {
    updater::check(&app).await
}

#[tauri::command]
fn update_apply(app: AppHandle) -> Result<(), String> {
    updater::apply(&app)
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
fn audio_rate(engine: State<'_, Engine>, rate: f32) {
    engine.send(Cmd::Rate(rate));
}

#[tauri::command]
fn audio_pitch(engine: State<'_, Engine>, semitones: f32) {
    engine.send(Cmd::Pitch(semitones));
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
fn media_meta(app: AppHandle, media: State<'_, Media>, title: String, artist: String, album: String, cover: Option<String>, duration: f64) {
    #[cfg(desktop)]
    app.state::<discord::Discord>().send(discord::Msg::Track {
        title: title.clone(),
        artist: artist.clone(),
        album: album.clone(),
        cover: cover.clone(),
        duration,
    });
    #[cfg(mobile)]
    let _ = app;
    media.send(MediaCmd::Meta { title, artist, album, cover, duration });
}

#[tauri::command]
fn media_playback(app: AppHandle, media: State<'_, Media>, playing: bool, pos: f64) {
    #[cfg(desktop)]
    app.state::<discord::Discord>().send(discord::Msg::Playback { playing, pos });
    #[cfg(mobile)]
    let _ = app;
    media.send(MediaCmd::Playback { playing, pos });
}

// ------------------------------------------------------------- Last.fm (see lastfm.rs)

#[tauri::command]
fn lastfm_state(lastfm: State<'_, lastfm::Lastfm>) -> lastfm::LastfmState {
    lastfm.state()
}

#[tauri::command]
async fn lastfm_connect(app: AppHandle, lastfm: State<'_, lastfm::Lastfm>) -> Result<(), String> {
    lastfm.connect(&app).await
}

#[tauri::command]
async fn lastfm_finish(app: AppHandle, lastfm: State<'_, lastfm::Lastfm>) -> Result<Option<String>, String> {
    lastfm.finish(&app).await
}

#[tauri::command]
fn lastfm_disconnect(app: AppHandle, lastfm: State<'_, lastfm::Lastfm>) {
    lastfm.disconnect(&app)
}

#[tauri::command]
async fn lastfm_now_playing(lastfm: State<'_, lastfm::Lastfm>, song: lastfm::Scrobble) -> Result<(), String> {
    lastfm.now_playing(song).await
}

#[tauri::command]
async fn lastfm_scrobble(app: AppHandle, lastfm: State<'_, lastfm::Lastfm>, song: lastfm::Scrobble) -> Result<(), String> {
    lastfm.scrobble(&app, song).await
}

/// "Show on Discord" in Options.
#[tauri::command]
fn set_discord(_app: AppHandle, on: bool) {
    #[cfg(desktop)]
    _app.state::<discord::Discord>().send(discord::Msg::Enabled(on));
    #[cfg(mobile)]
    let _ = on;
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

#[cfg(desktop)]
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use media::MediaKey;
    use tauri::{
        menu::{Menu, MenuItem, PredefinedMenuItem},
        tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
        Emitter,
    };
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(desktop)]
    let args: Vec<String> = std::env::args().collect();
    #[cfg(desktop)]
    let (launch_uri, autostarted) = (spotify_arg(&args), args.iter().any(|a| a == "--autostarted" || a == "--hidden"));
    #[cfg(mobile)]
    let (launch_uri, autostarted) = (None, false);

    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            use tauri::Emitter;
            // Launching Plouk again with a link plays it in the running window.
            if let Some(uri) = spotify_arg(&args) {
                let _ = app.emit("media-key", media::MediaKey { uri: Some(uri), ..media::MediaKey::new("open") });
            }
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostarted"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::Pending::default())
        .manage(discord::Discord::spawn());

    #[cfg(target_os = "android")]
    let builder = builder.plugin(android::init());

    let app = builder
        .manage(LaunchUri(Mutex::new(launch_uri)))
        .manage(Prefs { close_to_tray: AtomicBool::new(true), autostarted })
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(Engine::spawn(handle.clone()));
            app.manage(Media::spawn(handle.clone()));
            app.manage(lastfm::Lastfm::load(&handle));
            let resolver = Resolver::new(&handle)?;
            app.manage(resolver.clone());
            tauri::async_runtime::spawn(resolver.ensure(handle.clone()));

            #[cfg(desktop)]
            {
                if let Err(e) = build_tray(app) {
                    eprintln!("[plouk] tray unavailable: {e}");
                }
                // The page shows the window once it has painted; this is only a safety net.
                let hidden = app.state::<Prefs>().autostarted;
                tauri::async_runtime::spawn(async move {
                    if hidden {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    if let Some(w) = handle.get_webview_window("main") {
                        if !w.is_visible().unwrap_or(true) {
                            let _ = w.show();
                        }
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides to the tray (music keeps playing) unless the user chose "Quit".
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if cfg!(desktop) {
                    api.prevent_close();
                    if window.state::<Prefs>().close_to_tray.load(Ordering::Relaxed) {
                        let _ = window.hide();
                    } else {
                        window.app_handle().exit(0);
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            take_launch_uri,
            platform,
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
            audio_rate,
            audio_pitch,
            audio_stop,
            media_meta,
            media_playback,
            media_volume,
            set_discord,
            lastfm_state,
            lastfm_connect,
            lastfm_finish,
            lastfm_disconnect,
            lastfm_now_playing,
            lastfm_scrobble,
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
            update_check,
            update_apply,
            app_info,
            audio_normalize,
            capture_window,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Plouk");

    app.run(|_app, _event| {
        // macOS: clicking the Dock icon brings the hidden window back.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            show_main(_app);
        }
    });
}
