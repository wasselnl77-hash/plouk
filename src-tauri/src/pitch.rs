//! The pitch control, with Signalsmith Stretch. It shifts pitch without
//! touching the tempo (as many samples come out as go in); the turntable-style
//! speed is applied after it, by the sink. At 0 semitones the audio passes
//! straight through and the stretcher isn't even created.
//!
//! It also keeps the song position. rodio's own `get_pos` is measured after
//! the speed stage, so with the speed changed it drifts away from the song (and
//! the lyrics with it). Here every sample of the song goes past, so counting
//! them gives the real position at any speed.

use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering::Relaxed},
        Arc,
    },
    time::Duration,
};

use rodio::{source::SeekError, Source};
use signalsmith_stretch::Stretch;

/// Frames per processing block (about 12 ms).
const BLOCK: usize = 512;
/// Audio kept from just before a shift starts, to prime the stretcher so it starts smoothly.
const HISTORY: usize = 8192;

pub struct Pitch<S> {
    inner: S,
    /// Semitones as f32 bits, shared with the engine.
    shift: Arc<AtomicU32>,
    /// Song position in seconds (f64 bits), read by the engine.
    position: Arc<AtomicU64>,
    /// Samples read from the song so far, counted from `base`.
    consumed: u64,
    base: f64,
    stretch: Option<Stretch>,
    active: bool,
    current: f32,
    channels: usize,
    rate: u32,
    input: Vec<f32>,
    output: Vec<f32>,
    out_pos: usize,
    history: Vec<f32>,
    ended: bool,
}

impl<S: Source<Item = f32>> Pitch<S> {
    pub fn new(inner: S, shift: Arc<AtomicU32>, position: Arc<AtomicU64>) -> Self {
        let channels = inner.channels().max(1) as usize;
        let rate = inner.sample_rate().max(1);
        Self {
            inner,
            shift,
            position,
            consumed: 0,
            base: 0.0,
            stretch: None,
            active: false,
            current: 0.0,
            channels,
            rate,
            input: Vec::with_capacity(BLOCK * channels),
            output: Vec::with_capacity(BLOCK * channels),
            out_pos: 0,
            history: Vec::with_capacity(HISTORY * channels),
            ended: false,
        }
    }

    /// Reads the next block from the song and runs it through (or past) the stretcher.
    fn refill(&mut self) -> bool {
        self.input.clear();
        while self.input.len() < BLOCK * self.channels {
            match self.inner.next() {
                Some(s) => self.input.push(s),
                None => break,
            }
        }
        let frames = self.input.len() / self.channels;
        self.input.truncate(frames * self.channels);
        self.consumed += self.input.len() as u64;
        let secs = self.base + self.consumed as f64 / (self.rate as f64 * self.channels as f64);
        self.position.store(secs.to_bits(), Relaxed);
        self.output.clear();
        self.out_pos = 0;

        let want = f32::from_bits(self.shift.load(Relaxed));
        let wanted = want.abs() >= 0.01;

        if frames == 0 {
            // The song is over: let the stretcher's last few milliseconds out once.
            if self.active && !self.ended {
                self.ended = true;
                let stretch = self.stretch.as_mut().unwrap();
                self.output.resize(stretch.output_latency() * self.channels, 0.0);
                stretch.flush(&mut self.output);
                return !self.output.is_empty();
            }
            return false;
        }

        if wanted && !self.active {
            let (channels, rate) = (self.channels as u32, self.rate);
            let stretch = self.stretch.get_or_insert_with(|| Stretch::preset_cheaper(channels, rate));
            stretch.reset();
            stretch.seek(&self.history, 1.0);
            self.active = true;
            self.current = f32::NAN;
        } else if !wanted && self.active {
            self.active = false;
        }

        if self.active {
            let stretch = self.stretch.as_mut().unwrap();
            if self.current != want {
                stretch.set_transpose_factor_semitones(want, None);
                self.current = want;
            }
            self.output.resize(self.input.len(), 0.0);
            stretch.process(&self.input, &mut self.output);
        } else {
            self.output.extend_from_slice(&self.input);
        }

        // Keep the latest audio around for the next time a shift starts.
        self.history.extend_from_slice(&self.input);
        let max = HISTORY * self.channels;
        if self.history.len() > max {
            self.history.drain(..self.history.len() - max);
        }
        true
    }
}

impl<S: Source<Item = f32>> Iterator for Pitch<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.out_pos >= self.output.len() && !self.refill() {
            return None;
        }
        let s = self.output[self.out_pos];
        self.out_pos += 1;
        Some(s)
    }
}

impl<S: Source<Item = f32>> Source for Pitch<S> {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels as u16
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)?;
        self.base = pos.as_secs_f64();
        self.consumed = 0;
        self.position.store(self.base.to_bits(), Relaxed);
        // Start fresh from the new spot; the stretcher re-primes on the next block if needed.
        self.output.clear();
        self.out_pos = 0;
        self.history.clear();
        self.active = false;
        self.ended = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::source::SineWave;

    /// Frequency from the strongest autocorrelation lag, in the middle of the samples.
    fn freq(samples: &[f32], rate: f32) -> f32 {
        let mid = samples.len() / 2;
        let s = &samples[mid..mid + 4096];
        let corr = |lag: usize| s[..4096 - lag].iter().zip(&s[lag..]).map(|(a, b)| a * b).sum::<f32>();
        let lag = (30..400).max_by(|&a, &b| corr(a).total_cmp(&corr(b))).unwrap();
        rate / lag as f32
    }

    fn run(semitones: f32) -> (usize, f32) {
        let tone = SineWave::new(440.0).take_duration(Duration::from_secs(2));
        let out: Vec<f32> = Pitch::new(tone, Arc::new(AtomicU32::new(semitones.to_bits())), Default::default()).collect();
        (out.len(), freq(&out, 48_000.0))
    }

    #[test]
    fn passes_through_at_zero_and_shifts_an_octave() {
        let (n0, f0) = run(0.0);
        let (n12, f12) = run(12.0);
        assert!((f0 - 440.0).abs() < 5.0, "untouched tone at {f0} Hz");
        assert!((f12 - 880.0).abs() < 15.0, "octave up at {f12} Hz");
        // Same length, give or take the stretcher's tail: the tempo doesn't change.
        assert!((n12 as i64 - n0 as i64).abs() < 48_000 / 5, "{n0} vs {n12} samples");
    }
}
