//! System media integration (MPRIS on Linux): media keys, the desktop's media
//! widget, and "Raise"/"Quit" from it. Runs on its own thread.

use std::{sync::mpsc, thread, time::Duration};

use serde::Serialize;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
};
use tauri::{AppHandle, Emitter};

pub enum MediaCmd {
    Meta { title: String, artist: String, album: String, cover: Option<String>, duration: f64 },
    Playback { playing: bool, pos: f64 },
    Volume(f64),
}

#[derive(Serialize, Clone)]
pub struct MediaKey {
    pub action: String,
    pub value: Option<f64>,
    pub uri: Option<String>,
}

impl MediaKey {
    pub fn new(action: &str) -> Self {
        Self { action: action.into(), value: None, uri: None }
    }
}

pub struct Media {
    tx: mpsc::Sender<MediaCmd>,
}

impl Media {
    pub fn spawn(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel::<MediaCmd>();
        thread::Builder::new()
            .name("plouk-media".into())
            .spawn(move || {
                let config = PlatformConfig { display_name: "Plouk", dbus_name: "plouk", hwnd: None };
                let mut controls = match MediaControls::new(config) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[plouk] media controls unavailable: {e:?}");
                        while rx.recv().is_ok() {}
                        return;
                    }
                };
                let events_app = app.clone();
                let attached = controls.attach(move |event| on_event(&events_app, event));
                if let Err(e) = attached {
                    eprintln!("[plouk] media keys unavailable: {e:?}");
                }
                while let Ok(cmd) = rx.recv() {
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
                        MediaCmd::Volume(v) => controls.set_volume(v),
                    };
                }
            })
            .expect("failed to start media thread");
        Self { tx }
    }

    pub fn send(&self, cmd: MediaCmd) {
        let _ = self.tx.send(cmd);
    }
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
