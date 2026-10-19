//! Live HDMI video from a USB capture card (UVC camera).
//!
//! The MEGA65's HDMI output goes into a capture card (e.g. UGREEN 15390), which
//! the computer sees as a camera. This opens that camera through the system's
//! own video API (AVFoundation on macOS, Media Foundation on Windows, V4L2 on
//! Linux) and hands out the newest frame as RGB. Unlike the JTAG screen
//! capture it costs the MEGA65 nothing: no monitor traffic, no paused CPU.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nokhwa::pixel_format::RgbFormat;
use nokhwa::FormatDecoder;
use nokhwa::utils::{ApiBackend, CameraFormat, CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;

/// A video input the system offers.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveDevice {
    pub index: u32,
    pub name: String,
    pub description: String,
}

impl LiveDevice {
    /// Looks like an HDMI capture card rather than a webcam or phone.
    pub fn looks_like_capture_card(&self) -> bool {
        // macOS descriptions say "AVCaptureDeviceType…" for every camera.
        let s = format!("{} {}", self.name, self.description).to_ascii_lowercase().replace("avcapturedevicetype", "");
        ["capture", "hdmi", "ugreen", "macrosilicon", "cam link", "elgato", "usb video", "uvc"].iter().any(|w| s.contains(w))
    }
}

/// One RGB frame (3 bytes per pixel, rows top to bottom).
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    /// Counts up for every frame the stream delivered.
    pub seq: u64,
}

impl Frame {
    pub fn write_png<W: std::io::Write>(&self, w: W) -> anyhow::Result<()> {
        let mut enc = png::Encoder::new(w, self.width, self.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&self.rgb)?;
        Ok(())
    }

    pub fn save_png(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.write_png(std::io::BufWriter::new(std::fs::File::create(path)?))
    }
}

fn guard<T>(what: &str, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            anyhow::bail!("{what} failed inside the camera driver {msg}")
        }
    }
}

/// Ask for camera permission if the system needs it (macOS shows its prompt
/// once per app). Returns false when it was refused.
pub fn ensure_permission() -> bool {
    if nokhwa::nokhwa_check() {
        return true;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    nokhwa::nokhwa_initialize(move |ok| {
        let _ = tx.lock().map(|t| t.send(ok));
    });
    // The user may take a while to answer the prompt; look at the status
    // too in case the answer callback never arrives.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(120) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ok) => return ok || nokhwa::nokhwa_check(),
            Err(_) if nokhwa::nokhwa_check() => return true,
            Err(_) => {}
        }
    }
    false
}

/// The video inputs the system offers, capture cards first.
pub fn list() -> anyhow::Result<Vec<LiveDevice>> {
    let infos = guard("listing cameras", || Ok(nokhwa::query(ApiBackend::Auto)?))?;
    let mut v: Vec<LiveDevice> = infos
        .iter()
        .filter_map(|c| match c.index() {
            CameraIndex::Index(i) => Some(LiveDevice { index: *i, name: c.human_name(), description: c.description().to_string() }),
            CameraIndex::String(_) => None,
        })
        .collect();
    v.sort_by_key(|d| !d.looks_like_capture_card());
    Ok(v)
}

/// Pick a device: the one whose name contains `hint` (case-insensitive), else
/// the first that looks like a capture card, else the first one.
pub fn pick(hint: Option<&str>) -> anyhow::Result<LiveDevice> {
    let all = list()?;
    if let Some(h) = hint.filter(|h| !h.is_empty()) {
        let h = h.to_ascii_lowercase();
        if let Some(d) = all.iter().find(|d| d.name.to_ascii_lowercase().contains(&h) || d.index.to_string() == h) {
            return Ok(d.clone());
        }
        anyhow::bail!("no video input matching \"{h}\" (found: {})", names(&all));
    }
    all.into_iter().next().ok_or_else(|| anyhow::anyhow!("no video inputs found; is the capture card plugged in?"))
}

fn names(all: &[LiveDevice]) -> String {
    if all.is_empty() {
        return "none".into();
    }
    all.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// The capture format to use: the smallest picture of at least 720x480 that
/// at the best frame rate up to 60 (the MEGA65 sends 720x576@50 or
/// 720x480@60; cards often upscale, and bigger frames only cost decoding
/// time). None if every format is smaller than that.
fn choose_format(formats: &[CameraFormat]) -> Option<CameraFormat> {
    let big = |f: &&CameraFormat| f.width() >= 720 && f.height() >= 480;
    let best = formats.iter().filter(big).map(|f| f.frame_rate().min(60)).max()?;
    formats.iter().filter(big).filter(|f| f.frame_rate().min(60) == best).min_by_key(|f| f.width() * f.height()).copied()
}

fn open(dev: &LiveDevice) -> anyhow::Result<Camera> {
    if !ensure_permission() {
        anyhow::bail!("camera access was refused; allow it in System Settings → Privacy & Security → Camera");
    }
    guard("opening the video input", || {
        let idx = CameraIndex::Index(dev.index);
        let mut cam = Camera::new(idx, RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate))?;
        if let Ok(formats) = cam.compatible_camera_formats() {
            if let Some(f) = choose_format(&formats) {
                if f != cam.camera_format() {
                    let _ = cam.set_camera_requset(RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(f)));
                }
            }
        }
        cam.open_stream()?;
        Ok(cam)
    })
}

fn read_frame(cam: &mut Camera, seq: u64) -> anyhow::Result<Frame> {
    guard("reading a frame", || {
        let buf = cam.frame()?;
        let res = buf.resolution();
        let rgb = RgbFormat::write_output(buf.source_frame_format(), res, buf.buffer())?;
        let (w, h) = (res.width() as usize, res.height() as usize);
        Ok(Frame { width: w as u32, height: h as u32, rgb: crop_padded_rows(rgb, w, h), seq })
    })
}

/// Some drivers pad each row (e.g. a 1360-pixel YUYV picture arrives
/// 1376 pixels wide); keep the first `w` pixels of every row.
fn crop_padded_rows(rgb: Vec<u8>, w: usize, h: usize) -> Vec<u8> {
    if rgb.len() == w * h * 3 || h == 0 || rgb.len() % (h * 3) != 0 || rgb.len() < w * h * 3 {
        return rgb;
    }
    let stride = rgb.len() / h;
    rgb.chunks_exact(stride).flat_map(|row| &row[..w * 3]).copied().collect()
}

/// Grab one frame. Skips the first few, which are often black while the
/// card settles.
pub fn grab(dev: &LiveDevice, skip: usize) -> anyhow::Result<Frame> {
    let mut cam = open(dev)?;
    let mut last = None;
    for i in 0..=skip {
        last = Some(read_frame(&mut cam, i as u64)?);
    }
    let _ = guard("closing", || Ok(cam.stop_stream()?));
    Ok(last.unwrap())
}

#[derive(Default)]
struct Shared {
    frame: Option<Frame>,
    error: Option<String>,
    format: String,
    fps: f32,
}

/// A running capture on its own thread. The newest frame is kept; older
/// ones are dropped. Stops when dropped (without waiting, so a card that
/// stopped sending can't freeze the caller).
pub struct LiveStream {
    pub device: LiveDevice,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LiveStream {
    /// Start capturing; `on_frame` runs after every new frame (e.g. to wake a GUI).
    pub fn start(device: LiveDevice, on_frame: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (sh, st, dev) = (shared.clone(), stop.clone(), device.clone());
        let thread = std::thread::Builder::new()
            .name("hdmi-live".into())
            .spawn(move || {
                let fail = |e: anyhow::Error| {
                    sh.lock().unwrap().error = Some(format!("{e:#}"));
                    on_frame();
                };
                let opened = catch_unwind(AssertUnwindSafe(|| open(&dev))).unwrap_or_else(|_| Err(anyhow::anyhow!("the camera driver crashed while opening")));
                let mut cam = match opened {
                    Ok(c) => c,
                    Err(e) => return fail(e),
                };
                let f = cam.camera_format();
                sh.lock().unwrap().format = format!("{}x{} {} @ {} Hz", f.width(), f.height(), f.format(), f.frame_rate());
                let (mut seq, mut count, mut since) = (0u64, 0u32, Instant::now());
                while !st.load(Ordering::Relaxed) {
                    match read_frame(&mut cam, seq) {
                        Ok(fr) => {
                            seq += 1;
                            count += 1;
                            let mut s = sh.lock().unwrap();
                            s.frame = Some(fr);
                            s.error = None;
                            if since.elapsed() >= Duration::from_secs(1) {
                                s.fps = count as f32 / since.elapsed().as_secs_f32();
                                (count, since) = (0, Instant::now());
                            }
                            drop(s);
                            on_frame();
                        }
                        Err(e) => {
                            fail(e);
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    }
                }
                let _ = guard("closing", || Ok(cam.stop_stream()?));
            })
            .expect("spawn capture thread");
        Self { device, shared, stop, thread: Some(thread) }
    }

    /// The newest frame, if it is newer than `seen` (pass the last `seq` you showed).
    pub fn newer_than(&self, seen: Option<u64>) -> Option<Frame> {
        let s = self.shared.lock().unwrap();
        s.frame.as_ref().filter(|f| seen.map_or(true, |n| f.seq > n)).cloned()
    }

    pub fn latest(&self) -> Option<Frame> {
        self.shared.lock().unwrap().frame.clone()
    }

    /// Last error from the capture thread (cleared by the next good frame).
    pub fn error(&self) -> Option<String> {
        self.shared.lock().unwrap().error.clone()
    }

    /// e.g. "1280x720 MJPEG @ 60 Hz" once the stream is open.
    pub fn format(&self) -> String {
        self.shared.lock().unwrap().format.clone()
    }

    /// Frames per second actually arriving.
    pub fn fps(&self) -> f32 {
        self.shared.lock().unwrap().fps
    }

    /// The capture thread ended (failed to open or stopped).
    pub fn finished(&self) -> bool {
        self.thread.as_ref().map_or(true, |t| t.is_finished())
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nokhwa::utils::FrameFormat;

    #[test]
    fn prefers_smallest_fast_format() {
        let f = |w, h, fps| CameraFormat::new_from(w, h, FrameFormat::MJPEG, fps);
        let all = [f(1920, 1080, 60), f(1280, 720, 60), f(640, 480, 60), f(1280, 720, 30), f(1024, 768, 50)];
        assert_eq!(choose_format(&all), Some(f(1280, 720, 60)));
        assert_eq!(choose_format(&[f(1920, 1080, 30), f(640, 480, 60)]), Some(f(1920, 1080, 30)));
        assert_eq!(choose_format(&[f(640, 480, 60)]), None);
    }

    #[test]
    fn crops_padded_rows() {
        let rgb: Vec<u8> = (0..2 * 3 * 3).map(|i| i as u8).collect(); // 3 px stride, 2 rows
        assert_eq!(crop_padded_rows(rgb.clone(), 2, 2), vec![0, 1, 2, 3, 4, 5, 9, 10, 11, 12, 13, 14]);
        assert_eq!(crop_padded_rows(rgb.clone(), 3, 2), rgb);
    }
}
