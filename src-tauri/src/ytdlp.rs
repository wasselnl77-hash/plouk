//! Desktop song finder. Plouk keeps its own copy of yt-dlp (downloaded on first
//! launch, self-updated daily) and, if the system has no JavaScript runtime for
//! YouTube, its own copy of Deno too. Songs are searched on YouTube Music first,
//! with a scored regular YouTube search as fallback.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use futures::StreamExt;
use serde_json::Value;
use tauri::AppHandle;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::resolver::{EngineState, Resolver, TrackReq};

const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);
const EXE: &str = std::env::consts::EXE_SUFFIX;

pub struct YtDlp {
    bin_dir: PathBuf,
    binary: Mutex<Option<PathBuf>>,
    js_args: Mutex<Vec<String>>,
    convert_args: Vec<String>,
}

impl YtDlp {
    pub fn new(bin_dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&bin_dir)?;
        // With ffmpeg around, anything that isn't AAC gets converted so the decoder can play it.
        let convert_args =
            if which("ffmpeg").is_some() { ["-x", "--audio-format", "m4a"].map(String::from).to_vec() } else { Vec::new() };
        Ok(Self { bin_dir, binary: Mutex::new(None), js_args: Mutex::new(Vec::new()), convert_args })
    }

    fn js_args(&self) -> Vec<String> {
        self.js_args.lock().unwrap().clone()
    }
}

/// Makes sure a working yt-dlp (and a JavaScript runtime for it) exists.
pub async fn ensure(r: &Arc<Resolver>, app: &AppHandle) {
    let yt = &r.yt;
    let own = yt.bin_dir.join(format!("yt-dlp{EXE}"));
    if !own.exists() {
        r.set_state(app, EngineState::Downloading { progress: 0.0 });
        let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "aarch64") => "yt-dlp_linux_aarch64",
            ("linux", _) => "yt-dlp_linux",
            ("macos", _) => "yt-dlp_macos",
            ("windows", _) => "yt-dlp.exe",
            _ => "yt-dlp",
        };
        let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
        if let Err(e) = download(r, app, &url, &own, 0.0, 0.8).await {
            eprintln!("[plouk] yt-dlp download failed: {e}");
        }
    }

    // YouTube needs a JavaScript runtime. yt-dlp finds Deno itself; Node or Bun
    // must be named; otherwise Plouk fetches its own Deno.
    let js = if which("deno").is_some() {
        Vec::new()
    } else if let Some(rt) = ["node", "bun"].into_iter().find(|r| which(r).is_some()) {
        vec!["--js-runtimes".to_string(), rt.to_string()]
    } else {
        let deno = yt.bin_dir.join(format!("deno{EXE}"));
        if !deno.exists() {
            if let Err(e) = fetch_deno(r, app, &deno).await {
                eprintln!("[plouk] deno download failed: {e}");
            }
        }
        if deno.exists() {
            vec!["--js-runtimes".to_string(), format!("deno:{}", deno.display())]
        } else {
            Vec::new()
        }
    };
    *yt.js_args.lock().unwrap() = js;

    let candidates = [Some(own.clone()), which("yt-dlp")];
    for bin in candidates.into_iter().flatten() {
        if let Ok(version) = version_of(&bin).await {
            *yt.binary.lock().unwrap() = Some(bin.clone());
            r.set_state(app, EngineState::Ready { version });
            if bin == own {
                maybe_update(yt, own);
            }
            return;
        }
    }
    r.set_state(
        app,
        EngineState::Failed { message: "Couldn't set up yt-dlp. Check your internet connection and restart Plouk.".into() },
    );
}

/// Streams `url` to `dest`, reporting progress mapped into [from, to] of the setup toast.
async fn download(r: &Resolver, app: &AppHandle, url: &str, dest: &Path, from: f64, to: f64) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(900))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let total = resp.content_length().unwrap_or(0) as f64;
    let part = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let (mut got, mut last) = (0f64, 0f64);
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        got += chunk.len() as f64;
        if total > 0.0 && got / total - last > 0.01 {
            last = got / total;
            r.set_state(app, EngineState::Downloading { progress: from + (to - from) * last });
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    make_executable(&part)?;
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let _ = path;
    Ok(())
}

async fn fetch_deno(r: &Resolver, app: &AppHandle, dest: &Path) -> Result<(), String> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", _) => "x86_64-unknown-linux-gnu",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", _) => "x86_64-apple-darwin",
        ("windows", _) => "x86_64-pc-windows-msvc",
        _ => return Err("no Deno build for this platform".into()),
    };
    r.set_state(app, EngineState::Downloading { progress: 0.8 });
    let zip_path = dest.with_extension("zip");
    let url = format!("https://github.com/denoland/deno/releases/latest/download/deno-{target}.zip");
    download(r, app, &url, &zip_path, 0.8, 1.0).await?;
    let unpacked = (|| -> Result<(), String> {
        let file = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let mut entry = archive.by_name(&format!("deno{EXE}")).map_err(|e| e.to_string())?;
        let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        make_executable(dest)
    })();
    let _ = std::fs::remove_file(&zip_path);
    unpacked
}

fn maybe_update(yt: &YtDlp, bin: PathBuf) {
    let stamp = yt.bin_dir.join("last-update");
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
        let out = command(&bin).arg("-U").stdin(Stdio::null()).output().await;
        if let Ok(o) = out {
            eprintln!("[plouk] yt-dlp update: {}", String::from_utf8_lossy(&o.stdout).trim());
        }
    });
}

fn binary(yt: &YtDlp) -> Result<PathBuf, String> {
    yt.binary.lock().unwrap().clone().ok_or_else(|| "yt-dlp missing".into())
}

/// Runs yt-dlp's self-update right now and reports the resulting version.
pub async fn update_now(r: &Resolver, app: &AppHandle) -> Result<String, String> {
    let bin = binary(&r.yt)?;
    run(command(&bin).arg("-U"), 180).await?;
    let version = version_of(&bin).await?;
    let _ = std::fs::write(r.yt.bin_dir.join("last-update"), b"");
    r.set_state(app, EngineState::Ready { version: version.clone() });
    Ok(version)
}

pub async fn fetch(r: &Resolver, t: &TrackReq) -> Result<PathBuf, String> {
    let bin = binary(&r.yt)?;
    let artist = t.artist.split(',').next().unwrap_or(&t.artist).trim();
    let want = t.duration_ms as f64 / 1000.0;

    // 1) YouTube Music's song search returns the official audio first.
    let query = format!("{artist} {}", t.title);
    let music = format!("https://music.youtube.com/search?q={}#songs", urlencoding::encode(&query));
    match grab(r, &bin, &music, &t.id, true).await {
        Ok((path, got)) => {
            let off = got.map(|g| (g - want).abs()).unwrap_or(0.0);
            if want <= 0.0 || off <= (want * 0.15).max(20.0) {
                return Ok(path);
            }
            let _ = std::fs::remove_file(&path);
        }
        Err(e) => eprintln!("[plouk] music search failed for {}: {e}", t.title),
    }

    // 2) Fall back to a scored regular YouTube search.
    let search = format!("ytsearch8:{artist} - {}", t.title);
    let out = run(command(&bin).args(["--flat-playlist", "-J", "--no-warnings"]).args(r.yt.js_args()).arg(&search), 60).await?;
    let v: Value = serde_json::from_str(&out).map_err(|_| "Unexpected search result")?;
    let best = v["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some((score(e, t, artist), e["id"].as_str()?)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .ok_or_else(|| format!("Couldn't find “{}” on YouTube", t.title))?;
    let url = format!("https://www.youtube.com/watch?v={}", best.1);
    Ok(grab(r, &bin, &url, &t.id, false).await?.0)
}

async fn grab(r: &Resolver, bin: &Path, url: &str, id: &str, search: bool) -> Result<(PathBuf, Option<f64>), String> {
    let template = r.cache_dir.join(format!("{id}.%(ext)s"));
    let mut cmd = command(bin);
    cmd.args(["--no-progress", "--no-warnings", "--no-mtime", "-f", "bestaudio[ext=m4a]/bestaudio"])
        .arg("-o")
        .arg(&template)
        .args(["--print", "after_move:%(filepath)s\t%(duration)s"])
        .args(r.yt.js_args())
        .args(&r.yt.convert_args);
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
    let out = run(command(bin).arg("--version"), 30).await?;
    Ok(out.trim().to_string())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(format!("{name}{EXE}"))).find(|p| p.is_file())
}

/// A child process that doesn't flash a console window on Windows.
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}
