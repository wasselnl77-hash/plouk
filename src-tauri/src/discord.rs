//! "Listening to Plouk" on the user's Discord profile. It talks to the Discord
//! app running on the same computer (no login, nothing sent to a server of
//! ours), on its own thread. Discord rate-limits presence updates, so it's only
//! told when something it shows changes: the song, play/pause, or a seek.

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use discord_rich_presence::{
    activity::{Activity, ActivityType, Assets, StatusDisplayType, Timestamps},
    DiscordIpc, DiscordIpcClient,
};

/// Plouk's application on discord.com/developers. It's what names the activity
/// "Plouk"; it isn't a secret. Empty turns the feature off.
const APP_ID: &str = "";

pub enum Msg {
    Track { title: String, artist: String, album: String, cover: Option<String>, duration: f64 },
    Playback { playing: bool, pos: f64 },
    Enabled(bool),
}

pub struct Discord(Option<mpsc::Sender<Msg>>);

impl Discord {
    pub fn spawn() -> Self {
        if APP_ID.is_empty() {
            return Self(None);
        }
        let (tx, rx) = mpsc::channel();
        let started = thread::Builder::new().name("plouk-discord".into()).spawn(move || Presence::default().run(rx));
        Self(started.ok().map(|_| tx))
    }

    pub fn send(&self, msg: Msg) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(msg);
        }
    }
}

struct Track {
    title: String,
    artist: String,
    album: String,
    cover: Option<String>,
    duration: f64,
}

struct Presence {
    client: Option<DiscordIpcClient>,
    last_try: Option<Instant>,
    enabled: bool,
    track: Option<Track>,
    playing: bool,
    /// When the song would have started if it had played straight through (unix ms).
    start_ms: i64,
    /// What Discord is showing: song title and its start time.
    shown: Option<(String, i64)>,
}

impl Default for Presence {
    fn default() -> Self {
        // Off until the page says otherwise: showing what you listen to is opt-in.
        Self { client: None, last_try: None, enabled: false, track: None, playing: false, start_ms: 0, shown: None }
    }
}

impl Presence {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(msg) => self.apply(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(msg) = rx.try_recv() {
                self.apply(msg);
            }
            self.sync();
        }
        if let Some(mut c) = self.client.take() {
            let _ = c.close();
        }
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Track { title, artist, album, cover, duration } => {
                self.track = Some(Track { title, artist, album, cover, duration });
            }
            Msg::Playback { playing, pos } => {
                self.playing = playing;
                self.start_ms = now_ms() - (pos * 1000.0) as i64;
            }
            Msg::Enabled(on) => self.enabled = on,
        }
    }

    fn sync(&mut self) {
        let want = self.enabled && self.playing && self.track.is_some();
        if !want {
            if self.shown.take().is_some() {
                if let Some(c) = self.client.as_mut() {
                    let _ = c.clear_activity();
                }
            }
            return;
        }
        // The regular position updates drift a little; only a real jump (a seek) is worth a resend.
        if let (Some((title, start)), Some(track)) = (&self.shown, &self.track) {
            if *title == track.title && (start - self.start_ms).abs() < 2500 {
                return;
            }
        }
        if !self.connect() {
            return;
        }
        let track = self.track.as_ref().unwrap();
        let mut assets = Assets::new().large_text(&track.album);
        if let Some(cover) = &track.cover {
            assets = assets.large_image(cover);
        }
        let mut times = Timestamps::new().start(self.start_ms);
        if track.duration > 1.0 {
            times = times.end(self.start_ms + (track.duration * 1000.0) as i64);
        }
        let activity = Activity::new()
            .activity_type(ActivityType::Listening)
            .status_display_type(StatusDisplayType::Details)
            .details(&track.title)
            .state(&track.artist)
            .assets(assets)
            .timestamps(times);
        match self.client.as_mut().unwrap().set_activity(activity) {
            Ok(()) => self.shown = Some((track.title.clone(), self.start_ms)),
            // Discord went away; try again on a later update.
            Err(_) => self.client = None,
        }
    }

    /// Connects to the Discord app if it's running, at most every 15 seconds.
    fn connect(&mut self) -> bool {
        if self.client.is_some() {
            return true;
        }
        if self.last_try.is_some_and(|t| t.elapsed() < Duration::from_secs(15)) {
            return false;
        }
        self.last_try = Some(Instant::now());
        let mut client = DiscordIpcClient::new(APP_ID);
        if client.connect().is_ok() {
            self.client = Some(client);
            self.shown = None;
            true
        } else {
            false
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
