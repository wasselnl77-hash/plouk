//! Turns a Spotify track into a local audio file and looks after the song
//! cache. On desktop the song is found with Plouk's own yt-dlp (ytdlp.rs); on
//! Android the NewPipe extractor does it through the Kotlin plugin (android.rs).
//! Either way only the raw audio is fetched, never YouTube's player, so there
//! are no ads.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::SystemTime,
};

use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{watch, Semaphore};

use crate::loudness;

pub const AUDIO_EXTS: [&str; 7] = ["m4a", "mp3", "mp4", "aac", "flac", "ogg", "wav"];

#[derive(Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EngineState {
    Checking,
    Downloading { progress: f64 },
    Ready { version: String },
    Failed { message: String },
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrackReq {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
}

#[derive(Serialize, Clone)]
pub struct Prepared {
    pub path: String,
}

#[derive(Serialize)]
pub struct CacheInfo {
    pub bytes: u64,
    pub songs: u64,
}

type Pending = Shared<BoxFuture<'static, Result<Prepared, String>>>;

pub struct Resolver {
    pub(crate) cache_dir: PathBuf,
    state_tx: watch::Sender<EngineState>,
    inflight: Mutex<HashMap<String, Pending>>,
    slots: Semaphore,
    /// Song cache budget in bytes (0 = unlimited); least recently played songs go first.
    cache_limit: AtomicU64,
    #[cfg(desktop)]
    pub(crate) yt: crate::ytdlp::YtDlp,
}

impl Resolver {
    pub fn new(app: &AppHandle) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let cache_dir = app.path().app_cache_dir()?.join("audio");
        std::fs::create_dir_all(&cache_dir)?;
        let (state_tx, _) = watch::channel(EngineState::Checking);
        Ok(Arc::new(Self {
            cache_dir,
            state_tx,
            inflight: Mutex::new(HashMap::new()),
            slots: Semaphore::new(3),
            cache_limit: AtomicU64::new(5 << 30),
            #[cfg(desktop)]
            yt: crate::ytdlp::YtDlp::new(app.path().app_data_dir()?.join("bin"))?,
        }))
    }

    pub fn state(&self) -> EngineState {
        self.state_tx.borrow().clone()
    }

    pub(crate) fn set_state(&self, app: &AppHandle, s: EngineState) {
        let _ = app.emit("engine", &s);
        self.state_tx.send_replace(s);
    }

    /// Gets the song-finding engine ready (on desktop: downloads yt-dlp if needed).
    pub async fn ensure(self: Arc<Self>, app: AppHandle) {
        #[cfg(desktop)]
        crate::ytdlp::ensure(&self, &app).await;
        #[cfg(mobile)]
        self.set_state(&app, EngineState::Ready { version: "NewPipe".into() });
    }

    /// Waits until the engine is ready, or reports why it never will be.
    async fn ready(&self) -> Result<(), String> {
        let mut rx = self.state_tx.subscribe();
        loop {
            {
                let state = rx.borrow_and_update().clone();
                match state {
                    EngineState::Ready { .. } => return Ok(()),
                    EngineState::Failed { message } => return Err(message),
                    _ => {}
                }
            }
            rx.changed().await.map_err(|_| "Engine stopped".to_string())?;
        }
    }

    /// Updates the engine right now and reports the resulting version.
    pub async fn update_now(&self, app: &AppHandle) -> Result<String, String> {
        #[cfg(desktop)]
        return crate::ytdlp::update_now(self, app).await;
        #[cfg(mobile)]
        {
            let _ = app;
            Ok("NewPipe (built in, updates with Plouk)".into())
        }
    }

    fn songs(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let Ok(dir) = std::fs::read_dir(&self.cache_dir) else { return Vec::new() };
        dir.flatten()
            .filter_map(|e| {
                let path = e.path();
                let ext = path.extension()?.to_str()?;
                let meta = e.metadata().ok()?;
                AUDIO_EXTS.contains(&ext).then(|| (path, meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
            })
            .collect()
    }

    pub fn cache_info(&self) -> CacheInfo {
        let songs = self.songs();
        CacheInfo { bytes: songs.iter().map(|s| s.1).sum(), songs: songs.len() as u64 }
    }

    /// Deletes every cached song (files that are playing keep playing until closed).
    pub fn clear_cache(&self) -> u64 {
        let mut freed = 0;
        for (path, size, _) in self.songs() {
            if std::fs::remove_file(&path).is_ok() {
                freed += size;
                let _ = std::fs::remove_file(loudness::sidecar(&path));
            }
        }
        freed
    }

    pub fn set_cache_limit(&self, bytes: u64) {
        self.cache_limit.store(bytes, Ordering::Relaxed);
        self.prune(None);
    }

    fn prune(&self, keep: Option<&Path>) {
        let limit = self.cache_limit.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }
        let mut songs = self.songs();
        let mut total: u64 = songs.iter().map(|s| s.1).sum();
        songs.sort_by_key(|s| s.2);
        for (path, size, _) in songs {
            if total <= limit {
                break;
            }
            if Some(path.as_path()) == keep {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= size;
                let _ = std::fs::remove_file(loudness::sidecar(&path));
            }
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn cached(&self, id: &str) -> Option<PathBuf> {
        AUDIO_EXTS
            .iter()
            .map(|ext| self.cache_dir.join(format!("{id}.{ext}")))
            .find(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
    }

    /// Returns a local file for the track, sharing work with any identical request in flight.
    pub async fn prepare(self: Arc<Self>, track: TrackReq) -> Result<Prepared, String> {
        if !track.id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("Invalid track id".into());
        }
        if let Some(p) = self.cached(&track.id) {
            // Mark as recently played so the cache keeps it.
            if let Ok(f) = std::fs::File::options().append(true).open(&p) {
                let _ = f.set_modified(SystemTime::now());
            }
            loudness::spawn(&p);
            return Ok(Prepared { path: p.to_string_lossy().into() });
        }
        let pending = {
            let mut map = self.inflight.lock().unwrap();
            map.entry(track.id.clone())
                .or_insert_with(|| {
                    let me = self.clone();
                    let id = track.id.clone();
                    async move {
                        let res = me.fetch(&track).await;
                        me.inflight.lock().unwrap().remove(&id);
                        if let Ok(p) = &res {
                            let path = PathBuf::from(&p.path);
                            loudness::spawn(&path);
                            me.prune(Some(&path));
                        }
                        res
                    }
                    .boxed()
                    .shared()
                })
                .clone()
        };
        pending.await
    }

    async fn fetch(&self, t: &TrackReq) -> Result<Prepared, String> {
        self.ready().await?;
        let _slot = self.slots.acquire().await.map_err(|e| e.to_string())?;
        #[cfg(desktop)]
        let path = crate::ytdlp::fetch(self, t).await?;
        #[cfg(target_os = "android")]
        let path = crate::android::fetch(t, &self.cache_dir).await?;
        #[cfg(target_os = "ios")]
        let path: PathBuf = return Err(format!("Not supported on this device yet ({})", t.title));
        Ok(Prepared { path: path.to_string_lossy().into() })
    }
}
