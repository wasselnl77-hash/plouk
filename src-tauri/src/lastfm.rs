//! Last.fm scrobbling. Connecting opens the browser once so the user can let
//! Plouk post to their profile; the session that comes back lives in
//! `lastfm.json` next to the app state. The page decides *when* a song counts
//! (see `scrobble` in main.js); this side signs and sends the calls, and keeps
//! scrobbles that failed (offline, Last.fm down) to send with the next one.
//!
//! The API key and secret are baked in at build time from the
//! PLOUK_LASTFM_KEY / PLOUK_LASTFM_SECRET environment variables (GitHub secrets
//! for releases). Without them, Last.fm just doesn't show up in Options.

use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

const API: &str = "https://ws.audioscrobbler.com/2.0/";
const KEY: Option<&str> = option_env!("PLOUK_LASTFM_KEY");
const SECRET: Option<&str> = option_env!("PLOUK_LASTFM_SECRET");

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Scrobble {
    pub artist: String,
    pub track: String,
    pub album: String,
    pub duration: f64,
    /// When the song started playing (unix seconds).
    #[serde(default)]
    pub timestamp: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    user: Option<String>,
    session: Option<String>,
    #[serde(default)]
    pending: Vec<Scrobble>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastfmState {
    available: bool,
    user: Option<String>,
}

#[derive(Default)]
pub struct Lastfm {
    saved: Mutex<Saved>,
    /// The token from auth.getToken while the user is approving in the browser.
    token: Mutex<Option<String>>,
}

impl Lastfm {
    pub fn load(app: &AppHandle) -> Self {
        let saved = std::fs::read(file(app)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { saved: Mutex::new(saved), token: Mutex::new(None) }
    }

    pub fn state(&self) -> LastfmState {
        LastfmState { available: KEY.is_some() && SECRET.is_some(), user: self.saved.lock().unwrap().user.clone() }
    }

    fn store(&self, app: &AppHandle) {
        if let Ok(json) = serde_json::to_vec(&*self.saved.lock().unwrap()) {
            let _ = std::fs::write(file(app), json);
        }
    }

    /// Step 1: get a token and send the user to Last.fm to approve it.
    pub async fn connect(&self, app: &AppHandle) -> Result<(), String> {
        let key = KEY.ok_or("Last.fm isn't set up in this build")?;
        let res = call("auth.getToken", Params::new(), false).await?;
        let token = res["token"].as_str().ok_or("Last.fm sent no token")?.to_string();
        let url = format!("https://www.last.fm/api/auth/?api_key={key}&token={token}");
        *self.token.lock().unwrap() = Some(token);
        use tauri_plugin_opener::OpenerExt;
        app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
    }

    /// Step 2, polled by the page: once the user has approved, trade the token for a session.
    pub async fn finish(&self, app: &AppHandle) -> Result<Option<String>, String> {
        let Some(token) = self.token.lock().unwrap().clone() else { return Ok(None) };
        let res = match call("auth.getSession", Params::from([("token".into(), token)]), false).await {
            Ok(res) => res,
            Err(e) if e.contains("(14)") => return Ok(None), // not approved yet
            Err(e) => return Err(e),
        };
        let user = res["session"]["name"].as_str().map(str::to_string);
        let session = res["session"]["key"].as_str().map(str::to_string);
        if session.is_none() {
            return Err("Last.fm sent no session".into());
        }
        *self.token.lock().unwrap() = None;
        {
            let mut saved = self.saved.lock().unwrap();
            saved.user = user.clone();
            saved.session = session;
        }
        self.store(app);
        Ok(user)
    }

    pub fn disconnect(&self, app: &AppHandle) {
        *self.saved.lock().unwrap() = Saved::default();
        let _ = std::fs::remove_file(file(app));
    }

    pub async fn now_playing(&self, s: Scrobble) -> Result<(), String> {
        let Some(sk) = self.session() else { return Ok(()) };
        let mut p = track_params(&s, None);
        p.insert("sk".into(), sk);
        call("track.updateNowPlaying", p, true).await.map(|_| ())
    }

    /// Sends this scrobble plus any that failed before (Last.fm takes up to 50 at once).
    pub async fn scrobble(&self, app: &AppHandle, s: Scrobble) -> Result<(), String> {
        let Some(sk) = self.session() else { return Ok(()) };
        let batch: Vec<Scrobble> = {
            let mut saved = self.saved.lock().unwrap();
            saved.pending.push(s);
            let n = saved.pending.len().min(50);
            saved.pending[..n].to_vec()
        };
        let mut p = Params::from([("sk".into(), sk)]);
        for (i, s) in batch.iter().enumerate() {
            for (k, v) in track_params(s, Some(i)) {
                p.insert(k, v);
            }
        }
        let res = call("track.scrobble", p, true).await;
        if res.is_ok() {
            self.saved.lock().unwrap().pending.drain(..batch.len());
        }
        self.store(app);
        res.map(|_| ())
    }

    fn session(&self) -> Option<String> {
        self.saved.lock().unwrap().session.clone()
    }
}

fn file(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().unwrap_or_default().join("lastfm.json")
}

type Params = BTreeMap<String, String>;

/// `artist`, `track`… or `artist[0]`, `track[0]`… for batches.
fn track_params(s: &Scrobble, i: Option<usize>) -> Params {
    let key = |name: &str| match i {
        None => name.to_string(),
        Some(i) => format!("{name}[{i}]"),
    };
    let mut p = BTreeMap::from([(key("artist"), s.artist.clone()), (key("track"), s.track.clone())]);
    if !s.album.is_empty() {
        p.insert(key("album"), s.album.clone());
    }
    if s.duration > 0.0 {
        p.insert(key("duration"), (s.duration.round() as i64).to_string());
    }
    if i.is_some() {
        p.insert(key("timestamp"), s.timestamp.to_string());
    }
    p
}

/// A signed Last.fm API call (POST for writes, GET otherwise).
async fn call(method: &str, mut params: Params, post: bool) -> Result<Value, String> {
    let (key, secret) = KEY.zip(SECRET).ok_or("Last.fm isn't set up in this build")?;
    params.insert("method".into(), method.to_string());
    params.insert("api_key".into(), key.to_string());
    // api_sig: every parameter, sorted by name, as name+value, then the secret, md5'd.
    let mut sig = String::new();
    for (k, v) in &params {
        sig.push_str(k);
        sig.push_str(v);
    }
    sig.push_str(secret);
    params.insert("api_sig".into(), format!("{:x}", Md5::digest(sig.as_bytes())));
    params.insert("format".into(), "json".into());

    let client = reqwest::Client::new();
    let req = if post { client.post(API).form(&params) } else { client.get(API).query(&params) };
    let res: Value = req.send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    if let Some(code) = res["error"].as_i64() {
        return Err(format!("{} ({code})", res["message"].as_str().unwrap_or("Last.fm error")));
    }
    Ok(res)
}
