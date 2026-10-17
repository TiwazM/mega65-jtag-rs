//! The MEGA65 Studio window: a debugger-style layout with a toolbar, a register
//! and control sidebar, tabbed memory / disassembly / screen / console views,
//! and a log. All hardware work goes through [`crate::backend`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use egui::{Color32, RichText, TextureHandle, TextureOptions};
use mega65::monitor::Registers;
use mega65::{parse_hex, transport};

use crate::backend::{Backend, Cmd, Event, SdOp};

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
    symbols_path: String,
    dma_addr: String,
    watches: Vec<WatchDef>,
    save_pause: bool,
    basic_path: String,
    basic_mixed_case: bool,
    basic_fix_hash: bool,
    basic_type_in: bool,
    basic_run: bool,
    basic_save: bool,
    basic_save_name: String,
    basic_auto_start: u16,
    basic_auto_step: u16,
    /// Index into `mega65::memsearch::PRESETS`, or PRESETS.len() for a custom range.
    search_preset: usize,
    search_start: String,
    search_end: String,
    search_width: u8,
    search_value: String,
    /// SD tab: drive the SD controller directly instead of the helper program.
    sd_direct: bool,
    /// SD tab: let the helper replace a program that is loaded in memory.
    sd_force: bool,
    sd_layout: SdLayout,
    /// Directory shown on the card.
    sd_path: String,
}

/// How uploads lay files out on the card (see `mega65::fat32::Allocation`).
#[derive(Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
enum SdLayout {
    /// Contiguous when the card has room, else fragmented; disk images must be contiguous.
    #[default]
    Auto,
    Contiguous,
    Fragmented,
}

impl SdLayout {
    fn allocation(self, name: &str) -> mega65::fat32::Allocation {
        use mega65::fat32::Allocation;
        match self {
            SdLayout::Auto => Allocation::for_name(name),
            SdLayout::Contiguous => Allocation::Contiguous,
            SdLayout::Fragmented => Allocation::Any,
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct WatchDef {
    expr: String,
    name: String,
    /// 1 or 2 bytes (little-endian word).
    width: u8,
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
            symbols_path: String::new(),
            dma_addr: String::new(),
            watches: Vec::new(),
            save_pause: true,
            basic_path: String::new(),
            basic_mixed_case: false,
            basic_fix_hash: true,
            basic_type_in: false,
            basic_run: true,
            basic_save: false,
            basic_save_name: String::new(),
            basic_auto_start: 10,
            basic_auto_step: 10,
            search_preset: 0,
            search_start: "2000".into(),
            search_end: "10000".into(),
            search_width: 1,
            search_value: String::new(),
            sd_direct: false,
            sd_force: false,
            sd_layout: SdLayout::Auto,
            sd_path: "/".into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Tab {
    Memory,
    Disassembly,
    Screen,
    Profiler,
    Watches,
    Search,
    Dma,
    Basic,
    Sd,
    Console,
}

/// The SD card tab: the directory last listed and the transfer in progress.
#[derive(Default)]
struct SdState {
    /// The directory the entries belong to (None until the first listing).
    listed: Option<String>,
    entries: Vec<mega65::fat32::DirEntry>,
    /// (free clusters, total clusters, bytes per cluster) from the card.
    free: Option<(u64, u64, u64)>,
    /// An operation is running on the worker.
    busy: bool,
    progress: Option<(u64, u64)>,
    result: Option<Result<String, String>>,
    /// Name awaiting a second click on Delete.
    confirm_delete: Option<String>,
    new_dir: String,
    /// The helper program has been started on the MEGA65 and not stopped since.
    helper_up: bool,
}

/// The Search tab: the last result of the backend's memory search.
#[derive(Default)]
struct SearchState {
    started: bool,
    count: usize,
    hits: Vec<mega65::memsearch::Hit>,
    width: u8,
    history: Vec<String>,
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
    xemu_ext: bool,
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
    progress: Option<(usize, usize)>,
    flag_candidates: Vec<mega65::probe::FlagCandidate>,
    symbols: std::sync::Arc<mega65::symbols::SymbolTable>,
    dma_text: String,
    watch_vals: Vec<WatchState>,
    watch_new: (String, String, u8),
    watch_edit: Option<(usize, String)>,
    basic: BasicState,
    search: SearchState,
    sd: SdState,
}

/// The BASIC tab: the loaded source, its tokenized preview and the last send.
#[derive(Default)]
struct BasicState {
    source: String,
    /// Tokenized with the built-in table, for the preview and Save PRG.
    preview: Option<Result<mega65::basic::Program, Vec<mega65::basic::Diagnostic>>>,
    listing: String,
    load_error: Option<String>,
    mtime: Option<std::time::SystemTime>,
    checked: Option<std::time::Instant>,
    progress: Option<(&'static str, usize, usize)>,
    sending: bool,
    result: Option<Result<mega65::basic::SendReport, String>>,
}

#[derive(Default, Clone)]
struct WatchState {
    addr: Option<u32>,
    value: Option<u32>,
    changed_at: Option<std::time::Instant>,
    /// Timestamps of recent changes, for a changes/second figure.
    changes: VecDeque<std::time::Instant>,
}

impl StudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, "settings")).unwrap_or_default();
        let backend = Backend::start(cc.egui_ctx.clone());
        backend.send(Cmd::SetLive { enabled: s.live, interval: Duration::from_millis(s.interval_ms), screen: s.live_screen });
        let ports = transport::list_ports().into_iter().map(|p| p.path).collect();
        let mut app = Self {
            backend,
            s,
            connected: false,
            xemu: false,
            xemu_ext: false,
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
            progress: None,
            flag_candidates: Vec::new(),
            symbols: Default::default(),
            dma_text: String::new(),
            watch_vals: Vec::new(),
            watch_new: (String::new(), String::new(), 1),
            watch_edit: None,
            basic: BasicState::default(),
            search: SearchState::default(),
            sd: SdState::default(),
        };
        app.watch_vals = vec![WatchState::default(); app.s.watches.len()];
        if !app.s.basic_path.is_empty() {
            app.basic_reload();
        }
        if !app.s.symbols_path.is_empty() {
            app.load_symbols(std::path::PathBuf::from(app.s.symbols_path.clone()));
        }
        app
    }

    fn load_symbols(&mut self, path: PathBuf) {
        match mega65::symbols::SymbolTable::load(&path) {
            Ok(t) => {
                self.push_log(format!("loaded {} symbols from {}", t.len(), path.display()));
                self.s.symbols_path = path.display().to_string();
                self.symbols = std::sync::Arc::new(t);
                self.backend.send(Cmd::Symbols(self.symbols.clone()));
                self.send_watches();
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    /// Try `<prg stem>.sym` / `.vs` / `.lbl` / `.labels` / `.dbg` next to a PRG.
    fn auto_symbols_for(&mut self, prg: &std::path::Path) {
        for ext in ["sym", "vs", "lbl", "labels", "dbg", "map"] {
            let p = prg.with_extension(ext);
            if p.exists() {
                self.load_symbols(p);
                return;
            }
        }
    }

    /// Parse an address that may be a symbol name.
    fn addr(&self, s: &str) -> Result<u32, String> {
        self.symbols.resolve(s)
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
                Event::Connected { device, xemu, xemu_ext, versions } => {
                    self.connected = true;
                    self.xemu = xemu;
                    self.xemu_ext = xemu_ext;
                    self.status = match versions {
                        Some(v) => format!("{device}: {} / {} / {}", v.model, v.core, v.rom),
                        None => device,
                    };
                    if xemu_ext {
                        self.status.push_str("  [Xemu ai-agent build: frame stepping available]");
                    } else if xemu {
                        self.status.push_str("  [stock Xemu: CPU can't be resumed after a halt]");
                    }
                    self.push_log(format!("connected: {}", self.status));
                    self.last_error = None;
                    self.send_views();
                    self.send_watches();
                }
                Event::Disconnected => {
                    self.connected = false;
                    self.prof_on = false;
                    self.sd.busy = false;
                    self.sd.listed = None;
                    self.sd.helper_up = false;
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
                Event::Progress(p) => self.progress = p,
                Event::DmaList { addr, text } => self.dma_text = format!("DMA list at ${addr:07X}\n{text}"),
                Event::Watches(vals) => {
                    let now = std::time::Instant::now();
                    for (i, v) in vals.into_iter().enumerate() {
                        if let Some(w) = self.watch_vals.get_mut(i) {
                            if w.value.map(|old| old != v).unwrap_or(false) {
                                w.changed_at = Some(now);
                                w.changes.push_back(now);
                            }
                            while w.changes.front().map(|t| now.duration_since(*t) > Duration::from_secs(5)).unwrap_or(false) {
                                w.changes.pop_front();
                            }
                            w.value = Some(v);
                        }
                    }
                }
                Event::BasicProgress(stage, done, total) => self.basic.progress = Some((stage, done, total)),
                Event::BasicDone(r) => {
                    self.basic.sending = false;
                    self.basic.progress = None;
                    if let Err(e) = &r {
                        self.last_error = Some(e.lines().next().unwrap_or("").to_string());
                    }
                    self.basic.result = Some(r);
                }
                Event::Search { count, hits, width, history } => {
                    self.search.started = !history.is_empty();
                    self.search.count = count;
                    self.search.hits = hits;
                    self.search.width = width;
                    self.search.history = history;
                }
                Event::FrameFlagCandidates(v) => {
                    self.push_log(format!("frame flag scan: {} candidates", v.len()));
                    self.flag_candidates = v;
                }
                Event::BreakHit(r) => {
                    self.push_log(format!("CPU stopped at ${:04X} (breakpoint/watchpoint)", r.pc));
                    self.stopped = true;
                    self.s.tab = Tab::Disassembly;
                }
                Event::SdListing { path, entries, free } => {
                    self.s.sd_path = path.clone();
                    self.sd.listed = Some(path);
                    self.sd.entries = entries;
                    self.sd.free = free;
                    self.sd.confirm_delete = None;
                }
                Event::SdProgress(p) => self.sd.progress = p,
                Event::SdDone(r) => {
                    self.sd.busy = false;
                    self.sd.progress = None;
                    if let Err(e) = &r {
                        self.last_error = Some(e.clone());
                    }
                    self.sd.result = Some(r);
                }
            }
        }
    }

    fn send_watches(&mut self) {
        let syms = self.symbols.clone();
        let mut list = Vec::new();
        self.watch_vals.resize(self.s.watches.len(), WatchState::default());
        for (i, w) in self.s.watches.iter().enumerate() {
            let a = syms.resolve(&w.expr).ok();
            self.watch_vals[i].addr = a;
            list.push((a.unwrap_or(0), w.width));
        }
        self.backend.send(Cmd::Watches(list));
    }

    fn send_views(&self) {
        for (i, a) in self.s.mem_addr.iter().enumerate() {
            if let Ok(addr) = self.symbols.resolve(a) {
                self.backend.send(Cmd::Memory { id: i, addr, len: self.s.mem_rows * 16 });
            }
        }
        let addr = if self.s.disasm_follow { None } else { self.symbols.resolve(&self.s.disasm_addr).ok() };
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
            let stock_xemu = self.xemu && !self.xemu_ext;
            let can_resume = en && !(stock_xemu && self.stopped);
            if ui.add_enabled(en, egui::Button::new("⏸ Stop")).on_hover_text("halt the CPU (t1)").clicked() {
                self.backend.send(Cmd::Stop);
            }
            if ui.add_enabled(can_resume, egui::Button::new("▶ Go")).on_hover_text("resume (t0)").clicked() {
                self.backend.send(Cmd::Go);
            }
            if ui.add_enabled(en && !stock_xemu, egui::Button::new("⏭ Step")).clicked() {
                self.backend.send(Cmd::Step(self.step_count));
            }
            ui.add(egui::DragValue::new(&mut self.step_count).range(1..=1000)).on_hover_text("instructions per step");
            if self.xemu_ext {
                if ui.add_enabled(en, egui::Button::new("⏵ Frame")).on_hover_text("run exactly one video frame, then pause (Xemu)").clicked() {
                    self.backend.send(Cmd::Frames(1));
                }
                if ui.add_enabled(en, egui::Button::new("10 frames")).on_hover_text("run ten video frames, then pause (Xemu)").clicked() {
                    self.backend.send(Cmd::Frames(10));
                }
            }
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
            ui.separator();
            let sym_label = if self.symbols.is_empty() { "Symbols…".to_string() } else { format!("Symbols ({})", self.symbols.len()) };
            if ui.button(sym_label).on_hover_text("load a label file: VICE al, KickAssembler .label, ACME/64tass name = $addr, ca65 dbg").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("label files", &["sym", "vs", "lbl", "labels", "dbg", "map", "txt"]).pick_file() {
                    self.load_symbols(p);
                }
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
                match self.addr(&self.break_in) {
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
                match self.addr(&self.watch_in) {
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
                match self.addr(&self.goto_in) {
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
                let p = PathBuf::from(&self.s.prg_path);
                self.auto_symbols_for(&p);
                self.backend.send(Cmd::LoadPrg { path: p, run: self.s.prg_run, binary: self.s.prg_binary, jump: None });
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
                if let (Ok(s), Ok(e)) = (self.addr(&self.save_start), self.addr(&self.save_end)) {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("memory.prg").save_file() {
                        self.backend.send(Cmd::SaveMemory { path: p, start: s, end: e, prg: true, pause: self.s.save_pause });
                    }
                }
            }
        });
        ui.checkbox(&mut self.s.save_pause, "halt CPU while saving").on_hover_text("consistent snapshot, but the program (and its audio) freezes for the duration; off = read while running");
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
            ui.checkbox(&mut self.s.pause_for_screen, "Pause CPU while capturing").on_hover_text("gives a consistent frame; ignored on stock Xemu, not needed on the ai-agent build (it delivers whole frames)");
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
            let (w, h) = (t.size()[0] as f32, t.size()[1] as f32);
            let scale = (avail.x / w).min(avail.y / h).max(0.1);
            ui.image((t.id(), egui::vec2(w * scale, h * scale)));
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
            if ui.add_enabled(en && self.progress.is_none(), egui::Button::new("Find…"))
                .on_hover_text("scan memory for bytes that change once per frame (takes ~10-30 s)")
                .clicked()
            {
                self.flag_candidates.clear();
                self.backend.send(Cmd::FindFrameFlag);
            }
            if let Some((d, t)) = self.progress {
                ui.add(egui::ProgressBar::new(d as f32 / t.max(1) as f32).desired_width(120.0).show_percentage());
            }
            if changed && self.prof_on {
                self.send_profiler();
            }
            ui.label(RichText::new(format!("{} samples, {:.0}/s", self.prof_total, self.prof_rate)).weak());
            if let Some(f) = self.fps_history.back() {
                ui.label(RichText::new(format!("{f:.0} fps")).strong());
            }
        });
        if !self.flag_candidates.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("candidates (click to use):").weak());
                let mut pick = None;
                for c in self.flag_candidates.iter().take(12) {
                    if ui.small_button(c.describe()).clicked() {
                        pick = Some(c.addr);
                    }
                }
                if let Some(a) = pick {
                    self.s.prof_flag = format!("{a:04X}");
                    self.fps_history.clear();
                    if self.prof_on {
                        self.send_profiler();
                    }
                }
            });
        }
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
                    let mut range = if self.s.prof_bucket > 1 { format!("${addr:04X}-${:04X}", addr + self.s.prof_bucket - 1) } else { format!("${addr:04X}") };
                    if let Some(l) = self.symbols.label(*addr, 0x400) {
                        range.push_str(&format!("  {l}"));
                    }
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

    fn watches_tab(&mut self, ui: &mut egui::Ui) {
        let en = self.connected;
        ui.horizontal(|ui| {
            ui.label("Add:");
            ui.add(egui::TextEdit::singleline(&mut self.watch_new.0).desired_width(110.0).hint_text("address or symbol"));
            ui.add(egui::TextEdit::singleline(&mut self.watch_new.1).desired_width(110.0).hint_text("name (optional)"));
            ui.selectable_value(&mut self.watch_new.2, 1u8, "byte");
            ui.selectable_value(&mut self.watch_new.2, 2u8, "word");
            if ui.add_enabled(!self.watch_new.0.is_empty(), egui::Button::new("Add")).clicked() {
                let expr = self.watch_new.0.trim().to_string();
                let name = if self.watch_new.1.trim().is_empty() { expr.clone() } else { self.watch_new.1.trim().to_string() };
                self.s.watches.push(WatchDef { expr, name, width: self.watch_new.2 });
                self.watch_new.0.clear();
                self.watch_new.1.clear();
                self.send_watches();
            }
            if let Some(r) = &self.regs {
                let _ = r;
            }
            ui.label(RichText::new("values refresh on every live poll (and during profiling); press Set… (or click the value) to change it").weak().small());
        });
        let now = std::time::Instant::now();
        let mut remove: Option<usize> = None;
        let mut write: Option<(u32, u8, u32)> = None;
        let mut resend = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("watches").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for h in ["name", "address", "hex", "dec", "bits", "changes/s", ""] {
                    ui.label(RichText::new(h).weak());
                }
                ui.end_row();
                for i in 0..self.s.watches.len() {
                    let (name, expr, width) = {
                        let w = &self.s.watches[i];
                        (w.name.clone(), w.expr.clone(), w.width)
                    };
                    let st = self.watch_vals.get(i).cloned().unwrap_or_default();
                    ui.label(RichText::new(&name).strong());
                    match st.addr {
                        Some(a) => {
                            let lab = if expr.eq_ignore_ascii_case(&format!("{a:04X}")) || expr.eq_ignore_ascii_case(&format!("${a:04X}")) { String::new() } else { format!("  ({expr})") };
                            ui.label(RichText::new(format!("${a:07X}{lab}")).monospace().weak());
                        }
                        None => {
                            ui.label(RichText::new(format!("{expr}: unresolved")).color(Color32::from_rgb(230, 90, 90)));
                        }
                    }
                    let flash = st.changed_at.map(|t| now.duration_since(t) < Duration::from_millis(800)).unwrap_or(false);
                    let colour = if flash { Color32::from_rgb(255, 200, 80) } else { ui.visuals().text_color() };
                    match st.value {
                        Some(v) => {
                            if let Some((ei, text)) = &mut self.watch_edit {
                                if *ei == i {
                                    let mut ok = false;
                                    ui.horizontal(|ui| {
                                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(60.0).font(egui::TextStyle::Monospace).hint_text("new value"))
                                            .on_hover_text("decimal, or $ followed by hex; Enter writes it, Esc cancels");
                                        r.request_focus();
                                        ok = ui.small_button("OK").clicked();
                                    });
                                    if ok || ui.input(|inp| inp.key_pressed(egui::Key::Enter)) {
                                        match (mega65::parse_num(text), st.addr) {
                                            (Ok(nv), Some(a)) => write = Some((a, width, nv)),
                                            (Err(e), _) => self.last_error = Some(e),
                                            _ => {}
                                        }
                                        self.watch_edit = None;
                                    } else if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                                        self.watch_edit = None;
                                    }
                                    ui.label("");
                                    ui.label("");
                                    ui.label("");
                                    if ui.small_button("✕").clicked() {
                                        remove = Some(i);
                                    }
                                    ui.end_row();
                                    continue;
                                }
                            }
                            let hex = if width == 2 { format!("${v:04X}") } else { format!("${v:02X}") };
                            let mut edit = false;
                            ui.horizontal(|ui| {
                                let r = ui.add(egui::Label::new(RichText::new(hex).monospace().color(colour).strong()).sense(egui::Sense::click()));
                                edit |= r.clicked();
                                edit |= ui.add_enabled(en, egui::Button::new("Set…").small()).on_hover_text("write a new value to this address").clicked();
                            });
                            if edit && en {
                                self.watch_edit = Some((i, format!("{v}")));
                            }
                            ui.label(RichText::new(format!("{v}")).monospace().color(colour));
                            ui.label(RichText::new(if width == 2 { format!("{:08b} {:08b}", v >> 8, v & 0xff) } else { format!("{v:08b}") }).monospace().color(colour).small());
                            let per_s = st.changes.len() as f32 / 5.0;
                            ui.label(RichText::new(if st.changes.is_empty() { "-".into() } else { format!("{per_s:.1}") }).monospace());
                        }
                        None => {
                            ui.label(RichText::new("?").weak());
                            ui.label("");
                            ui.label("");
                            ui.label("");
                        }
                    }
                    if ui.small_button("✕").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(i) = remove {
            self.s.watches.remove(i);
            self.watch_vals.remove(i.min(self.watch_vals.len().saturating_sub(1)));
            resend = true;
        }
        if let Some((a, w, v)) = write {
            let data = if w == 2 { vec![v as u8, (v >> 8) as u8] } else { vec![v as u8] };
            self.backend.send(Cmd::WriteBytes { addr: a, data });
        }
        if resend {
            self.send_watches();
        }
        if self.connected && !self.s.watches.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
    }

    /// The memory range the Search tab is set to, as (start, len) regions.
    fn search_regions(&self) -> Result<Vec<(u32, usize)>, String> {
        use mega65::memsearch::PRESETS;
        if let Some(&(_, start, len)) = PRESETS.get(self.s.search_preset) {
            return Ok(vec![(start, len)]);
        }
        let s = self.addr(&self.s.search_start)?;
        let e = self.addr(&self.s.search_end)?;
        if e <= s {
            return Err("the end of the range must be after its start".into());
        }
        Ok(vec![(s, (e - s) as usize)])
    }

    fn search_step(&mut self, filter: mega65::memsearch::Filter, fresh: bool) {
        let start = if fresh || !self.search.started {
            match self.search_regions() {
                Ok(r) => Some((r, self.s.search_width)),
                Err(e) => {
                    self.last_error = Some(e);
                    return;
                }
            }
        } else {
            None
        };
        self.last_error = None;
        self.backend.send(Cmd::SearchStep { filter, start });
    }

    fn search_tab(&mut self, ui: &mut egui::Ui) {
        use mega65::memsearch::{Filter, PRESETS};
        let en = self.connected && self.progress.is_none();
        let started = self.search.started;
        ui.horizontal_wrapped(|ui| {
            ui.label("Where:");
            let name = |i: usize| PRESETS.get(i).map(|p| p.0.to_string()).unwrap_or_else(|| "Custom range".into());
            egui::ComboBox::from_id_salt("search_preset").selected_text(name(self.s.search_preset)).show_ui(ui, |ui| {
                for i in 0..=PRESETS.len() {
                    ui.selectable_value(&mut self.s.search_preset, i, name(i));
                }
            });
            if self.s.search_preset >= PRESETS.len() {
                ui.add(egui::TextEdit::singleline(&mut self.s.search_start).desired_width(80.0).hint_text("start"));
                ui.label("to");
                ui.add(egui::TextEdit::singleline(&mut self.s.search_end).desired_width(80.0).hint_text("end"))
                    .on_hover_text("28-bit hex addresses, e.g. 8000000 for attic RAM");
            }
            ui.label("Size:");
            ui.selectable_value(&mut self.s.search_width, 1u8, "byte").on_hover_text("values 0-255: lives, energy, level, each score digit pair");
            ui.selectable_value(&mut self.s.search_width, 2u8, "word").on_hover_text("values 0-65535 stored low byte first: timers, positions, some scores");
            if started {
                ui.label(RichText::new("(the range and size apply to the next new search)").weak().small());
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Value:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.search_value).desired_width(80.0).hint_text("e.g. 3 or $12"));
            let value = mega65::parse_num(&self.s.search_value).ok();
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !started {
                if ui.add_enabled(en && value.is_some(), egui::Button::new("Search for value")).clicked() || (enter && value.is_some() && en) {
                    self.search_step(Filter::Equal(value.unwrap_or(0)), true);
                }
                if ui.add_enabled(en, egui::Button::new("Start with unknown value"))
                    .on_hover_text("keep every address; narrow down with changed / increased / decreased as you play")
                    .clicked()
                {
                    self.search_step(Filter::Any, true);
                }
            } else {
                if ui.add_enabled(en && value.is_some(), egui::Button::new("= value")).clicked() || (enter && value.is_some() && en) {
                    self.search_step(Filter::Equal(value.unwrap_or(0)), false);
                }
                if ui.add_enabled(en && value.is_some(), egui::Button::new("≠ value")).clicked() {
                    self.search_step(Filter::NotEqual(value.unwrap_or(0)), false);
                }
                ui.separator();
                if ui.add_enabled(en, egui::Button::new("Increased")).clicked() {
                    self.search_step(Filter::Increased, false);
                }
                if ui.add_enabled(en, egui::Button::new("Decreased")).clicked() {
                    self.search_step(Filter::Decreased, false);
                }
                if ui.add_enabled(en, egui::Button::new("Changed")).clicked() {
                    self.search_step(Filter::Changed, false);
                }
                if ui.add_enabled(en, egui::Button::new("Unchanged")).clicked() {
                    self.search_step(Filter::Unchanged, false);
                }
                if ui.add_enabled(en && value.is_some(), egui::Button::new("Up by value")).on_hover_text("increased by exactly the value").clicked() {
                    self.search_step(Filter::IncreasedBy(value.unwrap_or(0)), false);
                }
                if ui.add_enabled(en && value.is_some(), egui::Button::new("Down by value")).clicked() {
                    self.search_step(Filter::DecreasedBy(value.unwrap_or(0)), false);
                }
                ui.separator();
                if ui.add_enabled(self.progress.is_none(), egui::Button::new("Start over")).clicked() {
                    self.backend.send(Cmd::SearchReset);
                }
            }
            if let Some((d, t)) = self.progress {
                ui.add(egui::ProgressBar::new(d as f32 / t.max(1) as f32).desired_width(120.0).show_percentage());
            }
        });
        if !started {
            ui.label(
                RichText::new(
                    "Find where a game keeps a number: type the value you can see on screen (lives, energy, level) and search for it, or start with an unknown value. \
                     Then play until it changes and press Increased / Decreased / Changed / Unchanged (or type the new value and press = value) until only a few addresses are left. \
                     The CPU keeps running and nothing is written. Tip: scores are often stored as BCD, so a score of 1250 is the bytes $12 and $50.",
                )
                .weak()
                .small(),
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{} address{} match", self.search.count, if self.search.count == 1 { "" } else { "es" })).strong());
            ui.label(RichText::new(self.search.history.join("  ›  ")).weak().small());
        });
        if self.search.count == 0 {
            ui.label(RichText::new("Nothing left. Press Start over; the value may be stored differently (word instead of byte, BCD, or in another memory range).").weak());
            return;
        }
        let shown = self.search.hits.len();
        if shown < self.search.count {
            ui.label(RichText::new(format!("showing the first {shown}; keep filtering to narrow the list")).weak().small());
        }
        let width = self.search.width;
        let mut add: Vec<u32> = Vec::new();
        let mut peek: Option<u32> = None;
        if self.search.count <= 16 {
            if ui.small_button("Watch all").clicked() {
                add.extend(self.search.hits.iter().map(|h| h.addr));
            }
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("searchgrid").striped(true).spacing([14.0, 2.0]).show(ui, |ui| {
                for h in ["address", "value", "hex", "before", "", ""] {
                    ui.label(RichText::new(h).weak());
                }
                ui.end_row();
                for h in &self.search.hits {
                    let a = h.addr;
                    let lab = if a < 0x10000 { format!("${a:04X}") } else { format!("${a:07X}") };
                    ui.label(RichText::new(lab).monospace());
                    ui.label(RichText::new(format!("{}", h.value)).monospace().strong());
                    ui.label(RichText::new(if width == 2 { format!("${:04X}", h.value) } else { format!("${:02X}", h.value) }).monospace());
                    ui.label(RichText::new(if h.prev == h.value { "-".to_string() } else { format!("{}", h.prev) }).monospace().weak());
                    if ui.small_button("→ Watch").on_hover_text("add to the Watches tab, where the value can be edited").clicked() {
                        add.push(a);
                    }
                    if ui.small_button("Memory").on_hover_text("show in the Memory tab").clicked() {
                        peek = Some(a);
                    }
                    ui.end_row();
                }
            });
        });
        if !add.is_empty() {
            for a in add {
                if self.s.watches.iter().any(|w| self.symbols.resolve(&w.expr).ok() == Some(a) && w.width == width) {
                    continue;
                }
                let expr = if a < 0x10000 { format!("{a:04X}") } else { format!("{a:07X}") };
                self.s.watches.push(WatchDef { expr: expr.clone(), name: format!("found {expr}"), width });
            }
            self.send_watches();
            self.s.tab = Tab::Watches;
        }
        if let Some(a) = peek {
            self.s.mem_addr[0] = format!("{:X}", a & !0xf);
            self.s.tab = Tab::Memory;
            self.send_views();
        }
    }

    fn dma_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("DMA list at:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.dma_addr).desired_width(120.0).hint_text("hex address or symbol"));
            let go = ui.add_enabled(self.connected, egui::Button::new("Decode")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if go && self.connected {
                match self.addr(&self.s.dma_addr) {
                    Ok(a) => self.backend.send(Cmd::DmaList { addr: a }),
                    Err(e) => self.last_error = Some(e),
                }
            }
            ui.label(RichText::new("the address a program writes to $D700/$D701/$D702 (or $D705); chained lists are followed").weak().small());
        });
        egui::ScrollArea::both().show(ui, |ui| {
            ui.label(RichText::new(&self.dma_text).monospace());
        });
    }

    fn basic_options(&self) -> mega65::basic::Options {
        mega65::basic::Options {
            case: if self.s.basic_mixed_case { mega65::basic::CaseMode::Mixed } else { mega65::basic::CaseMode::Upper },
            auto_start: self.s.basic_auto_start,
            auto_step: self.s.basic_auto_step.max(1),
            fix_hash: self.s.basic_fix_hash,
            c64: false,
        }
    }

    /// Read the source file again and redo the preview.
    fn basic_reload(&mut self) {
        let path = PathBuf::from(&self.s.basic_path);
        self.basic.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        self.basic.checked = Some(std::time::Instant::now());
        match std::fs::read(&path) {
            Ok(b) => {
                self.basic.source = mega65::basic::decode_source(&b);
                self.basic.load_error = None;
                if self.s.basic_save_name.is_empty() {
                    if let Some(stem) = path.file_stem() {
                        self.s.basic_save_name = stem.to_string_lossy().to_uppercase().chars().take(16).collect();
                    }
                }
            }
            Err(e) => {
                self.basic.source.clear();
                self.basic.load_error = Some(format!("{}: {e}", path.display()));
            }
        }
        self.basic_recompute();
    }

    fn basic_recompute(&mut self) {
        if self.basic.source.is_empty() {
            self.basic.preview = None;
            self.basic.listing.clear();
            return;
        }
        let kw = mega65::basic::Keywords::basic65();
        let opts = self.basic_options();
        let r = mega65::basic::tokenize(&self.basic.source, &kw, &opts);
        self.basic.listing = match &r {
            Ok(p) => p.listing(&kw, opts.case),
            Err(_) => String::new(),
        };
        self.basic.preview = Some(r);
    }

    fn basic_tab(&mut self, ui: &mut egui::Ui) {
        // Pick up edits made in the PC editor.
        if !self.s.basic_path.is_empty() && self.basic.checked.map(|t| t.elapsed() > Duration::from_secs(1)).unwrap_or(true) {
            let m = std::fs::metadata(&self.s.basic_path).and_then(|m| m.modified()).ok();
            if m != self.basic.mtime {
                self.basic_reload();
            }
            self.basic.checked = Some(std::time::Instant::now());
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        ui.label(RichText::new("Write BASIC on the PC as plain text (.bas or .txt), then send it to the MEGA65 as a tokenized program ready to RUN; no D81 or IMPORT needed. The MEGA65 must be at the READY prompt.").weak().small());
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Source:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.basic_path).desired_width(360.0).hint_text("program.bas"));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.basic_reload();
            }
            if ui.button("Browse…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("BASIC source", &["bas", "BAS", "txt", "TXT", "b65", "lst"]).add_filter("All files", &["*"]).pick_file() {
                    self.s.basic_path = p.display().to_string();
                    self.s.basic_save_name.clear();
                    self.basic_reload();
                }
            }
            if ui.add_enabled(!self.s.basic_path.is_empty(), egui::Button::new("Reload")).clicked() {
                self.basic_reload();
            }
        });
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .checkbox(&mut self.s.basic_fix_hash, "fix PRINT # / INPUT #")
                .on_hover_text("BASIC reads PRINT #4 (with a space) or ?#4 as PRINT followed by #4, a ?SYNTAX ERROR. This sends them as PRINT#4, the way they work when retyped.")
                .changed();
            ui.separator();
            ui.label("letters in strings:");
            changed |= ui.radio_value(&mut self.s.basic_mixed_case, false, "capitals").on_hover_text("every letter as typed on the MEGA65 without SHIFT (shows as capitals in the normal character set)").changed();
            changed |= ui.radio_value(&mut self.s.basic_mixed_case, true, "keep upper/lower case").on_hover_text("for the lower-case character set (PRINT CHR$(14)): lower case stays lower case, capitals become SHIFTed letters").changed();
            if let Some(Ok(p)) = &self.basic.preview {
                if p.auto_numbered || self.s.basic_auto_start != 10 || self.s.basic_auto_step != 10 {
                    ui.separator();
                    ui.label("no line numbers: number from");
                    changed |= ui.add(egui::DragValue::new(&mut self.s.basic_auto_start).range(0..=63999)).changed();
                    ui.label("step");
                    changed |= ui.add(egui::DragValue::new(&mut self.s.basic_auto_step).range(1..=1000)).changed();
                }
            }
        });
        if changed {
            self.basic_recompute();
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("method:");
            ui.radio_value(&mut self.s.basic_type_in, false, "direct (fast)").on_hover_text("tokenize here and write the program straight into BASIC memory, then read it back to check every byte");
            ui.radio_value(&mut self.s.basic_type_in, true, "type it in (slow check)").on_hover_text("type each line into the MEGA65's keyboard buffer so its own tokenizer is used, then compare with the PC's result line by line");
            ui.separator();
            ui.checkbox(&mut self.s.basic_run, "RUN afterwards");
            ui.checkbox(&mut self.s.basic_save, "SAVE on the MEGA65 as").on_hover_text("DSAVE to the MEGA65's current drive before running; an existing file with this name is never replaced");
            ui.add_enabled(self.s.basic_save, egui::TextEdit::singleline(&mut self.s.basic_save_name).desired_width(140.0).char_limit(16));
        });
        let ok = matches!(self.basic.preview, Some(Ok(_)));
        ui.horizontal(|ui| {
            let can = self.connected && !self.busy && ok && !self.basic.sending;
            let send = ui.add_enabled(can, egui::Button::new(RichText::new("Send BASIC").strong()).min_size(egui::vec2(110.0, 26.0)));
            if send.clicked() {
                self.basic_reload();
                if let Some(Ok(_)) = &self.basic.preview {
                    self.basic.sending = true;
                    self.basic.result = None;
                    self.basic.progress = Some(("starting", 0, 1));
                    self.backend.send(Cmd::SendBasic {
                        source: self.basic.source.clone(),
                        opts: self.basic_options(),
                        send: mega65::basic::SendOptions {
                            method: if self.s.basic_type_in { mega65::basic::Method::Type } else { mega65::basic::Method::Direct },
                            save_as: if self.s.basic_save { Some(self.s.basic_save_name.clone()) } else { None },
                            run: self.s.basic_run,
                        },
                    });
                }
            }
            if !self.connected {
                ui.label(RichText::new("connect to a MEGA65 or Xemu first").weak());
            }
            if ui.add_enabled(ok, egui::Button::new("Save PRG…")).on_hover_text("write the tokenized program as a PRG file on the PC").clicked() {
                let stem = std::path::Path::new(&self.s.basic_path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "program".into());
                if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{stem}.prg")).save_file() {
                    if let Some(Ok(p)) = &self.basic.preview {
                        match std::fs::write(&path, p.prg(0x2001)) {
                            Ok(()) => self.push_log(format!("saved {} ({} lines)", path.display(), p.lines.len())),
                            Err(e) => self.last_error = Some(e.to_string()),
                        }
                    }
                }
            }
            if let Some((stage, done, total)) = self.basic.progress {
                let f = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(f).desired_width(200.0).text(if total > 1 { format!("{stage} {done}/{total}") } else { stage.to_string() }));
            }
        });
        let red = Color32::from_rgb(230, 90, 90);
        let amber = Color32::from_rgb(220, 170, 60);
        let green = Color32::from_rgb(90, 190, 90);
        if let Some(e) = &self.basic.load_error {
            ui.label(RichText::new(e).color(red));
        }
        match &self.basic.result {
            Some(Ok(r)) => {
                let (txt, col) = if r.verified { (format!("Sent: {}", r.summary()), green) } else { (format!("Sent, but the MEGA65 stored something else: {}", r.summary()), red) };
                ui.label(RichText::new(txt).color(col));
                for (n, pc, m) in &r.differences {
                    ui.label(RichText::new(format!("line {n}: PC {pc}  |  MEGA65 {m}")).monospace().color(red));
                }
            }
            Some(Err(e)) => {
                for l in e.lines() {
                    ui.label(RichText::new(l).color(red));
                }
            }
            None => {}
        }
        match &self.basic.preview {
            Some(Err(errs)) => {
                for e in errs.iter().take(50) {
                    ui.label(RichText::new(e.to_string()).color(red));
                }
            }
            Some(Ok(p)) => {
                for w in p.warnings.iter().take(50) {
                    ui.label(RichText::new(w.to_string()).color(amber).small());
                }
            }
            None => {}
        }
        ui.separator();
        let n = match &self.basic.preview {
            Some(Ok(p)) => format!("{} lines, {} bytes", p.lines.len(), p.bytes(0x2001).len()),
            _ => String::new(),
        };
        ui.columns(2, |cols| {
            cols[0].label(RichText::new("Source").strong());
            egui::ScrollArea::both().id_salt("basic_src").show(&mut cols[0], |ui| {
                ui.label(RichText::new(&self.basic.source).monospace());
            });
            cols[1].label(RichText::new(format!("As the MEGA65 will LIST it   {n}")).strong());
            egui::ScrollArea::both().id_salt("basic_list").show(&mut cols[1], |ui| {
                ui.label(RichText::new(&self.basic.listing).monospace());
            });
        });
    }

    fn sd_send(&mut self, op: SdOp) {
        self.sd.busy = true;
        self.sd.result = None;
        self.sd.confirm_delete = None;
        if !self.s.sd_direct {
            self.sd.helper_up = !matches!(op, SdOp::Quit);
        }
        self.backend.send(Cmd::Sd { op, direct: self.s.sd_direct, force: self.s.sd_force });
    }

    fn sd_upload(&mut self, local: PathBuf) {
        let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let alloc = self.s.sd_layout.allocation(&name);
        self.sd.progress = Some((0, 1));
        self.sd_send(SdOp::Put { local, dir: self.s.sd_path.clone(), alloc });
    }

    fn sd_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Copy files between this computer and the SD card in the MEGA65. Uploads go into one unbroken run of clusters when the card has room, which disk images need before the MEGA65 can mount them.").weak().small());
        ui.horizontal_wrapped(|ui| {
            ui.label("Transfer:");
            ui.radio_value(&mut self.s.sd_direct, false, "fast (helper program)").on_hover_text("runs mega65-tools' remotesd helper on the MEGA65; about 160 KB/s");
            ui.radio_value(&mut self.s.sd_direct, true, "gentle (direct, slow)").on_hover_text("drives the SD controller through the monitor; nothing in memory changes, but the CPU is paused during each transfer and it is many times slower");
            if !self.s.sd_direct {
                ui.checkbox(&mut self.s.sd_force, "replace the program in memory").on_hover_text("the helper is a C64 program at $0801; without this the transfer refuses to start while a program is loaded");
            }
            ui.separator();
            ui.label("Layout:");
            ui.radio_value(&mut self.s.sd_layout, SdLayout::Auto, "contiguous when possible").on_hover_text("one unbroken run when the card has one, otherwise fragmented; .d81/.d64/.d71/.d65 must be contiguous and fail otherwise");
            ui.radio_value(&mut self.s.sd_layout, SdLayout::Contiguous, "must be contiguous").on_hover_text("fail, leaving the card unchanged, if there is no run of free clusters big enough");
            ui.radio_value(&mut self.s.sd_layout, SdLayout::Fragmented, "fragmented is fine").on_hover_text("take free clusters in order, the way older versions did");
        });
        ui.label(
            RichText::new(if self.s.sd_direct {
                "Gentle mode pauses the MEGA65 while it reads or writes the card and resumes it afterwards. A game with music will stutter."
            } else {
                "Fast mode switches the MEGA65 to C64 mode and loads a small helper program, replacing whatever was in memory. It stays there between transfers; use Leave SD mode to get back to BASIC."
            })
            .weak()
            .small(),
        );
        let en = self.connected && !self.busy && !self.sd.busy;
        ui.horizontal(|ui| {
            ui.label("Folder:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.sd_path).desired_width(240.0).hint_text("/"));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && en {
                self.sd_send(SdOp::List { path: self.s.sd_path.clone() });
            }
            let at_root = self.s.sd_path.trim_matches('/').is_empty();
            if ui.add_enabled(en && !at_root, egui::Button::new("Up")).clicked() {
                let parent = match self.s.sd_path.trim_end_matches('/').rsplit_once('/') {
                    Some(("", _)) | None => "/".to_string(),
                    Some((d, _)) => d.to_string(),
                };
                self.sd_send(SdOp::List { path: parent });
            }
            if ui.add_enabled(en, egui::Button::new(if self.sd.listed.is_some() { "Refresh" } else { "Show card" })).clicked() {
                self.sd_send(SdOp::List { path: self.s.sd_path.clone() });
            }
            ui.separator();
            if ui.add_enabled(en, egui::Button::new(RichText::new("Upload file…").strong())).on_hover_text("copy a file from this computer into this folder").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.sd_upload(p);
                }
            }
            ui.label(RichText::new("or drop a file on the window").weak().small());
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.sd.new_dir).desired_width(110.0).hint_text("new folder"));
            if ui.add_enabled(en && !self.sd.new_dir.trim().is_empty(), egui::Button::new("Create folder")).clicked() {
                let path = join_sd(&self.s.sd_path, self.sd.new_dir.trim());
                self.sd.new_dir.clear();
                self.sd_send(SdOp::Mkdir { path });
            }
            if self.sd.helper_up && !self.s.sd_direct {
                ui.separator();
                if ui.add_enabled(en, egui::Button::new("Leave SD mode")).on_hover_text("stop the helper program and return the MEGA65 to BASIC").clicked() {
                    self.sd_send(SdOp::Quit);
                }
            }
        });
        if !self.connected {
            ui.label(RichText::new("connect to a MEGA65 first").weak());
        }
        let red = Color32::from_rgb(230, 90, 90);
        let green = Color32::from_rgb(90, 190, 90);
        ui.horizontal(|ui| {
            if let Some((done, total)) = self.sd.progress {
                let f = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(f).desired_width(260.0).text(format!("{} / {}", fmt_size(done), fmt_size(total))));
            } else if self.sd.busy {
                ui.spinner();
                ui.label(RichText::new("talking to the card…").weak());
            }
            match &self.sd.result {
                Some(Ok(m)) if !m.is_empty() => {
                    ui.label(RichText::new(m).color(green));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e).color(red));
                }
                _ => {}
            }
        });
        ui.separator();
        let Some(listed) = self.sd.listed.clone() else {
            if self.connected && !self.sd.busy {
                ui.label(RichText::new("Press Show card to list the SD card's root folder.").weak());
            }
            return;
        };
        if let Some((free, total, bpc)) = self.sd.free {
            ui.label(RichText::new(format!("{listed}   {} free of {}", fmt_size(free * bpc), fmt_size(total * bpc))).small());
        } else {
            ui.label(RichText::new(&listed).small());
        }
        let entries = self.sd.entries.clone();
        let mut action: Option<SdOp> = None;
        let mut confirm: Option<Option<String>> = None;
        egui::ScrollArea::vertical().id_salt("sd_list").show(ui, |ui| {
            egui::Grid::new("sdgrid").striped(true).spacing([12.0, 3.0]).min_col_width(60.0).show(ui, |ui| {
                ui.label(RichText::new("Name").strong());
                ui.label(RichText::new("Size").strong());
                ui.label(RichText::new("Modified").strong());
                ui.label("");
                ui.end_row();
                for e in &entries {
                    let path = join_sd(&listed, &e.name);
                    if e.is_dir() {
                        ui.label(RichText::new(format!("📁 {}", e.name)).strong());
                        ui.label("");
                    } else {
                        ui.label(&e.name);
                        ui.label(RichText::new(fmt_size(e.size as u64)).monospace());
                    }
                    ui.label(RichText::new(&e.mtime).weak().small());
                    ui.horizontal(|ui| {
                        if e.is_dir() {
                            if ui.add_enabled(en, egui::Button::new("Open")).clicked() {
                                action = Some(SdOp::List { path: path.clone() });
                            }
                        } else {
                            if ui.add_enabled(en, egui::Button::new("Download")).clicked() {
                                if let Some(local) = rfd::FileDialog::new().set_file_name(&e.name).save_file() {
                                    action = Some(SdOp::Get { remote: path.clone(), local });
                                }
                            }
                            let is_d81 = e.name.to_ascii_lowercase().ends_with(".d81");
                            if is_d81
                                && listed.trim_matches('/').is_empty()
                                && !self.s.sd_direct
                                && ui.add_enabled(en, egui::Button::new("Mount")).on_hover_text("ask the Hypervisor to mount this disk image as drive 8").clicked()
                            {
                                action = Some(SdOp::Mount { name: e.name.clone() });
                            }
                        }
                        if self.sd.confirm_delete.as_deref() == Some(path.as_str()) {
                            let what = if e.is_dir() { "Delete folder and everything in it" } else { "Really delete" };
                            if ui.add_enabled(en, egui::Button::new(RichText::new(what).color(red))).clicked() {
                                action = Some(SdOp::Delete { path: path.clone() });
                            }
                            if ui.button("Cancel").clicked() {
                                confirm = Some(None);
                            }
                        } else if ui.add_enabled(en, egui::Button::new("Delete")).clicked() {
                            confirm = Some(Some(path.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
            if entries.is_empty() {
                ui.label(RichText::new("empty folder").weak());
            }
        });
        if let Some(c) = confirm {
            self.sd.confirm_delete = c;
        }
        if let Some(op) = action {
            if matches!(op, SdOp::Get { .. }) {
                self.sd.progress = Some((0, 1));
            }
            self.sd_send(op);
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
            if self.s.tab == Tab::Sd {
                if self.connected && !self.sd.busy && p.is_file() {
                    self.sd_upload(p);
                }
                return;
            }
            if is_basic_source(&p) {
                self.s.basic_path = p.display().to_string();
                self.basic_reload();
                self.s.tab = Tab::Basic;
                return;
            }
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
                ui.selectable_value(&mut self.s.tab, Tab::Watches, if self.s.watches.is_empty() { "Watches".to_string() } else { format!("Watches ({})", self.s.watches.len()) });
                ui.selectable_value(&mut self.s.tab, Tab::Search, if self.search.started { format!("Search ({})", self.search.count) } else { "Search".to_string() });
                ui.selectable_value(&mut self.s.tab, Tab::Dma, "DMA");
                ui.selectable_value(&mut self.s.tab, Tab::Basic, "BASIC");
                ui.selectable_value(&mut self.s.tab, Tab::Sd, if self.sd.busy { "SD card ●" } else { "SD card" });
                ui.selectable_value(&mut self.s.tab, Tab::Console, "Console / Log");
            });
            ui.separator();
            match self.s.tab {
                Tab::Memory => self.memory_tab(ui),
                Tab::Disassembly => self.disasm_tab(ui),
                Tab::Screen => self.screen_tab(ui),
                Tab::Profiler => self.profiler_tab(ui),
                Tab::Watches => self.watches_tab(ui),
                Tab::Search => self.search_tab(ui),
                Tab::Dma => self.dma_tab(ui),
                Tab::Basic => self.basic_tab(ui),
                Tab::Sd => self.sd_tab(ui),
                Tab::Console => self.console_tab(ui),
            }
        });
    }
}

/// `/GAMES` + `X.D81` -> `/GAMES/X.D81`.
fn join_sd(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn fmt_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn is_basic_source(p: &std::path::Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("bas" | "txt" | "b65" | "lst"))
}
