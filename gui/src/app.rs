//! The MEGA65 Studio window: a debugger-style layout with a toolbar, a register
//! and control sidebar, tabbed memory / disassembly / screen / console views,
//! and a log. All hardware work goes through [`crate::backend`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use egui::{Color32, RichText, TextureHandle, TextureOptions};
use mega65::monitor::Registers;
use mega65::{parse_hex, transport};

use crate::backend::{Backend, Cmd, Event};

const MEM_VIEW: usize = 0;
const MEM_VIEW_2: usize = 1;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    device: String,
    baud: u32,
    live: bool,
    interval_ms: u64,
    live_screen: bool,
    pause_for_screen: bool,
    mem_addr: [String; 2],
    mem_rows: usize,
    disasm_addr: String,
    disasm_follow: bool,
    prg_path: String,
    prg_run: bool,
    prg_binary: bool,
    tab: Tab,
    prof_flag: String,
    prof_bucket: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device: transport::autodetect_port().unwrap_or_default(),
            baud: transport::DEFAULT_BAUD,
            live: true,
            interval_ms: 500,
            live_screen: false,
            pause_for_screen: false,
            mem_addr: ["0400".into(), "FFD3000".into()],
            mem_rows: 16,
            disasm_addr: String::new(),
            disasm_follow: true,
            prg_path: String::new(),
            prg_run: true,
            prg_binary: false,
            tab: Tab::Memory,
            prof_flag: String::new(),
            prof_bucket: 16,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Tab {
    Memory,
    Disassembly,
    Screen,
    Profiler,
    Console,
}

#[derive(Default)]
struct MemView {
    addr: u32,
    data: Vec<u8>,
    /// Byte being edited (offset) and its text.
    edit: Option<(usize, String)>,
}

pub struct StudioApp {
    backend: Backend,
    s: Settings,
    connected: bool,
    xemu: bool,
    busy: bool,
    status: String,
    regs: Option<Registers>,
    stopped: bool,
    mem: [MemView; 2],
    disasm: String,
    disasm_at: u32,
    screen_tex: Option<TextureHandle>,
    screen_text: String,
    log: VecDeque<String>,
    console_in: String,
    break_in: String,
    watch_in: String,
    goto_in: String,
    type_in: String,
    bin_addr: String,
    save_start: String,
    save_end: String,
    ports: Vec<String>,
    last_error: Option<String>,
    step_count: usize,
    prof_on: bool,
    prof_total: u32,
    prof_buckets: Vec<(u32, u32)>,
    prof_rate: f32,
    fps_history: VecDeque<f32>,
}

impl StudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, "settings")).unwrap_or_default();
        let backend = Backend::start(cc.egui_ctx.clone());
        backend.send(Cmd::SetLive { enabled: s.live, interval: Duration::from_millis(s.interval_ms), screen: s.live_screen });
        let ports = transport::list_ports().into_iter().map(|p| p.path).collect();
        Self {
            backend,
            s,
            connected: false,
            xemu: false,
            busy: false,
            status: "not connected".into(),
            regs: None,
            stopped: false,
            mem: Default::default(),
            disasm: String::new(),
            disasm_at: 0,
            screen_tex: None,
            screen_text: String::new(),
            log: VecDeque::new(),
            console_in: String::new(),
            break_in: String::new(),
            watch_in: String::new(),
            goto_in: String::new(),
            type_in: String::new(),
            bin_addr: String::new(),
            save_start: "2001".into(),
            save_end: "4000".into(),
            ports,
            last_error: None,
            step_count: 1,
            prof_on: false,
            prof_total: 0,
            prof_buckets: Vec::new(),
            prof_rate: 0.0,
            fps_history: VecDeque::new(),
        }
    }

    fn push_log(&mut self, line: String) {
        for l in line.lines() {
            self.log.push_back(l.to_string());
        }
        while self.log.len() > 2000 {
            self.log.pop_front();
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.backend.rx.try_recv() {
            match ev {
                Event::Connected { device, xemu, versions } => {
                    self.connected = true;
                    self.xemu = xemu;
                    self.status = match versions {
                        Some(v) => format!("{device}: {} / {} / {}", v.model, v.core, v.rom),
                        None => device,
                    };
                    if xemu {
                        self.status.push_str("  [Xemu: CPU can't be resumed after a halt]");
                    }
                    self.push_log(format!("connected: {}", self.status));
                    self.last_error = None;
                    self.send_views();
                }
                Event::Disconnected => {
                    self.connected = false;
                    self.prof_on = false;
                    self.status = "not connected".into();
                    self.regs = None;
                    self.push_log("disconnected".into());
                }
                Event::Registers { regs, stopped } => {
                    self.regs = Some(regs);
                    self.stopped = stopped;
                }
                Event::Memory { id, addr, data } => {
                    if let Some(v) = self.mem.get_mut(id) {
                        v.addr = addr;
                        v.data = data;
                    }
                }
                Event::Disasm { addr, text } => {
                    self.disasm = text;
                    self.disasm_at = addr;
                }
                Event::Screen { width, height, rgb, text } => {
                    let img = egui::ColorImage::from_rgb([width, height], &rgb);
                    match &mut self.screen_tex {
                        Some(t) => t.set(img, TextureOptions::NEAREST),
                        None => self.screen_tex = Some(ctx.load_texture("screen", img, TextureOptions::NEAREST)),
                    }
                    self.screen_text = text;
                }
                Event::Log(l) => self.push_log(l),
                Event::Error(e) => {
                    self.push_log(format!("error: {e}"));
                    self.last_error = Some(e);
                }
                Event::Busy(b) => self.busy = b,
                Event::Profile { total, buckets, samples_per_sec, fps } => {
                    self.prof_total = total;
                    self.prof_buckets = buckets;
                    self.prof_rate = samples_per_sec;
                    if let Some(f) = fps {
                        self.fps_history.push_back(f);
                        while self.fps_history.len() > 240 {
                            self.fps_history.pop_front();
                        }
                    }
                }
                Event::BreakHit(r) => {
                    self.push_log(format!("CPU stopped at ${:04X} (breakpoint/watchpoint)", r.pc));
                    self.stopped = true;
                    self.s.tab = Tab::Disassembly;
                }
            }
        }
    }

    fn send_views(&self) {
        for (i, a) in self.s.mem_addr.iter().enumerate() {
            if let Ok(addr) = parse_hex(a) {
                self.backend.send(Cmd::Memory { id: i, addr, len: self.s.mem_rows * 16 });
            }
        }
        let addr = if self.s.disasm_follow { None } else { parse_hex(&self.s.disasm_addr).ok() };
        self.backend.send(Cmd::Disasm { addr, pages: 2 });
    }

    fn send_live(&self) {
        self.backend.send(Cmd::SetLive {
            enabled: self.s.live,
            interval: Duration::from_millis(self.s.interval_ms.max(100)),
            screen: self.s.live_screen,
        });
    }

    // ------------------------------------------------------------------ UI

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Device:");
            egui::ComboBox::from_id_salt("device")
                .selected_text(if self.s.device.is_empty() { "(choose)" } else { self.s.device.as_str() })
                .width(220.0)
                .show_ui(ui, |ui| {
                    for p in self.ports.clone() {
                        ui.selectable_value(&mut self.s.device, p.clone(), p);
                    }
                    ui.selectable_value(&mut self.s.device, "tcp:localhost:4510".into(), "Xemu (TCP :4510)");
                    #[cfg(unix)]
                    ui.selectable_value(&mut self.s.device, "/tmp/xemu-umon.sock".into(), "Xemu (unix socket)");
                });
            ui.add(egui::TextEdit::singleline(&mut self.s.device).desired_width(200.0).hint_text("port, tcp:host:port or socket path"));
            if ui.button("⟳").on_hover_text("rescan serial ports").clicked() {
                self.ports = transport::list_ports().into_iter().map(|p| p.path).collect();
            }
            ui.label("Baud:");
            ui.add(egui::DragValue::new(&mut self.s.baud).speed(1000));
            if self.connected {
                if ui.button("Disconnect").clicked() {
                    self.backend.send(Cmd::Disconnect);
                }
            } else if ui.add_enabled(!self.s.device.is_empty(), egui::Button::new("Connect")).clicked() {
                self.backend.send(Cmd::Connect { device: self.s.device.clone(), baud: self.s.baud });
            }
            ui.separator();
            let en = self.connected && !self.busy;
            let can_resume = en && !(self.xemu && self.stopped);
            if ui.add_enabled(en, egui::Button::new("⏸ Stop")).on_hover_text("halt the CPU (t1)").clicked() {
                self.backend.send(Cmd::Stop);
            }
            if ui.add_enabled(can_resume, egui::Button::new("▶ Go")).on_hover_text("resume (t0)").clicked() {
                self.backend.send(Cmd::Go);
            }
            if ui.add_enabled(en && !self.xemu, egui::Button::new("⏭ Step")).clicked() {
                self.backend.send(Cmd::Step(self.step_count));
            }
            ui.add(egui::DragValue::new(&mut self.step_count).range(1..=1000)).on_hover_text("instructions per step");
            if ui.add_enabled(en, egui::Button::new("Refresh")).clicked() {
                self.send_views();
                self.backend.send(Cmd::Refresh);
            }
            if ui.add_enabled(en, egui::Button::new("Reset")).clicked() {
                self.backend.send(Cmd::Reset);
            }
            ui.separator();
            if ui.checkbox(&mut self.s.live, "Live").on_hover_text("poll registers, memory and disassembly").changed() {
                self.send_live();
            }
            if ui.add(egui::DragValue::new(&mut self.s.interval_ms).range(100..=5000).suffix(" ms")).changed() {
                self.send_live();
            }
            if self.busy {
                ui.spinner();
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let en = self.connected && !self.busy;
        ui.heading("CPU");
        match &self.regs {
            Some(r) => {
                let flag = |ui: &mut egui::Ui, name: &str, v: String| {
                    ui.label(RichText::new(name).weak());
                    ui.label(RichText::new(v).monospace().strong());
                    ui.end_row();
                };
                egui::Grid::new("regs").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    flag(ui, "PC", format!("${:04X}", r.pc));
                    flag(ui, "A", format!("${:02X}", r.a));
                    flag(ui, "X", format!("${:02X}", r.x));
                    flag(ui, "Y", format!("${:02X}", r.y));
                    flag(ui, "Z", format!("${:02X}", r.z));
                    flag(ui, "B", format!("${:02X}", r.b));
                    flag(ui, "SP", format!("${:04X}", r.sp));
                    flag(ui, "MAPL/H", format!("{:04X} {:04X}", r.mapl, r.maph));
                    flag(ui, "Flags", if r.flags_str.is_empty() { format!("${:02X}", r.flags) } else { r.flags_str.clone() });
                });
                ui.label(RichText::new(&r.last_op).monospace().small());
                let state = if self.stopped { ("HALTED", Color32::from_rgb(220, 80, 80)) } else { ("running", Color32::from_rgb(80, 180, 80)) };
                ui.label(RichText::new(state.0).color(state.1).strong());
                if r.hypervisor {
                    ui.label(RichText::new("in Hypervisor").color(Color32::YELLOW));
                }
            }
            None => {
                ui.label(RichText::new("no registers yet").weak());
            }
        }
        ui.separator();
        ui.heading("Breakpoint");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.break_in).desired_width(70.0).hint_text("PC hex"));
            if ui.add_enabled(en, egui::Button::new("Set")).clicked() {
                match parse_hex(&self.break_in) {
                    Ok(a) => self.backend.send(Cmd::SetBreak(Some(a as u16))),
                    Err(e) => self.last_error = Some(e),
                }
            }
            if ui.add_enabled(en, egui::Button::new("Clear")).clicked() {
                self.backend.send(Cmd::SetBreak(None));
            }
        });
        ui.heading("Watchpoint");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.watch_in).desired_width(70.0).hint_text("28-bit hex"));
            if ui.add_enabled(en, egui::Button::new("Set")).clicked() {
                match parse_hex(&self.watch_in) {
                    Ok(a) => self.backend.send(Cmd::SetWatch(Some(a))),
                    Err(e) => self.last_error = Some(e),
                }
            }
            if ui.add_enabled(en, egui::Button::new("Clear")).clicked() {
                self.backend.send(Cmd::SetWatch(None));
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.goto_in).desired_width(70.0).hint_text("PC hex"));
            if ui.add_enabled(en, egui::Button::new("Go to")).on_hover_text("set PC and run").clicked() {
                match parse_hex(&self.goto_in) {
                    Ok(a) => self.backend.send(Cmd::GoTo(a as u16)),
                    Err(e) => self.last_error = Some(e),
                }
            }
            if ui.add_enabled(en, egui::Button::new("IRQ off")).clicked() {
                self.backend.send(Cmd::SetIrqs(false));
            }
            if ui.add_enabled(en, egui::Button::new("IRQ on")).clicked() {
                self.backend.send(Cmd::SetIrqs(true));
            }
        });
        ui.separator();
        ui.heading("Program");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.s.prg_path).desired_width(150.0).hint_text("file.prg"));
            if ui.button("Browse…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("PRG / binary", &["prg", "PRG", "bin", "sid"]).pick_file() {
                    self.s.prg_path = p.display().to_string();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.s.prg_run, "RUN");
            ui.checkbox(&mut self.s.prg_binary, "keep load address").on_hover_text("otherwise forced to the BASIC start of the current mode");
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(en && !self.s.prg_path.is_empty(), egui::Button::new("Load PRG")).clicked() {
                self.backend.send(Cmd::LoadPrg { path: PathBuf::from(&self.s.prg_path), run: self.s.prg_run, binary: self.s.prg_binary, jump: None });
            }
            ui.add(egui::TextEdit::singleline(&mut self.bin_addr).desired_width(60.0).hint_text("addr"));
            if ui.add_enabled(en && !self.s.prg_path.is_empty(), egui::Button::new("Load raw at")).on_hover_text("write the file as-is at this 28-bit address (ROM areas are unprotected first)").clicked() {
                match parse_hex(&self.bin_addr) {
                    Ok(a) => self.backend.send(Cmd::LoadBinary { path: PathBuf::from(&self.s.prg_path), addr: a }),
                    Err(e) => self.last_error = Some(e),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.save_start).desired_width(60.0).hint_text("start"));
            ui.add(egui::TextEdit::singleline(&mut self.save_end).desired_width(60.0).hint_text("end"));
            if ui.add_enabled(en, egui::Button::new("Save…")).on_hover_text("save a memory range as a PRG").clicked() {
                if let (Ok(s), Ok(e)) = (parse_hex(&self.save_start), parse_hex(&self.save_end)) {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("memory.prg").save_file() {
                        self.backend.send(Cmd::SaveMemory { path: p, start: s, end: e, prg: true });
                    }
                }
            }
        });
        ui.separator();
        ui.heading("Keyboard");
        ui.add(egui::TextEdit::singleline(&mut self.type_in).hint_text("text to type (~M = RETURN)"));
        ui.horizontal(|ui| {
            if ui.add_enabled(en, egui::Button::new("Type")).on_hover_text("virtual keyboard, works in games").clicked() {
                self.backend.send(Cmd::TypeText { text: self.type_in.clone(), ret: true, buffer: false });
            }
            if ui.add_enabled(en, egui::Button::new("Stuff")).on_hover_text("KERNAL keyboard buffer, fast at a BASIC prompt").clicked() {
                self.backend.send(Cmd::TypeText { text: self.type_in.clone(), ret: true, buffer: true });
            }
            if ui.add_enabled(en, egui::Button::new("RUN/STOP")).clicked() {
                self.backend.send(Cmd::TypeText { text: "~C".into(), ret: false, buffer: false });
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(en, egui::Button::new("GO64")).clicked() {
                self.backend.send(Cmd::Go64);
            }
            if ui.add_enabled(en, egui::Button::new("PAL")).clicked() {
                self.backend.send(Cmd::Video { pal: true });
            }
            if ui.add_enabled(en, egui::Button::new("NTSC")).clicked() {
                self.backend.send(Cmd::Video { pal: false });
            }
        });
    }

    fn memory_tab(&mut self, ui: &mut egui::Ui) {
        for i in 0..2 {
            ui.horizontal(|ui| {
                ui.label(format!("View {}:", i + 1));
                let r = ui.add(egui::TextEdit::singleline(&mut self.s.mem_addr[i]).desired_width(90.0).hint_text("hex address"));
                let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Go").clicked() || go {
                    self.send_views();
                }
                if let Some(r) = &self.regs {
                    if ui.button("PC").on_hover_text("view the CPU's memory around PC").clicked() {
                        self.s.mem_addr[i] = format!("777{:04X}", r.pc.saturating_sub(16));
                        self.send_views();
                    }
                }
                if ui.button("◀").clicked() {
                    if let Ok(a) = parse_hex(&self.s.mem_addr[i]) {
                        self.s.mem_addr[i] = format!("{:X}", a.saturating_sub((self.s.mem_rows * 16) as u32));
                        self.send_views();
                    }
                }
                if ui.button("▶").clicked() {
                    if let Ok(a) = parse_hex(&self.s.mem_addr[i]) {
                        self.s.mem_addr[i] = format!("{:X}", a + (self.s.mem_rows * 16) as u32);
                        self.send_views();
                    }
                }
                if i == 0 {
                    ui.label("rows:");
                    if ui.add(egui::DragValue::new(&mut self.s.mem_rows).range(1..=64)).changed() {
                        self.send_views();
                    }
                    ui.label(RichText::new("click a byte to edit; Enter writes it").weak());
                }
            });
            self.hex_grid(ui, if i == 0 { MEM_VIEW } else { MEM_VIEW_2 });
            ui.separator();
        }
    }

    fn hex_grid(&mut self, ui: &mut egui::Ui, id: usize) {
        let en = self.connected;
        let mut write: Option<(u32, u8)> = None;
        let view = &mut self.mem[id];
        egui::ScrollArea::vertical().id_salt(("mem", id)).max_height(16.0 * 18.0).show(ui, |ui| {
            egui::Grid::new(("hexgrid", id)).spacing([4.0, 0.0]).striped(true).show(ui, |ui| {
                for (row, chunk) in view.data.chunks(16).enumerate() {
                    let base = view.addr + (row * 16) as u32;
                    ui.label(RichText::new(format!("{base:07X}")).monospace().weak());
                    for (col, &b) in chunk.iter().enumerate() {
                        let off = row * 16 + col;
                        if let Some((eoff, text)) = &mut view.edit {
                            if *eoff == off {
                                let r = ui.add(egui::TextEdit::singleline(text).desired_width(22.0).font(egui::TextStyle::Monospace));
                                r.request_focus();
                                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    if let Ok(v) = u8::from_str_radix(text.trim(), 16) {
                                        write = Some((base + col as u32, v));
                                    }
                                    view.edit = None;
                                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    view.edit = None;
                                }
                                continue;
                            }
                        }
                        let r = ui.add(egui::Label::new(RichText::new(format!("{b:02X}")).monospace()).sense(egui::Sense::click()));
                        if r.clicked() && en {
                            view.edit = Some((off, format!("{b:02X}")));
                        }
                    }
                    for _ in chunk.len()..16 {
                        ui.label("");
                    }
                    let ascii: String = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
                    ui.label(RichText::new(ascii).monospace());
                    ui.end_row();
                }
            });
        });
        if let Some((addr, v)) = write {
            self.backend.send(Cmd::WriteBytes { addr, data: vec![v] });
        }
    }

    fn disasm_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.s.disasm_follow, "Follow PC").changed() {
                self.send_views();
            }
            ui.add_enabled_ui(!self.s.disasm_follow, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.s.disasm_addr).desired_width(90.0).hint_text("hex (777xxxx = CPU view)"));
                if ui.button("Go").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    self.send_views();
                }
            });
            ui.label(RichText::new(format!("at ${:07X}", self.disasm_at)).weak());
        });
        egui::ScrollArea::both().show(ui, |ui| {
            let pc = self.regs.as_ref().map(|r| r.pc);
            for line in self.disasm.lines() {
                let addr_tok = line.trim_start_matches('$').split_whitespace().next().unwrap_or("");
                let is_pc = pc.map(|p| u32::from_str_radix(addr_tok, 16).map(|a| (a & 0xffff) as u16 == p).unwrap_or(false)).unwrap_or(false);
                let t = RichText::new(line).monospace();
                ui.label(if is_pc { t.color(Color32::from_rgb(255, 200, 80)).strong() } else { t });
            }
        });
    }

    fn screen_tab(&mut self, ui: &mut egui::Ui) {
        let en = self.connected && !self.busy;
        ui.horizontal(|ui| {
            if ui.add_enabled(en, egui::Button::new("Capture")).clicked() {
                self.backend.send(Cmd::Screenshot { pause: self.s.pause_for_screen });
            }
            if ui.checkbox(&mut self.s.live_screen, "Auto").on_hover_text("capture on every live poll (slow: ~1 s each)").changed() {
                self.send_live();
            }
            ui.checkbox(&mut self.s.pause_for_screen, "Pause CPU while capturing").on_hover_text("gives a consistent frame; not available on Xemu");
            if ui.add_enabled(en && self.screen_tex.is_some(), egui::Button::new("Save PNG…")).clicked() {
                if let Some(p) = rfd::FileDialog::new().set_file_name("mega65-screen.png").add_filter("PNG", &["png"]).save_file() {
                    self.backend.send(Cmd::SavePng { path: p, pause: self.s.pause_for_screen });
                }
            }
            if ui.add_enabled(!self.screen_text.is_empty(), egui::Button::new("Copy text")).clicked() {
                ui.output_mut(|o| o.copied_text = self.screen_text.clone());
            }
        });
        if let Some(t) = &self.screen_tex {
            let avail = ui.available_size();
            let scale = (avail.x / 720.0).min(avail.y / t.size()[1] as f32).max(0.1);
            ui.image((t.id(), egui::vec2(720.0 * scale, t.size()[1] as f32 * scale)));
        } else {
            ui.label(RichText::new("no capture yet").weak());
        }
    }

    fn send_profiler(&self) {
        let flag = parse_hex(&self.s.prof_flag).ok();
        self.backend.send(Cmd::Profiler { enabled: self.prof_on, frame_flag: flag, bucket: self.s.prof_bucket });
    }

    fn profiler_tab(&mut self, ui: &mut egui::Ui) {
        let en = self.connected;
        ui.horizontal_wrapped(|ui| {
            let label = if self.prof_on { "■ Stop" } else { "● Start" };
            if ui.add_enabled(en, egui::Button::new(label)).clicked() {
                self.prof_on = !self.prof_on;
                self.send_profiler();
            }
            if ui.add_enabled(en, egui::Button::new("Reset")).clicked() {
                self.backend.send(Cmd::ProfilerReset);
                self.fps_history.clear();
            }
            ui.label("bucket:");
            let mut changed = false;
            for b in [1u32, 16, 256, 4096] {
                changed |= ui.selectable_value(&mut self.s.prof_bucket, b, format!("{b}")).changed();
            }
            ui.label("frame flag:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.prof_flag).desired_width(70.0).hint_text("hex addr"))
                .on_hover_text("a byte that changes once per rendered frame; its change rate is shown as fps");
            changed |= r.lost_focus();
            if changed && self.prof_on {
                self.send_profiler();
            }
            ui.label(RichText::new(format!("{} samples, {:.0}/s", self.prof_total, self.prof_rate)).weak());
            if let Some(f) = self.fps_history.back() {
                ui.label(RichText::new(format!("{f:.0} fps")).strong());
            }
        });
        ui.label(RichText::new("Samples the PC without stopping the CPU; a hot single address right after a DMA trigger or a slow memory read means the CPU was stalled there. Click a row to disassemble it.").weak().small());
        // fps graph
        if !self.fps_history.is_empty() {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 80.0), egui::Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            let max = 62.0f32;
            for (i, y) in [0.0f32, 30.0, 60.0].iter().enumerate() {
                let yy = rect.bottom() - y / max * rect.height();
                p.line_segment([egui::pos2(rect.left(), yy), egui::pos2(rect.right(), yy)], egui::Stroke::new(0.5, ui.visuals().weak_text_color()));
                if i > 0 {
                    p.text(egui::pos2(rect.left() + 2.0, yy), egui::Align2::LEFT_BOTTOM, format!("{y:.0}"), egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
                }
            }
            let n = self.fps_history.len().max(2);
            let w = rect.width() / 240.0;
            for (i, f) in self.fps_history.iter().enumerate() {
                let x = rect.right() - (n - i) as f32 * w;
                let h = (f / max).min(1.0) * rect.height();
                let col = if *f < 50.0 { Color32::from_rgb(230, 90, 90) } else { Color32::from_rgb(90, 190, 110) };
                p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - h), egui::pos2(x + w.max(1.0), rect.bottom())), 0.0, col);
            }
        }
        let total = self.prof_total.max(1) as f32;
        let mut goto: Option<u32> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("profgrid").striped(true).spacing([14.0, 2.0]).show(ui, |ui| {
                ui.label(RichText::new("share").weak());
                ui.label(RichText::new("samples").weak());
                ui.label(RichText::new("address").weak());
                ui.label("");
                ui.end_row();
                for (addr, count) in self.prof_buckets.iter().take(40) {
                    let share = *count as f32 * 100.0 / total;
                    ui.label(RichText::new(format!("{share:5.1}%")).monospace());
                    ui.label(RichText::new(format!("{count}")).monospace());
                    let range = if self.s.prof_bucket > 1 { format!("${addr:04X}-${:04X}", addr + self.s.prof_bucket - 1) } else { format!("${addr:04X}") };
                    if ui.add(egui::Label::new(RichText::new(range).monospace()).sense(egui::Sense::click())).clicked() {
                        goto = Some(*addr);
                    }
                    // simple bar
                    let (r, _) = ui.allocate_exact_size(egui::vec2(160.0, 12.0), egui::Sense::hover());
                    ui.painter().rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(160.0 * (share / 100.0).min(1.0), 12.0)), 1.0, Color32::from_rgb(255, 170, 60));
                    ui.end_row();
                }
            });
        });
        if let Some(a) = goto {
            self.s.disasm_follow = false;
            self.s.disasm_addr = format!("777{a:04X}");
            self.s.tab = Tab::Disassembly;
            self.send_views();
        }
    }

    fn console_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("monitor>").monospace());
            let r = ui.add(egui::TextEdit::singleline(&mut self.console_in).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace).hint_text("raw monitor command, e.g. m0400 or r"));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && self.connected {
                let line = std::mem::take(&mut self.console_in);
                self.backend.send(Cmd::Raw(line));
                r.request_focus();
            }
        });
        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for l in &self.log {
                ui.label(RichText::new(l).monospace().small());
            }
        });
    }
}

impl eframe::App for StudioApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.s);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        if self.s.live && self.connected {
            ctx.request_repaint_after(Duration::from_millis(self.s.interval_ms.max(100)));
        }
        if self.prof_on {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        // Dropped files load as PRGs.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(p) = dropped.into_iter().next() {
            self.s.prg_path = p.display().to_string();
            if self.connected {
                self.backend.send(Cmd::LoadPrg { path: p, run: self.s.prg_run, binary: self.s.prg_binary, jump: None });
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).small());
                if let Some(e) = &self.last_error {
                    ui.label(RichText::new(format!("  {e}")).color(Color32::from_rgb(230, 90, 90)).small());
                }
            });
        });
        egui::SidePanel::left("side").default_width(270.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.sidebar(ui));
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.s.tab, Tab::Memory, "Memory");
                ui.selectable_value(&mut self.s.tab, Tab::Disassembly, "Disassembly");
                ui.selectable_value(&mut self.s.tab, Tab::Screen, "Screen");
                ui.selectable_value(&mut self.s.tab, Tab::Profiler, if self.prof_on { "Profiler ●" } else { "Profiler" });
                ui.selectable_value(&mut self.s.tab, Tab::Console, "Console / Log");
            });
            ui.separator();
            match self.s.tab {
                Tab::Memory => self.memory_tab(ui),
                Tab::Disassembly => self.disasm_tab(ui),
                Tab::Screen => self.screen_tab(ui),
                Tab::Profiler => self.profiler_tab(ui),
                Tab::Console => self.console_tab(ui),
            }
        });
    }
}
