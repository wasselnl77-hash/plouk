//! Turns a Spotify track into a local audio file. Plouk keeps its own copy of
//! yt-dlp (downloaded on first launch, self-updated daily), finds the song on
//! YouTube Music, and caches the audio stream. Only the raw audio is fetched,
//! never YouTube's player, so there are no ads.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use futures::{
    future::{BoxFuture, Shared},
    FutureExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore, sync::watch};

use crate::loudness;


const AUDIO_EXTS: [&str; 7] = ["m4a", "mp3", "mp4", "aac", "flac", "ogg", "wav"];
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EngineState {
    Checking,
    Downloading { progress: f64 },
    Ready { version: String },
    Failed { message: String },
}

#[derive(Deserialize, Clone)]
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
    bin_dir: PathBuf,
    cache_dir: PathBuf,
    state_tx: watch::Sender<EngineState>,
    binary: Mutex<Option<PathBuf>>,
    inflight: Mutex<HashMap<String, Pending>>,
    slots: Semaphore,
    js_args: Vec<String>,
    convert_args: Vec<String>,
    /// Song cache budget in bytes (0 = unlimited); least recently played songs go first.
    cache_limit: AtomicU64,
}

impl Resolver {
    pub fn new(app: &AppHandle) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let bin_dir = app.path().app_data_dir()?.join("bin");
        let cache_dir = app.path().app_cache_dir()?.join("audio");
        std::fs::create_dir_all(&bin_dir)?;
        std::fs::create_dir_all(&cache_dir)?;

        // yt-dlp needs a JavaScript runtime for YouTube; it finds Deno on its own.
        let mut js_args = Vec::new();
        if which("deno").is_none() {
            if let Some(rt) = ["node", "bun"].into_iter().find(|r| which(r).is_some()) {
                js_args = vec!["--js-runtimes".to_string(), rt.to_string()];
            }
        }
        // With ffmpeg around, anything that isn't AAC gets converted so the decoder can play it.
        let convert_args =
            if which("ffmpeg").is_some() { ["-x", "--audio-format", "m4a"].map(String::from).to_vec() } else { Vec::new() };

        let (state_tx, _) = watch::channel(EngineState::Checking);
        Ok(Arc::new(Self {
            bin_dir,
            cache_dir,
            state_tx,
            binary: Mutex::new(None),
            inflight: Mutex::new(HashMap::new()),
            slots: Semaphore::new(3),
            js_args,
            convert_args,
            cache_limit: AtomicU64::new(5 << 30),
        }))
    }

    pub fn state(&self) -> EngineState {
        self.state_tx.borrow().clone()
    }

    fn set_state(&self, app: &AppHandle, s: EngineState) {
        let _ = app.emit("engine", &s);
        self.state_tx.send_replace(s);
    }

    /// Makes sure a working yt-dlp exists, downloading it if needed.
    pub async fn ensure(self: Arc<Self>, app: AppHandle) {
        let own = self.bin_dir.join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" });
        if !own.exists() {
            self.set_state(&app, EngineState::Downloading { progress: 0.0 });
            if let Err(e) = self.download_binary(&own, &app).await {
                eprintln!("[plouk] yt-dlp download failed: {e}");
            }
        }

        let candidates = [Some(own.clone()), which("yt-dlp")];
        for bin in candidates.into_iter().flatten() {
            if let Ok(version) = version_of(&bin).await {
                *self.binary.lock().unwrap() = Some(bin.clone());
                self.set_state(&app, EngineState::Ready { version });
                if bin == own {
                    self.clone().maybe_update(own).await;
                }
                return;
            }
        }
        self.set_state(
            &app,
            EngineState::Failed { message: "Couldn't set up yt-dlp. Check your internet connection and restart Plouk.".into() },
        );
    }

    async fn download_binary(&self, dest: &Path, app: &AppHandle) -> Result<(), String> {
        let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "aarch64") => "yt-dlp_linux_aarch64",
            ("linux", _) => "yt-dlp_linux",
            ("macos", _) => "yt-dlp_macos",
            ("windows", _) => "yt-dlp.exe",
            _ => "yt-dlp",
        };
        let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
        let resp = reqwest::Client::new()
            .get(&url)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?;
        let total = resp.content_length().unwrap_or(0) as f64;
        let part = dest.with_extension("part");
        let mut file = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
        let mut got = 0f64;
        let mut last = 0f64;
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            got += chunk.len() as f64;
            if total > 0.0 && got / total - last > 0.01 {
                last = got / total;
                self.set_state(app, EngineState::Downloading { progress: last });
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&part, dest).map_err(|e| e.to_string())
    }

    async fn maybe_update(self: Arc<Self>, bin: PathBuf) {
        let stamp = self.bin_dir.join("last-update");
        let fresh = std::fs::metadata(&stamp)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < UPDATE_EVERY);
        if fresh {
            return;
        }
        let _ = std::fs::write(&stamp, b"");
        tokio::spawn(async move {
            let out = Command::new(&bin).arg("-U").stdin(Stdio::null()).output().await;
            if let Ok(o) = out {
                eprintln!("[plouk] yt-dlp update: {}", String::from_utf8_lossy(&o.stdout).trim());
            }
        });
    }

    async fn binary(&self) -> Result<PathBuf, String> {
        let mut rx = self.state_tx.subscribe();
        loop {
            {
                let state = rx.borrow_and_update().clone();
                match state {
                    EngineState::Ready { .. } => {
                        return self.binary.lock().unwrap().clone().ok_or("yt-dlp missing".into())
                    }
                    EngineState::Failed { message } => return Err(message),
                    _ => {}
                }
            }
            rx.changed().await.map_err(|_| "Engine stopped".to_string())?;
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

    /// Runs yt-dlp's self-update right now and reports the resulting version.
    pub async fn update_now(&self, app: &AppHandle) -> Result<String, String> {
        let bin = self.binary().await?;
        run(Command::new(&bin).arg("-U"), 180).await?;
        let version = version_of(&bin).await?;
        let _ = std::fs::write(self.bin_dir.join("last-update"), b"");
        self.set_state(app, EngineState::Ready { version: version.clone() });
        Ok(version)
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
        let bin = self.binary().await?;
        let _slot = self.slots.acquire().await.map_err(|e| e.to_string())?;
        let artist = t.artist.split(',').next().unwrap_or(&t.artist).trim();
        let want = t.duration_ms as f64 / 1000.0;

        // 1) YouTube Music's song search returns the official audio first.
        let query = format!("{artist} {}", t.title);
        let music = format!("https://music.youtube.com/search?q={}#songs", urlencoding::encode(&query));
        match self.download(&bin, &music, &t.id, true).await {
            Ok((path, got)) => {
                let off = got.map(|g| (g - want).abs()).unwrap_or(0.0);
                if want <= 0.0 || off <= (want * 0.15).max(20.0) {
                    return Ok(Prepared { path: path.to_string_lossy().into() });
                }
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => eprintln!("[plouk] music search failed for {}: {e}", t.title),
        }

        // 2) Fall back to a scored regular YouTube search.
        let search = format!("ytsearch8:{artist} - {}", t.title);
        let out = run(Command::new(&bin).args(["--flat-playlist", "-J", "--no-warnings"]).args(&self.js_args).arg(&search), 60)
            .await?;
        let v: Value = serde_json::from_str(&out).map_err(|_| "Unexpected search result")?;
        let best = v["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| Some((score(e, t, artist), e["id"].as_str()?)))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .ok_or_else(|| format!("Couldn't find “{}” on YouTube", t.title))?;
        let url = format!("https://www.youtube.com/watch?v={}", best.1);
        let (path, _) = self.download(&bin, &url, &t.id, false).await?;
        Ok(Prepared { path: path.to_string_lossy().into() })
    }

    async fn download(&self, bin: &Path, url: &str, id: &str, search: bool) -> Result<(PathBuf, Option<f64>), String> {
        let template = self.cache_dir.join(format!("{id}.%(ext)s"));
        let mut cmd = Command::new(bin);
        cmd.args(["--no-progress", "--no-warnings", "--no-mtime", "-f", "bestaudio[ext=m4a]/bestaudio"])
            .arg("-o")
            .arg(&template)
            .args(["--print", "after_move:%(filepath)s\t%(duration)s"])
            .args(&self.js_args)
            .args(&self.convert_args);
        if search {
            cmd.args(["-I", "1"]);
        } else {
            cmd.arg("--no-playlist");
        }
        cmd.arg(url);
        let out = run(&mut cmd, 120).await?;
        let line = out.lines().rev().find(|l| l.contains('\t')).ok_or("No song found")?;
        let (path, dur) = line.split_once('\t').unwrap_or((line, ""));
        let path = PathBuf::from(path.trim());
        if !path.exists() {
            return Err("Download failed".into());
        }
        Ok((path, dur.trim().parse().ok()))
    }
}

/// How well a YouTube search result matches the Spotify track (higher is better).
fn score(e: &Value, t: &TrackReq, artist: &str) -> f64 {
    let norm = |s: &str| s.to_lowercase();
    let title = norm(e["title"].as_str().unwrap_or_default());
    let channel = norm(e["channel"].as_str().or(e["uploader"].as_str()).unwrap_or_default());
    let want_title = norm(&t.title);
    let want_artist = norm(artist);
    let mut s = 0.0;

    match e["duration"].as_f64() {
        Some(d) => s -= ((d - t.duration_ms as f64 / 1000.0).abs() * 1.5).min(90.0),
        None => s -= 40.0,
    }
    let core_title = want_title.split(" - ").next().unwrap_or(&want_title).split(" (").next().unwrap_or(&want_title);
    if title.contains(core_title) {
        s += 25.0;
    }
    if channel.contains(&want_artist) || title.contains(&want_artist) {
        s += 15.0;
    }
    if channel.ends_with(" - topic") {
        s += 20.0;
    }
    if title.contains("official audio") {
        s += 8.0;
    }
    for bad in ["live", "cover", "remix", "sped up", "slowed", "nightcore", "8d", "reverb", "karaoke", "instrumental", "hour", "loop", "reaction", "tutorial"] {
        if title.contains(bad) && !want_title.contains(bad) {
            s -= 40.0;
        }
    }
    s
}

async fn run(cmd: &mut Command, timeout_secs: u64) -> Result<String, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output())
        .await
        .map_err(|_| "YouTube took too long".to_string())?
        .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        return Err(last.trim_start_matches("ERROR: ").to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn version_of(bin: &Path) -> Result<String, String> {
    let out = run(Command::new(bin).arg("--version"), 30).await?;
    Ok(out.trim().to_string())
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join(name)).find(|p| p.is_file())
}
