//! Reads what you drop from Spotify. A drag out of the Spotify app carries only
//! links (open.spotify.com/... or spotify:...); the track list comes from
//! Spotify's public embed page, so no login or API key is needed.

use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    sync::{Mutex, OnceLock},
    time::Duration,
};

use futures::{stream, StreamExt};
use serde::Serialize;
use serde_json::Value;

const BROWSER_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const KINDS: [&str; 3] = ["playlist", "album", "track"];

pub fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .timeout(Duration::from_secs(25))
            .build()
            .expect("http client")
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub explicit: bool,
    pub art: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub subtitle: String,
    pub cover: Option<String>,
    pub url: Option<String>,
    pub tracks: Vec<Track>,
}

/// Pulls every (kind, id) pair out of dropped/pasted text, in order, without duplicates.
pub fn parse_refs(input: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let separators = |c: char| c.is_whitespace() || matches!(c, ',' | '"' | '\'' | '<' | '>' | '(' | ')');
    for token in input.split(separators) {
        if let Some(r) = parse_one(token.trim()) {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

fn parse_one(token: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = if let Some(i) = token.find("open.spotify.com/") {
        let path = token[i + "open.spotify.com/".len()..].split(['?', '#']).next()?;
        path.split('/').filter(|s| !s.is_empty()).collect()
    } else if token.starts_with("spotify:") {
        token.split(':').collect()
    } else {
        return None;
    };
    parts
        .windows(2)
        .find(|w| KINDS.contains(&w[0]) && valid_id(w[1]))
        .map(|w| (w[0].to_string(), w[1].to_string()))
}

fn valid_id(s: &str) -> bool {
    (16..=32).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn net(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "Spotify took too long to answer".into()
    } else {
        format!("Couldn't reach Spotify ({e})")
    }
}

async fn entity(kind: &str, id: &str) -> Result<Value, String> {
    let url = format!("https://open.spotify.com/embed/{kind}/{id}");
    let html = http()
        .get(&url)
        .send()
        .await
        .map_err(net)?
        .error_for_status()
        .map_err(|e| match e.status().map(|s| s.as_u16()) {
            Some(404) => "That playlist is private or doesn't exist".to_string(),
            _ => net(e),
        })?
        .text()
        .await
        .map_err(net)?;
    let marker = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html.find(marker).ok_or("Spotify changed its page format")? + marker.len();
    let end = start + html[start..].find("</script>").ok_or("Spotify changed its page format")?;
    let v: Value = serde_json::from_str(&html[start..end]).map_err(|_| "Couldn't read Spotify's data")?;
    v.pointer("/props/pageProps/state/data/entity")
        .filter(|e| !e.is_null())
        .cloned()
        .ok_or_else(|| "That playlist is private or unavailable".to_string())
}

fn best_image(e: &Value) -> Option<String> {
    if let Some(images) = e.pointer("/visualIdentity/image").and_then(Value::as_array) {
        if let Some(url) = images
            .iter()
            .max_by_key(|i| i["maxWidth"].as_u64().unwrap_or(0))
            .and_then(|i| i["url"].as_str())
        {
            return Some(url.to_string());
        }
    }
    e.pointer("/coverArt/sources/0/url").and_then(Value::as_str).map(String::from)
}

fn list_item(v: &Value, art: Option<&String>) -> Option<Track> {
    if v["entityType"].as_str().is_some_and(|t| t != "track") {
        return None;
    }
    let id = v["uri"].as_str()?.rsplit(':').next()?.to_string();
    Some(Track {
        id,
        title: v["title"].as_str()?.to_string(),
        artist: v["subtitle"].as_str().unwrap_or_default().to_string(),
        duration_ms: v["duration"].as_u64().unwrap_or(0),
        explicit: v["isExplicit"].as_bool().unwrap_or(false),
        art: art.cloned(),
    })
}

fn single_track(e: &Value) -> Option<Track> {
    let artist = e["artists"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    Some(Track {
        id: e["id"].as_str()?.to_string(),
        title: e["name"].as_str().or(e["title"].as_str())?.to_string(),
        artist,
        duration_ms: e["duration"].as_u64().unwrap_or(0),
        explicit: e["isExplicit"].as_bool().unwrap_or(false),
        art: best_image(e),
    })
}

pub async fn fetch(input: &str) -> Result<Collection, String> {
    let refs = parse_refs(input);
    let (kind, id) = refs.first().cloned().ok_or("That doesn't look like a Spotify link")?;

    if kind == "track" {
        // One or more songs dragged out of Spotify: make a small ad-hoc collection.
        let ids: Vec<String> = refs.into_iter().filter(|(k, _)| k == "track").map(|(_, i)| i).take(300).collect();
        let tracks: Vec<Track> = stream::iter(ids.clone())
            .map(|id| async move { entity("track", &id).await.ok().and_then(|e| single_track(&e)) })
            .buffered(6)
            .filter_map(|t| async move { t })
            .collect()
            .await;
        let first = tracks.first().ok_or("Couldn't read those songs from Spotify")?.clone();
        let mut h = DefaultHasher::new();
        ids.hash(&mut h);
        let single = tracks.len() == 1;
        return Ok(Collection {
            id: if single { format!("track:{}", first.id) } else { format!("tracks:{:x}", h.finish()) },
            kind: "tracks".into(),
            name: if single { first.title.clone() } else { format!("{} dropped songs", tracks.len()) },
            subtitle: if single { first.artist.clone() } else { "Dropped from Spotify".into() },
            cover: first.art.clone(),
            url: single.then(|| format!("https://open.spotify.com/track/{}", first.id)),
            tracks,
        });
    }

    let e = entity(&kind, &id).await?;
    let cover = best_image(&e);
    let album_art = if kind == "album" { cover.clone() } else { None };
    let tracks: Vec<Track> = e["trackList"]
        .as_array()
        .map(|list| list.iter().filter_map(|t| list_item(t, album_art.as_ref())).collect())
        .unwrap_or_default();
    if tracks.is_empty() {
        return Err("That playlist has no playable songs".into());
    }
    Ok(Collection {
        id: format!("{kind}:{id}"),
        name: e["name"].as_str().or(e["title"].as_str()).unwrap_or("Untitled").to_string(),
        subtitle: e["subtitle"].as_str().unwrap_or_default().to_string(),
        url: Some(format!("https://open.spotify.com/{kind}/{id}")),
        kind,
        cover,
        tracks,
    })
}

/// Per-track cover art via Spotify's public oEmbed endpoint (upgraded to 640px).
pub async fn track_art(id: &str) -> Result<Option<String>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(id) {
        return Ok(hit.clone());
    }
    let url = format!("https://open.spotify.com/oembed?url=https://open.spotify.com/track/{id}");
    let v: Value = http().get(url).send().await.map_err(net)?.json().await.map_err(net)?;
    let art = v["thumbnail_url"].as_str().map(|u| u.replace("ab67616d00001e02", "ab67616d0000b273"));
    cache.lock().unwrap().insert(id.to_string(), art.clone());
    Ok(art)
}

#[cfg(test)]
mod tests {
    use super::parse_refs;

    #[test]
    fn parses_spotify_drops() {
        let drop = "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=abc\nspotify:track:4uLU6hMCjMI75M1A2tKUQC";
        assert_eq!(
            parse_refs(drop),
            vec![
                ("playlist".into(), "37i9dQZF1DXcBWIGoYBM5M".into()),
                ("track".into(), "4uLU6hMCjMI75M1A2tKUQC".into())
            ]
        );
        assert_eq!(
            parse_refs("https://open.spotify.com/intl-de/album/4LH4d3cOWNNsVw41Gqt2kv"),
            vec![("album".into(), "4LH4d3cOWNNsVw41Gqt2kv".into())]
        );
        assert_eq!(
            parse_refs("spotify:user:someone:playlist:37i9dQZF1DXcBWIGoYBM5M"),
            vec![("playlist".into(), "37i9dQZF1DXcBWIGoYBM5M".into())]
        );
        assert!(parse_refs("hello world https://example.com").is_empty());
    }
}
