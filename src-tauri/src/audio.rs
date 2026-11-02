//! The audio engine: a dedicated thread that owns the output stream and up to a
//! few "decks" (one rodio Sink per track). Two decks overlap during a crossfade.
//! Pausing does a short turntable-style spin-down (pitch + volume), and every
//! deck carries a meter so the UI can breathe with the bass.

use std::{
    f32::consts::FRAC_PI_2,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering::Relaxed},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use rodio::{source::SeekError, OutputStream, OutputStreamHandle, Sink, Source};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::{decode::FileSource, loudness};

const TICK: Duration = Duration::from_millis(10);
const EMIT_EVERY: Duration = Duration::from_millis(33);
const SPIN_DOWN: Duration = Duration::from_millis(320);
const SPIN_UP: Duration = Duration::from_millis(260);

type Reply = oneshot::Sender<Result<Loaded, String>>;

pub enum Cmd {
    Load { path: String, start: f64, autoplay: bool, reply: Reply },
    Crossfade { path: String, secs: f64, reply: Reply },
    Play,
    Pause,
    Seek(f64),
    Volume(f32),
    /// Playback rate, like a turntable's pitch control: slower is also lower.
    Rate(f32),
    Normalize(bool),
    Stop,
}

#[derive(Serialize, Clone, Copy)]
pub struct Loaded {
    pub deck: u64,
    pub duration: f64,
}

#[derive(Serialize, Clone)]
struct Tick {
    deck: u64,
    pos: f64,
    dur: f64,
    playing: bool,
    level: f32,
    bass: f32,
}

pub struct Engine {
    tx: mpsc::Sender<Cmd>,
}

impl Engine {
    pub fn spawn(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("plouk-audio".into())
            .spawn(move || Audio::new(app).run(rx))
            .expect("failed to start audio thread");
        Self { tx }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

// ---------------------------------------------------------------- ramps

#[derive(Clone, Copy)]
enum Shape {
    Linear,
    Smooth,
    EqualIn,
    EqualOut,
}

#[derive(Clone, Copy)]
struct Ramp {
    from: f32,
    to: f32,
    start: Instant,
    len: Duration,
    shape: Shape,
}

impl Ramp {
    fn hold(v: f32) -> Self {
        Self { from: v, to: v, start: Instant::now(), len: Duration::ZERO, shape: Shape::Linear }
    }

    fn new(from: f32, to: f32, len: Duration, shape: Shape) -> Self {
        Self { from, to, start: Instant::now(), len, shape }
    }

    fn progress(&self, now: Instant) -> f32 {
        if self.len.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.start).as_secs_f32() / self.len.as_secs_f32()).clamp(0.0, 1.0)
    }

    fn value(&self, now: Instant) -> f32 {
        let t = self.progress(now);
        let k = match self.shape {
            Shape::Linear => t,
            Shape::Smooth => t * t * (3.0 - 2.0 * t),
            Shape::EqualIn => (t * FRAC_PI_2).sin(),
            Shape::EqualOut => 1.0 - (t * FRAC_PI_2).cos(),
        };
        self.from + (self.to - self.from) * k
    }

    fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

// ---------------------------------------------------------------- meter

#[derive(Default)]
struct Levels {
    level: AtomicU32,
    bass: AtomicU32,
}

impl Levels {
    fn read(&self) -> (f32, f32) {
        (f32::from_bits(self.level.load(Relaxed)), f32::from_bits(self.bass.load(Relaxed)))
    }
}

/// Pass-through source that measures overall RMS and low-passed (bass) RMS.
struct Meter<S> {
    inner: S,
    levels: Arc<Levels>,
    channels: u16,
    ch: u16,
    mono: f32,
    lp: f32,
    alpha: f32,
    sum_all: f32,
    sum_bass: f32,
    n: u32,
    block: u32,
}

impl<S: Source<Item = f32>> Meter<S> {
    fn new(inner: S, levels: Arc<Levels>) -> Self {
        let rate = inner.sample_rate().max(1) as f32;
        let channels = inner.channels().max(1);
        Self {
            alpha: 1.0 - (-2.0 * std::f32::consts::PI * 140.0 / rate).exp(),
            block: (rate / 60.0) as u32,
            inner,
            levels,
            channels,
            ch: 0,
            mono: 0.0,
            lp: 0.0,
            sum_all: 0.0,
            sum_bass: 0.0,
            n: 0,
        }
    }
}

impl<S: Source<Item = f32>> Iterator for Meter<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let s = self.inner.next()?;
        self.mono += s;
        self.ch += 1;
        if self.ch >= self.channels {
            let m = self.mono / self.channels as f32;
            self.mono = 0.0;
            self.ch = 0;
            self.lp += self.alpha * (m - self.lp);
            self.sum_all += m * m;
            self.sum_bass += self.lp * self.lp;
            self.n += 1;
            if self.n >= self.block {
                let n = self.n as f32;
                self.levels.level.store((self.sum_all / n).sqrt().to_bits(), Relaxed);
                self.levels.bass.store((self.sum_bass / n).sqrt().to_bits(), Relaxed);
                self.sum_all = 0.0;
                self.sum_bass = 0.0;
                self.n = 0;
            }
        }
        Some(s)
    }
}

impl<S: Source<Item = f32>> Source for Meter<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.ch = 0;
        self.mono = 0.0;
        self.inner.try_seek(pos)
    }
}

// ---------------------------------------------------------------- engine

struct Deck {
    id: u64,
    sink: Sink,
    levels: Arc<Levels>,
    duration: f64,
    gain: Ramp,
    retiring: bool,
    path: PathBuf,
    /// Loudness correction (linear); fades in once the song has been measured.
    norm: Ramp,
    norm_pending: bool,
}

struct Audio {
    app: AppHandle,
    stream: Option<(OutputStream, OutputStreamHandle)>,
    decks: Vec<Deck>,
    current: Option<u64>,
    volume: f32,
    playing: bool,
    transport: Ramp,
    speed: Ramp,
    /// The user's rate, applied on top of the spin-up/spin-down speed.
    rate: Ramp,
    pausing_until: Option<Instant>,
    next_id: u64,
    last_emit: Instant,
    normalize: bool,
    last_norm_check: Instant,
}

impl Audio {
    fn new(app: AppHandle) -> Self {
        Self {
            app,
            stream: None,
            decks: Vec::new(),
            current: None,
            volume: 0.8,
            playing: false,
            transport: Ramp::hold(1.0),
            speed: Ramp::hold(1.0),
            rate: Ramp::hold(1.0),
            pausing_until: None,
            next_id: 1,
            last_emit: Instant::now(),
            normalize: true,
            last_norm_check: Instant::now(),
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(cmd) => self.handle(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(cmd) = rx.try_recv() {
                self.handle(cmd);
            }
            self.tick();
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        let now = Instant::now();
        match cmd {
            Cmd::Load { path, start, autoplay, reply } => {
                let res = self.load(&path, start, autoplay, now);
                let _ = reply.send(res);
            }
            Cmd::Crossfade { path, secs, reply } => {
                let res = if self.playing && self.current.is_some() {
                    self.crossfade(&path, secs, now)
                } else {
                    self.load(&path, 0.0, self.playing, now)
                };
                let _ = reply.send(res);
            }
            Cmd::Play => self.play(now),
            Cmd::Pause => self.pause(now),
            Cmd::Seek(secs) => {
                if let Some(deck) = self.current_deck() {
                    let max = if deck.duration > 1.0 { deck.duration - 0.4 } else { f64::MAX };
                    let to = secs.clamp(0.0, max);
                    let _ = deck.sink.try_seek(Duration::from_secs_f64(to));
                }
            }
            Cmd::Volume(v) => self.volume = v.clamp(0.0, 1.0),
            Cmd::Rate(r) => {
                self.rate = Ramp::new(self.rate.value(now), r.clamp(0.5, 2.0), Duration::from_millis(250), Shape::Smooth)
            }
            Cmd::Normalize(on) => self.normalize = on,
            Cmd::Stop => {
                for d in self.decks.drain(..) {
                    d.sink.stop();
                }
                self.current = None;
                self.playing = false;
            }
        }
    }

    fn handle_stream(&mut self) -> Result<OutputStreamHandle, String> {
        if self.stream.is_none() {
            let s = OutputStream::try_default().map_err(|e| format!("No audio output device: {e}"))?;
            self.stream = Some(s);
        }
        Ok(self.stream.as_ref().unwrap().1.clone())
    }

    fn open(&mut self, path: &str, gain: Ramp) -> Result<Deck, String> {
        let handle = self.handle_stream()?;
        // A broken file must never take the whole engine down with it.
        let decoder = catch_unwind(AssertUnwindSafe(|| FileSource::open(Path::new(path))))
            .map_err(|_| "Couldn't decode this audio file".to_string())??;
        let duration = decoder.total_duration().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let levels = Arc::new(Levels::default());
        let source = Meter::new(decoder, levels.clone());
        let sink = Sink::try_new(&handle).map_err(|e| format!("Audio output error: {e}"))?;
        sink.set_volume(0.0);
        sink.append(source);
        let id = self.next_id;
        self.next_id += 1;
        let path = PathBuf::from(path);
        let measured = loudness::cached_gain_db(&path);
        if measured.is_none() {
            loudness::spawn(&path);
        }
        let norm = Ramp::hold(measured.map(db_to_gain).unwrap_or(1.0));
        Ok(Deck { id, sink, levels, duration, gain, retiring: false, path, norm, norm_pending: measured.is_none() })
    }

    fn retire_all(&mut self, len: Duration, shape: Shape, now: Instant) {
        for d in self.decks.iter_mut() {
            if d.sink.is_paused() {
                d.sink.stop();
                d.retiring = true;
                d.gain = Ramp::hold(0.0);
                continue;
            }
            let v = d.gain.value(now);
            d.gain = Ramp::new(v, 0.0, len, shape);
            d.retiring = true;
        }
    }

    fn load(&mut self, path: &str, start: f64, autoplay: bool, now: Instant) -> Result<Loaded, String> {
        let gain = if autoplay {
            Ramp::new(0.0, 1.0, Duration::from_millis(40), Shape::Linear)
        } else {
            Ramp::hold(1.0)
        };
        let deck = self.open(path, gain)?;
        self.retire_all(Duration::from_millis(160), Shape::Linear, now);
        if !autoplay {
            deck.sink.pause();
        }
        if start > 0.3 {
            let _ = deck.sink.try_seek(Duration::from_secs_f64(start));
        }
        let loaded = Loaded { deck: deck.id, duration: deck.duration };
        self.current = Some(deck.id);
        self.decks.push(deck);
        self.playing = autoplay;
        self.pausing_until = None;
        self.speed = Ramp::hold(1.0);
        self.transport = Ramp::hold(if autoplay { 1.0 } else { 0.0 });
        Ok(loaded)
    }

    fn crossfade(&mut self, path: &str, secs: f64, now: Instant) -> Result<Loaded, String> {
        let len = Duration::from_secs_f64(secs.clamp(0.2, 20.0));
        let deck = self.open(path, Ramp::new(0.0, 1.0, len, Shape::EqualIn))?;
        self.retire_all(len, Shape::EqualOut, now);
        let loaded = Loaded { deck: deck.id, duration: deck.duration };
        self.current = Some(deck.id);
        self.decks.push(deck);
        Ok(loaded)
    }

    fn play(&mut self, now: Instant) {
        if self.current.is_none() || (self.playing && self.pausing_until.is_none()) {
            return;
        }
        for d in &self.decks {
            d.sink.play();
        }
        let t = self.transport.value(now);
        let s = self.speed.value(now).min(0.78);
        self.transport = Ramp::new(t, 1.0, Duration::from_millis(180), Shape::Linear);
        self.speed = Ramp::new(s, 1.0, SPIN_UP, Shape::Smooth);
        self.pausing_until = None;
        self.playing = true;
    }

    fn pause(&mut self, now: Instant) {
        if !self.playing {
            return;
        }
        let t = self.transport.value(now);
        self.transport = Ramp::new(t, 0.0, SPIN_DOWN, Shape::Smooth);
        self.speed = Ramp::new(1.0, 0.55, SPIN_DOWN, Shape::Linear);
        self.pausing_until = Some(now + SPIN_DOWN);
        self.playing = false;
    }

    fn current_deck(&self) -> Option<&Deck> {
        let id = self.current?;
        self.decks.iter().find(|d| d.id == id)
    }

    fn tick(&mut self) {
        let now = Instant::now();

        if let Some(until) = self.pausing_until {
            if now >= until {
                for d in &self.decks {
                    d.sink.pause();
                }
                self.speed = Ramp::hold(1.0);
                self.pausing_until = None;
            }
        }

        // Songs measured while already playing glide to their corrected level.
        if now.duration_since(self.last_norm_check) >= Duration::from_millis(400) {
            self.last_norm_check = now;
            for d in self.decks.iter_mut().filter(|d| d.norm_pending) {
                if let Some(db) = loudness::cached_gain_db(&d.path) {
                    d.norm = Ramp::new(d.norm.value(now), db_to_gain(db), Duration::from_millis(1500), Shape::Smooth);
                    d.norm_pending = false;
                }
            }
        }

        let master = self.volume * self.volume;
        let transport = self.transport.value(now);
        let speed = self.speed.value(now) * self.rate.value(now);
        for d in &self.decks {
            let norm = if self.normalize { d.norm.value(now) } else { 1.0 };
            d.sink.set_volume(master * d.gain.value(now) * transport * norm);
            d.sink.set_speed(speed);
        }

        // Drop decks that finished fading out.
        self.decks.retain(|d| {
            let finished = d.retiring && d.gain.done(now);
            if finished {
                d.sink.stop();
            }
            !finished
        });

        // Current track ran out.
        if let Some(id) = self.current {
            if let Some(pos) = self.decks.iter().position(|d| d.id == id) {
                if self.decks[pos].sink.empty() {
                    self.decks.remove(pos);
                    self.current = None;
                    self.playing = false;
                    let _ = self.app.emit("audio-ended", id);
                }
            }
        }

        // Paused, the position only needs to arrive now and then.
        let every = if self.playing { EMIT_EVERY } else { EMIT_EVERY * 15 };
        if now.duration_since(self.last_emit) >= every {
            self.last_emit = now;
            if let Some(d) = self.current_deck() {
                let (level, bass) = if self.playing { d.levels.read() } else { (0.0, 0.0) };
                let tick = Tick {
                    deck: d.id,
                    pos: d.sink.get_pos().as_secs_f64(),
                    dur: d.duration,
                    playing: self.playing,
                    level,
                    bass,
                };
                let _ = self.app.emit("audio", tick);
            }
        }
    }
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}
