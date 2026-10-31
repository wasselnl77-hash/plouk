//! System media integration: media keys, the desktop's media widget and lock
//! screen. Linux uses MPRIS on its own thread, macOS uses Now Playing on the
//! main thread, and Android hands everything to the Kotlin media session.

use serde::Serialize;
use tauri::AppHandle;

pub enum MediaCmd {
    Meta { title: String, artist: String, album: String, cover: Option<String>, duration: f64 },
    Playback { playing: bool, pos: f64 },
    /// Only MPRIS shows a volume; elsewhere it's dropped.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Volume(f64),
}

#[derive(Serialize, Clone)]
pub struct MediaKey {
    pub action: String,
    pub value: Option<f64>,
    pub uri: Option<String>,
}

impl MediaKey {
    #[cfg_attr(mobile, allow(dead_code))]
    pub fn new(action: &str) -> Self {
        Self { action: action.into(), value: None, uri: None }
    }
}

// ---------------------------------------------------------------- desktop (souvlaki)

#[cfg(desktop)]
mod desktop {
    use std::time::Duration;

    use souvlaki::{
        MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
    };
    use tauri::{AppHandle, Emitter};

    use super::{MediaCmd, MediaKey};

    pub fn create(app: &AppHandle) -> Option<MediaControls> {
        let config = PlatformConfig { display_name: "Plouk", dbus_name: "plouk", hwnd: None };
        let mut controls = match MediaControls::new(config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[plouk] media controls unavailable: {e:?}");
                return None;
            }
        };
        let events_app = app.clone();
        if let Err(e) = controls.attach(move |event| on_event(&events_app, event)) {
            eprintln!("[plouk] media keys unavailable: {e:?}");
        }
        Some(controls)
    }

    pub fn apply(controls: &mut MediaControls, cmd: MediaCmd) {
        let _ = match cmd {
            MediaCmd::Meta { title, artist, album, cover, duration } => controls.set_metadata(MediaMetadata {
                title: Some(&title),
                artist: Some(&artist),
                album: Some(&album),
                cover_url: cover.as_deref(),
                duration: (duration > 0.0).then(|| Duration::from_secs_f64(duration)),
            }),
            MediaCmd::Playback { playing, pos } => {
                let progress = Some(MediaPosition(Duration::from_secs_f64(pos.max(0.0))));
                controls.set_playback(if playing {
                    MediaPlayback::Playing { progress }
                } else {
                    MediaPlayback::Paused { progress }
                })
            }
            #[cfg(target_os = "linux")]
            MediaCmd::Volume(v) => controls.set_volume(v),
            #[cfg(not(target_os = "linux"))]
            MediaCmd::Volume(_) => Ok(()),
        };
    }

    fn on_event(app: &AppHandle, event: MediaControlEvent) {
        let signed = |dir: SeekDirection, secs: f64| if matches!(dir, SeekDirection::Forward) { secs } else { -secs };
        let key = match event {
            MediaControlEvent::Play => MediaKey::new("play"),
            MediaControlEvent::Pause | MediaControlEvent::Stop => MediaKey::new("pause"),
            MediaControlEvent::Toggle => MediaKey::new("toggle"),
            MediaControlEvent::Next => MediaKey::new("next"),
            MediaControlEvent::Previous => MediaKey::new("prev"),
            MediaControlEvent::Seek(dir) => MediaKey { value: Some(signed(dir, 10.0)), ..MediaKey::new("seekBy") },
            MediaControlEvent::SeekBy(dir, d) => MediaKey { value: Some(signed(dir, d.as_secs_f64())), ..MediaKey::new("seekBy") },
            MediaControlEvent::SetPosition(MediaPosition(p)) => MediaKey { value: Some(p.as_secs_f64()), ..MediaKey::new("setPos") },
            MediaControlEvent::SetVolume(v) => MediaKey { value: Some(v), ..MediaKey::new("volume") },
            MediaControlEvent::OpenUri(uri) => MediaKey { uri: Some(uri), ..MediaKey::new("open") },
            MediaControlEvent::Raise => {
                crate::show_main(app);
                return;
            }
            MediaControlEvent::Quit => {
                app.exit(0);
                return;
            }
            _ => return,
        };
        let _ = app.emit("media-key", key);
    }
}

// ---------------------------------------------------------------- Media handle

pub struct Media {
    #[cfg(all(desktop, not(target_os = "macos")))]
    tx: std::sync::mpsc::Sender<MediaCmd>,
    #[cfg(any(target_os = "macos", mobile))]
    app: AppHandle,
}

#[cfg(target_os = "macos")]
thread_local! {
    static CONTROLS: std::cell::RefCell<Option<souvlaki::MediaControls>> = const { std::cell::RefCell::new(None) };
}

impl Media {
    /// Called from `setup`, which runs on the main thread.
    pub fn spawn(app: AppHandle) -> Self {
        #[cfg(all(desktop, not(target_os = "macos")))]
        {
            let (tx, rx) = std::sync::mpsc::channel::<MediaCmd>();
            std::thread::Builder::new()
                .name("plouk-media".into())
                .spawn(move || {
                    let Some(mut controls) = desktop::create(&app) else {
                        while rx.recv().is_ok() {}
                        return;
                    };
                    while let Ok(cmd) = rx.recv() {
                        desktop::apply(&mut controls, cmd);
                    }
                })
                .expect("failed to start media thread");
            Self { tx }
        }
        #[cfg(target_os = "macos")]
        {
            let controls = desktop::create(&app);
            CONTROLS.with(|c| *c.borrow_mut() = controls);
            Self { app }
        }
        #[cfg(mobile)]
        {
            Self { app }
        }
    }

    pub fn send(&self, cmd: MediaCmd) {
        #[cfg(all(desktop, not(target_os = "macos")))]
        {
            let _ = self.tx.send(cmd);
        }
        #[cfg(target_os = "macos")]
        {
            let _ = self.app.run_on_main_thread(move || {
                CONTROLS.with(|c| {
                    if let Some(controls) = c.borrow_mut().as_mut() {
                        desktop::apply(controls, cmd);
                    }
                })
            });
        }
        #[cfg(target_os = "android")]
        {
            crate::android::media(&self.app, cmd);
        }
        #[cfg(target_os = "ios")]
        {
            let _ = (&self.app, cmd);
        }
    }
}
