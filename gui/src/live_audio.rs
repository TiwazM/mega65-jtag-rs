//! HDMI live sound: the capture card's audio input played on this computer's
//! speakers, with a short buffer (~50 ms) between the two so the sound keeps
//! roughly in step with the picture.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// Aim for this much sound waiting between input and output.
const TARGET_MS: u32 = 50;
/// Drop the oldest sound beyond this, so a stall can't build up a lag.
const MAX_MS: u32 = 150;

struct Shared {
    /// Interleaved stereo, at the input's sample rate.
    buf: VecDeque<[f32; 2]>,
    /// Output is waiting for the buffer to fill to TARGET_MS again.
    priming: bool,
    /// Loudest recent input level (0..1), for the meter.
    level: f32,
}

pub struct LiveAudio {
    _input: cpal::Stream,
    _output: cpal::Stream,
    shared: Arc<Mutex<Shared>>,
    volume: Arc<AtomicU32>,
}

/// Input devices whose name contains `hint` (case-insensitive) come first.
pub fn find_input(hint: &str) -> anyhow::Result<cpal::Device> {
    let host = cpal::default_host();
    let hint = hint.to_ascii_lowercase();
    let words = ["ugreen", "capture", "hdmi", "macrosilicon", "cam link", "elgato", "usb video"];
    let mut fallback = None;
    for d in host.input_devices()? {
        let n = d.name().unwrap_or_default().to_ascii_lowercase();
        if !hint.is_empty() && n.contains(&hint) {
            return Ok(d);
        }
        if fallback.is_none() && words.iter().any(|w| n.contains(w)) {
            fallback = Some(d);
        }
    }
    fallback.ok_or_else(|| anyhow::anyhow!("the capture card has no sound input on this computer"))
}

impl LiveAudio {
    /// Start playing the input whose name contains `hint` (the video device's
    /// name usually matches its sound input), at `volume` (0..1).
    pub fn start(hint: &str, volume: f32) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let input = find_input(hint)?;
        let output = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no sound output on this computer"))?;
        let in_cfg = input.default_input_config()?;
        let out_cfg = output.default_output_config()?;
        let in_rate = in_cfg.sample_rate().0;
        let out_rate = out_cfg.sample_rate().0;
        let shared = Arc::new(Mutex::new(Shared { buf: VecDeque::new(), priming: true, level: 0.0 }));
        let vol = Arc::new(AtomicU32::new(volume.to_bits()));
        let in_stream = match in_cfg.sample_format() {
            SampleFormat::F32 => build_input::<f32>(&input, &in_cfg.config(), shared.clone(), in_rate),
            SampleFormat::I16 => build_input::<i16>(&input, &in_cfg.config(), shared.clone(), in_rate),
            SampleFormat::U16 => build_input::<u16>(&input, &in_cfg.config(), shared.clone(), in_rate),
            SampleFormat::I32 => build_input::<i32>(&input, &in_cfg.config(), shared.clone(), in_rate),
            f => anyhow::bail!("unsupported input sample format {f}"),
        }?;
        let out_stream = match out_cfg.sample_format() {
            SampleFormat::F32 => build_output::<f32>(&output, &out_cfg.config(), shared.clone(), vol.clone(), in_rate, out_rate),
            SampleFormat::I16 => build_output::<i16>(&output, &out_cfg.config(), shared.clone(), vol.clone(), in_rate, out_rate),
            SampleFormat::U16 => build_output::<u16>(&output, &out_cfg.config(), shared.clone(), vol.clone(), in_rate, out_rate),
            SampleFormat::I32 => build_output::<i32>(&output, &out_cfg.config(), shared.clone(), vol.clone(), in_rate, out_rate),
            f => anyhow::bail!("unsupported output sample format {f}"),
        }?;
        in_stream.play()?;
        out_stream.play()?;
        Ok(Self { _input: in_stream, _output: out_stream, shared, volume: vol })
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Recent input level 0..1 (peak, decaying), for a meter.
    pub fn level(&self) -> f32 {
        self.shared.lock().map(|s| s.level).unwrap_or(0.0)
    }
}

fn build_input<T>(dev: &cpal::Device, cfg: &cpal::StreamConfig, shared: Arc<Mutex<Shared>>, rate: u32) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = cfg.channels as usize;
    let max = (rate * MAX_MS / 1000) as usize;
    Ok(dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            let Ok(mut s) = shared.lock() else { return };
            let mut peak = 0f32;
            for f in data.chunks(ch.max(1)) {
                let l = <f32 as FromSample<T>>::from_sample_(f[0]);
                let r = if ch > 1 { <f32 as FromSample<T>>::from_sample_(f[1]) } else { l };
                peak = peak.max(l.abs()).max(r.abs());
                s.buf.push_back([l, r]);
            }
            while s.buf.len() > max {
                s.buf.pop_front();
            }
            s.level = peak.max(s.level * 0.9);
        },
        |e| eprintln!("HDMI sound input: {e}"),
        None,
    )?)
}

fn build_output<T>(dev: &cpal::Device, cfg: &cpal::StreamConfig, shared: Arc<Mutex<Shared>>, vol: Arc<AtomicU32>, in_rate: u32, out_rate: u32) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = cfg.channels as usize;
    let target = (in_rate * TARGET_MS / 1000) as usize;
    // Input frames per output frame; nearest-neighbour is fine for this.
    let step = in_rate as f64 / out_rate as f64;
    let mut pos = 0f64;
    Ok(dev.build_output_stream(
        cfg,
        move |data: &mut [T], _| {
            let v = f32::from_bits(vol.load(Ordering::Relaxed));
            let Ok(mut s) = shared.lock() else { return };
            if s.priming && s.buf.len() >= target {
                s.priming = false;
            }
            for f in data.chunks_mut(ch.max(1)) {
                let frame = if s.priming { None } else { s.buf.front().copied() };
                let [l, r] = match frame {
                    Some(x) => x,
                    None => {
                        s.priming = true; // ran dry: wait to refill rather than crackle
                        [0.0, 0.0]
                    }
                };
                if frame.is_some() {
                    pos += step;
                    while pos >= 1.0 {
                        s.buf.pop_front();
                        pos -= 1.0;
                    }
                }
                for (i, out) in f.iter_mut().enumerate() {
                    *out = <T as FromSample<f32>>::from_sample_(if i % 2 == 0 { l } else { r } * v);
                }
            }
        },
        |e| eprintln!("HDMI sound output: {e}"),
        None,
    )?)
}
