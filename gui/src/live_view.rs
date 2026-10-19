//! The Screen tab's "HDMI live" source: the MEGA65's HDMI picture through a
//! USB capture card, shown as it arrives. Needs no serial connection.

use egui::{Color32, RichText, TextureHandle, TextureOptions};
use mega65::live::{self, LiveDevice, LiveStream};

use crate::live_audio::LiveAudio;

#[derive(Default)]
pub struct LiveView {
    stream: Option<LiveStream>,
    audio: Option<LiveAudio>,
    audio_error: Option<String>,
    tex: Option<TextureHandle>,
    seen: Option<u64>,
    devices: Option<Result<Vec<LiveDevice>, String>>,
    /// Last problem or note shown under the controls.
    pub note: Option<(String, bool)>,
}

impl LiveView {
    pub fn running(&self) -> bool {
        self.stream.is_some()
    }

    pub fn texture(&self) -> Option<&TextureHandle> {
        self.tex.as_ref()
    }

    fn refresh_devices(&mut self) {
        self.devices = Some(live::list().map_err(|e| format!("{e:#}")));
    }

    pub fn start(&mut self, ctx: &egui::Context, device_name: &mut String) {
        if self.devices.is_none() {
            self.refresh_devices();
        }
        let devs = match &self.devices {
            Some(Ok(d)) => d.clone(),
            Some(Err(e)) => {
                self.note = Some((e.clone(), true));
                return;
            }
            None => Vec::new(),
        };
        let dev = devs.iter().find(|d| d.name == *device_name).or_else(|| devs.first()).cloned();
        let Some(dev) = dev else {
            self.note = Some(("no video inputs found; is the capture card plugged in?".into(), true));
            return;
        };
        *device_name = dev.name.clone();
        let c = ctx.clone();
        self.stream = Some(LiveStream::start(dev, move || c.request_repaint()));
        self.seen = None;
        self.note = Some(("starting…".into(), false));
    }

    pub fn stop(&mut self) {
        self.stream = None;
        self.audio = None;
        self.note = None;
    }

    /// Pick up the newest frame. Call once per GUI frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.stream else { return };
        if let Some(f) = s.newer_than(self.seen) {
            self.seen = Some(f.seq);
            let img = egui::ColorImage::from_rgb([f.width as usize, f.height as usize], &f.rgb);
            match &mut self.tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("hdmi-live", img, TextureOptions::LINEAR)),
            }
        }
        if let Some(e) = s.error() {
            self.note = Some((e, true));
        } else if self.seen.is_some() {
            self.note = Some((format!("{}  ·  {} — {:.0} frames/s", s.device.name, s.format(), s.fps()), false));
        }
        if s.finished() {
            // Opening failed: keep the message, drop the stream so Start works again.
            self.stream = None;
            self.audio = None;
        }
    }

    /// Play the card's sound while the picture runs and `on` is set.
    fn sync_audio(&mut self, on: bool, volume: f32) {
        let want = on && self.stream.is_some();
        if !want {
            self.audio = None;
            self.audio_error = None;
            return;
        }
        if self.audio.is_none() && self.audio_error.is_none() {
            let hint = self.stream.as_ref().map(|s| s.device.name.clone()).unwrap_or_default();
            match LiveAudio::start(&hint, volume) {
                Ok(a) => self.audio = Some(a),
                Err(e) => self.audio_error = Some(format!("sound: {e:#}")),
            }
        }
        if let Some(a) = &self.audio {
            a.set_volume(volume);
        }
    }

    /// 🔊 Sound toggle, volume and level meter.
    pub fn sound_controls(&mut self, ui: &mut egui::Ui, on: &mut bool, volume: &mut f32) {
        if ui.checkbox(on, "🔊 Sound").on_hover_text("play the MEGA65's HDMI sound from the capture card on this computer (about 50 ms behind, like the picture)").changed() {
            self.audio_error = None;
        }
        ui.add_enabled(*on, egui::Slider::new(volume, 0.0..=1.0).show_value(false)).on_hover_text("volume");
        if let Some(a) = &self.audio {
            let lvl = a.level().clamp(0.0, 1.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(40.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, ui.visuals().extreme_bg_color);
            let mut f = r;
            f.set_width(r.width() * lvl);
            ui.painter().rect_filled(f, 2.0, Color32::from_rgb(90, 200, 120));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.sync_audio(*on, *volume);
        if let Some(e) = &self.audio_error {
            ui.label(RichText::new(e).small().color(Color32::from_rgb(230, 90, 90)));
        }
    }

    /// Camera picker, Start/Stop and Snapshot.
    pub fn controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, device_name: &mut String) {
        if self.devices.is_none() {
            self.refresh_devices();
        }
        let names: Vec<String> = match &self.devices {
            Some(Ok(d)) => d.iter().map(|d| d.name.clone()).collect(),
            _ => Vec::new(),
        };
        if device_name.is_empty() {
            if let Some(n) = names.first() {
                *device_name = n.clone();
            }
        }
        let running = self.running();
        ui.add_enabled_ui(!running, |ui| {
            egui::ComboBox::from_id_salt("hdmi-device")
                .selected_text(if device_name.is_empty() { "(no video input)" } else { device_name.as_str() })
                .show_ui(ui, |ui| {
                    for n in &names {
                        ui.selectable_value(device_name, n.clone(), n);
                    }
                })
                .response
                .on_hover_text("the capture card the MEGA65's HDMI goes into (it shows up as a camera)");
            if ui.small_button("↻").on_hover_text("look for video inputs again").clicked() {
                self.refresh_devices();
            }
        });
        if running {
            if ui.button("■ Stop").clicked() {
                self.stop();
            }
        } else if ui.add_enabled(!device_name.is_empty(), egui::Button::new("▶ Start")).clicked() {
            self.start(ctx, device_name);
        }
        let frame = self.stream.as_ref().and_then(|s| s.latest());
        if ui.add_enabled(frame.is_some(), egui::Button::new("📷 Snapshot…")).on_hover_text("save the current picture as a PNG").clicked() {
            if let Some(f) = frame {
                if let Some(p) = rfd::FileDialog::new().set_file_name("mega65-hdmi.png").add_filter("PNG", &["png"]).save_file() {
                    self.note = Some(match f.save_png(&p) {
                        Ok(()) => (format!("saved {} ({}x{})", p.display(), f.width, f.height), false),
                        Err(e) => (format!("could not save: {e:#}"), true),
                    });
                }
            }
        }
    }

    pub fn note_ui(&self, ui: &mut egui::Ui) {
        if let Some((n, err)) = &self.note {
            let t = RichText::new(n).small();
            ui.label(if *err { t.color(Color32::from_rgb(230, 90, 90)) } else { t.weak() });
        }
    }
}
