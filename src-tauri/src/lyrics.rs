//! Time-synced lyrics from LRCLIB (free, no key).

use serde::Serialize;
use serde_json::Value;

use crate::spotify::http;

const UA: &str = "Plouk/0.1.0 (desktop music player)";

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    pub synced: Option<String>,
    pub plain: Option<String>,
    pub instrumental: bool,
}

async fn search(params: &[(&str, &str)]) -> Vec<Value> {
    let req = http().get("https://lrclib.net/api/search").header("User-Agent", UA).query(params);
    match req.send().await {
        Ok(r) => r.json::<Vec<Value>>().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// "Song - Remastered 2011" / "Song (feat. X)" → "Song"
fn clean_title(t: &str) -> &str {
    let t = t.split(" - ").next().unwrap_or(t);
    match t.find(" (feat") .or_else(|| t.find(" (with ")) {
        Some(i) => &t[..i],
        None => t,
    }
    .trim()
}

pub async fn find(title: &str, artist: &str, duration: f64) -> Result<Lyrics, String> {
    let artist = artist.split(',').next().unwrap_or(artist).trim();
    let title = clean_title(title);
    let mut items = search(&[("track_name", title), ("artist_name", artist)]).await;
    if items.is_empty() {
        items = search(&[("q", &format!("{title} {artist}"))]).await;
    }

    let off = |v: &Value| v["duration"].as_f64().map(|d| (d - duration).abs()).unwrap_or(30.0);
    let pick = |synced: bool, tolerance: f64| {
        items
            .iter()
            .filter(|v| if synced { v["syncedLyrics"].is_string() } else { v["plainLyrics"].is_string() })
            .filter(|v| duration <= 0.0 || off(v) <= tolerance)
            .min_by(|a, b| off(a).total_cmp(&off(b)))
    };

    if let Some(v) = pick(true, 10.0).or_else(|| pick(true, 25.0)) {
        return Ok(Lyrics { synced: v["syncedLyrics"].as_str().map(String::from), plain: None, instrumental: false });
    }
    if let Some(v) = pick(false, 25.0) {
        return Ok(Lyrics { synced: None, plain: v["plainLyrics"].as_str().map(String::from), instrumental: false });
    }
    let instrumental = items.iter().any(|v| v["instrumental"].as_bool() == Some(true) && off(v) <= 10.0);
    Ok(Lyrics { instrumental, ..Default::default() })
}
