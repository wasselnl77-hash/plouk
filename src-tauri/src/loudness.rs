//! "Even out volume": measures each song's integrated loudness (EBU R128 /
//! BS.1770 K-weighting with gating) and caches the gain that brings it to
//! -14 LUFS next to the file as `<song>.gain`. Runs on background threads.

use std::{
    collections::HashSet,
    f64::consts::PI,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    thread,
};

use rodio::Source;

use crate::decode::FileSource;

const TARGET_LUFS: f64 = -14.0;

pub fn sidecar(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".gain");
    PathBuf::from(p)
}

/// The cached gain in dB, if this song has been measured.
pub fn cached_gain_db(path: &Path) -> Option<f32> {
    std::fs::read_to_string(sidecar(path)).ok()?.trim().parse().ok()
}

/// Measures the song in the background unless it is cached or already being measured.
pub fn spawn(path: &Path) {
    static BUSY: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    if sidecar(path).exists() {
        return;
    }
    let busy = BUSY.get_or_init(Default::default);
    if !busy.lock().unwrap().insert(path.to_path_buf()) {
        return;
    }
    let path = path.to_path_buf();
    let _ = thread::Builder::new().name("plouk-loudness".into()).spawn(move || {
        if let Some(db) = measure_gain_db(&path) {
            let _ = std::fs::write(sidecar(&path), format!("{db:.2}"));
        }
        busy.lock().unwrap().remove(&path);
    });
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z1;
        self.z1 = self.b[1] * x - self.a[1] * y + self.z2;
        self.z2 = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// BS.1770 K-weighting (high shelf + RLB high-pass) for any sample rate.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Default::default()
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let highpass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Default::default()
    };
    [shelf, highpass]
}

fn measure_gain_db(path: &Path) -> Option<f32> {
    let (integrated, peak) = measure(path)?;
    // Never push the loudest sample past full scale.
    let headroom = if peak > 0.0 { 20.0 * (0.98 / peak as f64).log10() } else { 0.0 };
    Some((TARGET_LUFS - integrated).clamp(-15.0, 9.0).min(headroom) as f32)
}

/// Integrated loudness (LUFS) and sample peak.
fn measure(path: &Path) -> Option<(f64, f32)> {
    let src = FileSource::open(path).ok()?;
    let channels = src.channels().max(1) as usize;
    let rate = src.sample_rate().max(1) as f64;
    let mut filters = vec![k_weighting(rate); channels];
    let sub_len = (rate * 0.1) as usize; // 100 ms; 400 ms blocks overlap by 75 %
    let mut subs: Vec<f64> = Vec::new();
    let (mut acc, mut frames, mut ch, mut peak) = (0.0f64, 0usize, 0usize, 0f32);

    for s in src {
        peak = peak.max(s.abs());
        let [shelf, hp] = &mut filters[ch];
        let y = hp.run(shelf.run(s as f64));
        acc += y * y;
        ch += 1;
        if ch == channels {
            ch = 0;
            frames += 1;
            if frames == sub_len {
                subs.push(acc / sub_len as f64);
                acc = 0.0;
                frames = 0;
            }
        }
    }

    let blocks: Vec<f64> = subs.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).collect();
    let lufs = |z: f64| -0.691 + 10.0 * z.log10();
    let above = |gate: f64| -> Vec<f64> { blocks.iter().copied().filter(|&z| z > 0.0 && lufs(z) > gate).collect() };
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;

    let absolute = above(-70.0);
    if absolute.is_empty() {
        return Some((TARGET_LUFS, peak));
    }
    let relative_gate = lufs(mean(&absolute)) - 10.0;
    let gated = above(relative_gate.max(-70.0));
    Some((lufs(mean(&gated)), peak))
}

#[cfg(test)]
mod tests {
    use super::{measure, measure_gain_db};

    /// Run with PLOUK_TEST_AUDIO=/path/to/song.m4a and optionally PLOUK_TEST_LUFS=<ffmpeg ebur128 I:>.
    #[test]
    fn measures_a_real_song() {
        let Ok(path) = std::env::var("PLOUK_TEST_AUDIO") else { return };
        let (lufs, peak) = measure(path.as_ref()).expect("measured");
        let db = measure_gain_db(path.as_ref()).expect("gain");
        eprintln!("integrated {lufs:.2} LUFS, peak {peak:.3}, gain {db:.2} dB");
        assert!((-15.0..=9.0).contains(&db), "gain {db}");
        if let Some(reference) = std::env::var("PLOUK_TEST_LUFS").ok().and_then(|v| v.parse::<f64>().ok()) {
            assert!((lufs - reference).abs() < 0.5, "{lufs:.2} LUFS vs reference {reference}");
        }
    }
}
