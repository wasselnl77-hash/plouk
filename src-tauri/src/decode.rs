//! A small symphonia-backed rodio Source. It hands symphonia the real file
//! (so the byte length is known and MP4s with the index at the end open fine),
//! seeks sample-accurately, and treats any decode trouble as end-of-track
//! instead of panicking.

use std::{fs::File, path::Path, time::Duration};

use rodio::{source::SeekError, Source};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL},
    errors::Error,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
    units::{Time, TimeBase},
};

pub struct FileSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    samples: Vec<f32>,
    pos: usize,
    channels: u16,
    rate: u32,
    duration: Option<Duration>,
}

fn to_duration(t: Time) -> Duration {
    Duration::from_secs(t.seconds) + Duration::from_secs_f64(t.frac.clamp(0.0, 0.999_999_999))
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Can't open audio file: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let opts = FormatOptions { enable_gapless: true, ..Default::default() };
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &opts, &MetadataOptions::default())
            .map_err(|e| format!("Unsupported audio file: {e}"))?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("The file has no audio track")?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| format!("Unsupported audio codec: {e}"))?;
        let duration = params.time_base.zip(params.n_frames).map(|(tb, n)| to_duration(tb.calc_time(n)));

        let mut source = Self {
            format,
            decoder,
            track_id,
            time_base: params.time_base,
            samples: Vec::new(),
            pos: 0,
            channels: params.channels.map(|c| c.count() as u16).unwrap_or(2),
            rate: params.sample_rate.unwrap_or(44_100),
            duration,
        };
        if !source.refill() {
            return Err("Couldn't decode any audio from the file".into());
        }
        Ok(source)
    }

    /// Decodes the next packet into the sample buffer. Returns false at the end.
    fn refill(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(_) => return false,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    buf.copy_interleaved_ref(decoded);
                    self.channels = spec.channels.count().max(1) as u16;
                    self.rate = spec.rate;
                    self.samples.clear();
                    self.samples.extend_from_slice(buf.samples());
                    self.pos = 0;
                    if !self.samples.is_empty() {
                        return true;
                    }
                }
                Err(Error::DecodeError(_)) => continue,
                Err(_) => return false,
            }
        }
    }
}

impl Iterator for FileSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.samples.len() && !self.refill() {
            return None;
        }
        let s = self.samples[self.pos];
        self.pos += 1;
        Some(s)
    }
}

impl Source for FileSource {
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.samples.len())
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let time = Time::new(pos.as_secs(), pos.subsec_nanos() as f64 / 1e9);
        let seeked = self
            .format
            .seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track_id) })
            .map_err(|e| SeekError::Other(Box::new(e)))?;
        self.decoder.reset();
        self.samples.clear();
        self.pos = 0;

        // The demuxer lands on a packet boundary; drop samples up to the exact spot.
        let late = seeked.required_ts.saturating_sub(seeked.actual_ts);
        let mut skip = match self.time_base {
            Some(tb) => {
                let t = tb.calc_time(late);
                ((t.seconds as f64 + t.frac) * self.rate as f64) as usize * self.channels as usize
            }
            None => 0,
        };
        while skip > 0 && self.refill() {
            let n = skip.min(self.samples.len());
            self.pos = n;
            skip -= n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::FileSource;
    use rodio::Source;
    use std::time::Duration;

    /// Run with PLOUK_TEST_AUDIO=/path/to/song.m4a cargo test
    #[test]
    fn decodes_and_seeks() {
        let Ok(path) = std::env::var("PLOUK_TEST_AUDIO") else { return };
        let mut src = FileSource::open(path.as_ref()).expect("open");
        let dur = src.total_duration().expect("duration");
        assert!(dur > Duration::from_secs(30), "duration {dur:?}");
        let first: Vec<f32> = src.by_ref().take(44_100 * 2).collect();
        assert!(first.iter().any(|s| s.abs() > 0.01), "first second is silent");
        src.try_seek(Duration::from_secs(120)).expect("seek");
        let n = src.by_ref().take(44_100 * 2).filter(|s| s.abs() > 0.001).count();
        assert!(n > 1000, "audio after seek: {n}");
        let rest = src.count() as f64 / (44_100.0 * 2.0);
        assert!((rest - (dur.as_secs_f64() - 121.0)).abs() < 2.0, "remaining {rest}s vs duration {dur:?}");
    }
}
