//! HDMI live sound: find the capture card's sound input and record it to a
//! WAV file (stereo, at the card's rate, usually 48 kHz).
//!
//! The recorder opens its own input stream, so it works the same whether or
//! not Studio is also playing the sound. Samples go from the audio callback
//! through a channel to a writer thread; the WAV header is brought up to date
//! twice a second, so the file stays playable even if the program is killed.
//!
//! On macOS the sound input counts as a microphone: a command-line program
//! started from a shell without microphone permission hangs while opening
//! the input (or records silence). Record
//! through the Studio app (`m65-studio --record-audio`) to use its permission.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// The sound input whose name contains `hint` (case-insensitive); otherwise
/// the first one whose name looks like a capture card.
///
/// Devices are matched by name before anything is opened: cpal's
/// `input_devices()` opens every audio device to ask whether it has inputs,
/// and on macOS some (an iPhone's Continuity microphone, for one) can block
/// that call indefinitely.
pub fn find_input(hint: &str) -> anyhow::Result<cpal::Device> {
    let host = cpal::default_host();
    let hint = hint.to_ascii_lowercase();
    let words = ["ugreen", "capture", "hdmi", "macrosilicon", "cam link", "elgato", "usb video"];
    let mut by_hint = Vec::new();
    let mut by_word = Vec::new();
    for d in host.devices()? {
        let n = d.name().unwrap_or_default().to_ascii_lowercase();
        if !hint.is_empty() && n.contains(&hint) {
            by_hint.push(d);
        } else if words.iter().any(|w| n.contains(w)) {
            by_word.push(d);
        }
    }
    by_hint
        .into_iter()
        .chain(by_word)
        .find(|d| d.default_input_config().is_ok())
        .ok_or_else(|| anyhow::anyhow!("the capture card has no sound input on this computer"))
}

/// Names of all audio devices (inputs and outputs; not probed, see
/// [`find_input`]), for `m65rs live list`.
pub fn device_names() -> anyhow::Result<Vec<String>> {
    Ok(cpal::default_host().devices()?.map(|d| d.name().unwrap_or_default()).collect())
}

/// Sample format written to the WAV file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WavFormat {
    /// 16-bit integer PCM (what HDMI from the MEGA65 carries; plays anywhere).
    #[default]
    Pcm16,
    /// 32-bit float, exactly the samples the computer delivered.
    Float32,
}

/// A stereo WAV file being written. Sizes in the header are kept current by
/// [`WavWriter::update_header`] and [`WavWriter::finish`].
pub struct WavWriter {
    out: BufWriter<File>,
    format: WavFormat,
    frames: u64,
}

impl WavWriter {
    pub fn create(path: &Path, rate: u32, format: WavFormat) -> anyhow::Result<Self> {
        let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut w = Self { out: BufWriter::new(f), format, frames: 0 };
        let (tag, bits): (u16, u16) = match format {
            WavFormat::Pcm16 => (1, 16),
            WavFormat::Float32 => (3, 32),
        };
        let block = 2 * bits / 8;
        let o = &mut w.out;
        o.write_all(b"RIFF")?;
        o.write_all(&0u32.to_le_bytes())?;
        o.write_all(b"WAVE")?;
        o.write_all(b"fmt ")?;
        o.write_all(&16u32.to_le_bytes())?;
        o.write_all(&tag.to_le_bytes())?;
        o.write_all(&2u16.to_le_bytes())?;
        o.write_all(&rate.to_le_bytes())?;
        o.write_all(&(rate * block as u32).to_le_bytes())?;
        o.write_all(&block.to_le_bytes())?;
        o.write_all(&bits.to_le_bytes())?;
        o.write_all(b"data")?;
        o.write_all(&0u32.to_le_bytes())?;
        Ok(w)
    }

    pub fn write(&mut self, frames: &[[f32; 2]]) -> std::io::Result<()> {
        for f in frames {
            for &s in f {
                match self.format {
                    WavFormat::Pcm16 => self.out.write_all(&to_i16(s).to_le_bytes())?,
                    WavFormat::Float32 => self.out.write_all(&s.to_le_bytes())?,
                }
            }
        }
        self.frames += frames.len() as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Write the current sizes into the header (the file stays valid if the
    /// program stops without calling `finish`).
    pub fn update_header(&mut self) -> std::io::Result<()> {
        let bytes = match self.format {
            WavFormat::Pcm16 => 4,
            WavFormat::Float32 => 8,
        };
        let data = (self.frames * bytes).min(u32::MAX as u64 - 36) as u32;
        self.out.flush()?;
        let f = self.out.get_mut();
        f.seek(SeekFrom::Start(4))?;
        f.write_all(&(36 + data).to_le_bytes())?;
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&data.to_le_bytes())?;
        f.seek(SeekFrom::End(0))?;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<u64> {
        self.update_header()?;
        self.out.get_ref().sync_all()?;
        Ok(self.frames)
    }
}

/// Same scaling as cpal's i16 → f32 (÷32768), so 16-bit input round-trips exactly.
fn to_i16(s: f32) -> i16 {
    (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

/// What to record.
#[derive(Clone, Debug, Default)]
pub struct RecordOptions {
    /// Part of the sound input's name ("" = first capture card).
    pub hint: String,
    pub format: WavFormat,
    /// Stop after this long (counted from the first sample written).
    pub max_seconds: Option<f64>,
    /// Write nothing until a sample is at least this loud (0..1), so the
    /// file starts exactly where the sound starts.
    pub wait_for_sound: Option<f32>,
}

/// Result of a finished recording.
#[derive(Clone, Debug)]
pub struct RecordSummary {
    pub path: PathBuf,
    pub device: String,
    pub rate: u32,
    pub frames: u64,
    /// Loudest sample (0..1); 0 means the file is silent.
    pub peak: f32,
}

impl RecordSummary {
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / self.rate.max(1) as f64
    }
}

struct Status {
    frames: AtomicU64,
    peak: AtomicU32,
    level: AtomicU32,
    waiting: AtomicBool,
    done: AtomicBool,
    error: Mutex<Option<String>>,
}

/// A recording in progress. Dropping it stops and finishes the file.
pub struct AudioRecorder {
    stream: Option<cpal::Stream>,
    writer: Option<JoinHandle<anyhow::Result<u64>>>,
    status: Arc<Status>,
    pub path: PathBuf,
    pub device: String,
    pub rate: u32,
}

impl AudioRecorder {
    pub fn start(path: impl Into<PathBuf>, opts: &RecordOptions) -> anyhow::Result<Self> {
        let path = path.into();
        let dev = find_input(&opts.hint)?;
        let device = dev.name().unwrap_or_default();
        let cfg = dev.default_input_config()?;
        let rate = cfg.sample_rate().0;
        let mut wav = WavWriter::create(&path, rate, opts.format)?;
        let status = Arc::new(Status {
            frames: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            level: AtomicU32::new(0),
            waiting: AtomicBool::new(opts.wait_for_sound.is_some()),
            done: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel::<Vec<[f32; 2]>>();
        let st = status.clone();
        let max_frames = opts.max_seconds.map(|s| (s * rate as f64).round() as u64);
        let mut threshold = opts.wait_for_sound;
        let writer = std::thread::Builder::new().name("wav-writer".into()).spawn(move || -> anyhow::Result<u64> {
            let mut last_header = Instant::now();
            let mut peak = 0f32;
            for mut chunk in rx {
                if let Some(t) = threshold {
                    match chunk.iter().position(|f| f[0].abs() >= t || f[1].abs() >= t) {
                        Some(i) => {
                            chunk.drain(..i);
                            threshold = None;
                            st.waiting.store(false, Ordering::Relaxed);
                        }
                        None => continue,
                    }
                }
                if let Some(m) = max_frames {
                    let room = m.saturating_sub(wav.frames()) as usize;
                    chunk.truncate(room);
                }
                for f in &chunk {
                    peak = peak.max(f[0].abs()).max(f[1].abs());
                }
                if let Err(e) = wav.write(&chunk) {
                    *st.error.lock().unwrap() = Some(format!("writing: {e}"));
                    break;
                }
                st.frames.store(wav.frames(), Ordering::Relaxed);
                st.peak.store(peak.to_bits(), Ordering::Relaxed);
                if max_frames.is_some_and(|m| wav.frames() >= m) {
                    break;
                }
                if last_header.elapsed() >= Duration::from_millis(500) {
                    wav.update_header()?;
                    last_header = Instant::now();
                }
            }
            st.done.store(true, Ordering::Relaxed);
            Ok(wav.finish()?)
        })?;
        let st = status.clone();
        let stream = match cfg.sample_format() {
            SampleFormat::F32 => build::<f32>(&dev, &cfg.config(), tx, st),
            SampleFormat::I16 => build::<i16>(&dev, &cfg.config(), tx, st),
            SampleFormat::U16 => build::<u16>(&dev, &cfg.config(), tx, st),
            SampleFormat::I32 => build::<i32>(&dev, &cfg.config(), tx, st),
            f => Err(anyhow::anyhow!("unsupported input sample format {f}")),
        }?;
        stream.play()?;
        Ok(Self { stream: Some(stream), writer: Some(writer), status, path, device, rate })
    }

    /// Seconds written so far.
    pub fn seconds(&self) -> f64 {
        self.status.frames.load(Ordering::Relaxed) as f64 / self.rate.max(1) as f64
    }

    /// Still waiting for the first sound (with `wait_for_sound`).
    pub fn waiting(&self) -> bool {
        self.status.waiting.load(Ordering::Relaxed)
    }

    /// The writer has stopped (time limit reached, or an error).
    pub fn finished(&self) -> bool {
        self.status.done.load(Ordering::Relaxed)
    }

    /// Recent input level 0..1 (peak, decaying), for a meter.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.status.level.load(Ordering::Relaxed))
    }

    pub fn error(&self) -> Option<String> {
        self.status.error.lock().ok().and_then(|e| e.clone())
    }

    /// Stop recording and finish the file.
    pub fn stop(mut self) -> anyhow::Result<RecordSummary> {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> anyhow::Result<RecordSummary> {
        // Dropping the stream drops the sender, which ends the writer's loop.
        self.stream = None;
        let frames = match self.writer.take() {
            Some(w) => w.join().map_err(|_| anyhow::anyhow!("WAV writer panicked"))??,
            None => self.status.frames.load(Ordering::Relaxed),
        };
        if let Some(e) = self.error() {
            anyhow::bail!("{e}");
        }
        Ok(RecordSummary {
            path: self.path.clone(),
            device: self.device.clone(),
            rate: self.rate,
            frames,
            peak: f32::from_bits(self.status.peak.load(Ordering::Relaxed)),
        })
    }
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        if self.writer.is_some() {
            let _ = self.stop_inner();
        }
    }
}

fn build<T>(dev: &cpal::Device, cfg: &cpal::StreamConfig, tx: mpsc::Sender<Vec<[f32; 2]>>, st: Arc<Status>) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = (cfg.channels as usize).max(1);
    let err_st = st.clone();
    Ok(dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            let mut v = Vec::with_capacity(data.len() / ch);
            let mut peak = 0f32;
            for f in data.chunks(ch) {
                let l = <f32 as FromSample<T>>::from_sample_(f[0]);
                let r = if ch > 1 { <f32 as FromSample<T>>::from_sample_(f[1]) } else { l };
                peak = peak.max(l.abs()).max(r.abs());
                v.push([l, r]);
            }
            let old = f32::from_bits(st.level.load(Ordering::Relaxed));
            st.level.store(peak.max(old * 0.9).to_bits(), Ordering::Relaxed);
            let _ = tx.send(v); // fails only once the writer has stopped
        },
        move |e| {
            if let Ok(mut s) = err_st.error.lock() {
                s.get_or_insert_with(|| format!("sound input: {e}"));
            }
        },
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_samples() {
        let dir = std::env::temp_dir().join(format!("m65-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let mut w = WavWriter::create(&p, 48000, WavFormat::Pcm16).unwrap();
        w.write(&[[0.0, -1.0], [0.5, 1.0]]).unwrap();
        w.update_header().unwrap();
        w.write(&[[-0.5, 0.25]]).unwrap();
        assert_eq!(w.finish().unwrap(), 3);
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 36 + 12);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 48000);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 12);
        let s: Vec<i16> = b[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(s, vec![0, -32768, 16384, 32767, -16384, 8192]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn i16_round_trip() {
        for i in [-32768i16, -1, 0, 1, 12345, 32767] {
            assert_eq!(to_i16(<f32 as FromSample<i16>>::from_sample_(i)), i);
        }
    }
}
