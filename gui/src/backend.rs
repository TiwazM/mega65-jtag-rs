//! The hardware side of the GUI: one worker thread owns the `Machine` and runs
//! every monitor exchange, so the UI never blocks on the serial link and two
//! panels can never interleave commands. The UI sends [`Cmd`]s and receives
//! [`Event`]s over channels.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mega65::fat32::{Allocation, DirEntry, Fat32};
use mega65::machine::{Machine, Prg, Versions};
use mega65::monitor::{Monitor, Registers};
use mega65::screen::VideoState;
use mega65::sdcard::{BlockDevice, DirectSd, HelperSd};

#[derive(Debug, Clone)]
pub enum Cmd {
    Connect { device: String, baud: u32 },
    Disconnect,
    /// Re-read registers, memory views and disassembly now.
    Refresh,
    Stop,
    Go,
    GoTo(u16),
    Step(usize),
    /// Xemu (ai-agent build): run exactly N video frames, then pause.
    Frames(u32),
    SetBreak(Option<u16>),
    SetWatch(Option<u32>),
    SetIrqs(bool),
    /// Register (or move) a memory view; it is refreshed with every poll.
    Memory { id: usize, addr: u32, len: usize },
    WriteBytes { addr: u32, data: Vec<u8> },
    /// Disassemble `pages` x 16 instructions from `addr` (None = follow PC).
    Disasm { addr: Option<u32>, pages: usize },
    Screenshot { pause: bool },
    SavePng { path: PathBuf, pause: bool },
    LoadPrg { path: PathBuf, run: bool, binary: bool, jump: Option<u16> },
    LoadBinary { path: PathBuf, addr: u32 },
    SaveMemory { path: PathBuf, start: u32, end: u32, prg: bool, pause: bool },
    TypeText { text: String, ret: bool, buffer: bool },
    /// Hold exactly these keyboard matrix keys down (scan codes, at most three)
    /// until the next `HoldKeys`; an empty list releases everything.
    HoldKeys(Vec<u8>),
    Raw(String),
    Reset,
    Go64,
    Video { pal: bool },
    /// Live polling of registers/memory/disassembly (and optionally the screen).
    SetLive { enabled: bool, interval: Duration, screen: bool },
    /// Continuous PC sampling; `frame_flag` is a byte that changes once per
    /// rendered frame (its change rate is reported as fps). Replaces normal
    /// polling while enabled.
    Profiler { enabled: bool, frame_flag: Option<u32>, bucket: u32 },
    ProfilerReset,
    /// Scan memory for bytes that change once per frame.
    FindFrameFlag,
    /// Use this symbol table for disassembly annotations.
    Symbols(std::sync::Arc<mega65::symbols::SymbolTable>),
    /// Decode the DMA list at an address.
    DmaList { addr: u32 },
    /// Addresses (and widths in bytes, 1 or 2) to read on every poll.
    Watches(Vec<(u32, u8)>),
    /// Tokenize BASIC source and put it on the MEGA65 (see `mega65::basic`).
    SendBasic { source: String, opts: mega65::basic::Options, send: mega65::basic::SendOptions },
    /// One memory-search step (see `mega65::memsearch`). `start` begins a new
    /// search over these (start, len) regions with this width; otherwise the
    /// filter refines the current one.
    SearchStep { filter: mega65::memsearch::Filter, start: Option<(Vec<(u32, usize)>, u8)> },
    SearchReset,
    /// One SD card operation. `direct` drives the SD controller through the
    /// monitor (slow, pauses the CPU, touches no memory); otherwise the
    /// mega65-tools helper program is put in C64 memory and stays resident
    /// until `SdOp::Quit`. `force` lets the helper replace a loaded program.
    Sd { op: SdOp, direct: bool, force: bool },
    /// Read the Hypervisor's DOS state and the F011 disk-image registers.
    Hyppo,
    /// Fill `[start, end)` with a byte.
    Fill { start: u32, end: u32, value: u8 },
    /// Serve this D81 from the PC as the internal drive (virtual F011), or
    /// stop serving with None. Starting restarts the MEGA65 so the Hypervisor
    /// boots with the drive attached; requests are answered between other
    /// commands.
    Vf011(Option<PathBuf>),
    Shutdown,
}

/// What the SD tab asks for. Paths are absolute on the card (`/GAMES/X.D81`).
#[derive(Debug, Clone)]
pub enum SdOp {
    List { path: String },
    /// Upload a PC file into directory `dir` under its own file name.
    Put { local: PathBuf, dir: String, alloc: Allocation },
    Get { remote: String, local: PathBuf },
    /// Delete a file, or a directory with everything in it.
    Delete { path: String },
    Mkdir { path: String },
    /// Ask the Hypervisor to mount a D81 from the card's root directory.
    Mount { name: String },
    /// Stop the helper program and return the MEGA65 to BASIC.
    Quit,
}

#[derive(Debug)]
pub enum Event {
    Connected { device: String, xemu: bool, xemu_ext: bool, versions: Option<Versions> },
    Disconnected,
    Registers { regs: Registers, stopped: bool },
    Memory { id: usize, addr: u32, data: Vec<u8> },
    Disasm { addr: u32, text: String },
    Screen { width: usize, height: usize, rgb: Vec<u8>, text: String },
    Log(String),
    Error(String),
    Busy(bool),
    /// The CPU stopped on its own (breakpoint/watchpoint).
    BreakHit(Registers),
    /// Profiler snapshot: total samples, (bucket address, count) sorted by count,
    /// sampling rate, and the frame flag's change rate if one is configured.
    Profile { total: u32, buckets: Vec<(u32, u32)>, samples_per_sec: f32, fps: Option<f32> },
    /// Progress of a long operation, (done, total), or None when finished.
    Progress(Option<(usize, usize)>),
    FrameFlagCandidates(Vec<mega65::probe::FlagCandidate>),
    DmaList { addr: u32, text: String },
    /// Current watch values, in the order they were registered.
    Watches(Vec<u32>),
    /// Send BASIC progress: stage, done, total.
    BasicProgress(&'static str, usize, usize),
    /// Send BASIC finished (or failed, with the reason).
    BasicDone(Result<mega65::basic::SendReport, String>),
    /// Memory search state after a step: how many addresses match, the first
    /// `hits.len()` of them, the width, and the step history.
    Search { count: usize, hits: Vec<mega65::memsearch::Hit>, width: u8, history: Vec<String> },
    /// A directory of the SD card, with (free, total, bytes per cluster) if
    /// known, and the first sector of each disk image in it (name, sector), so
    /// the Drives tab can name what the Hypervisor has mounted.
    SdListing { path: String, entries: Vec<DirEntry>, free: Option<(u64, u64, u64)>, images: Vec<(String, u32)> },
    /// Transfer progress (done, total bytes), or None when finished.
    SdProgress(Option<(u64, u64)>),
    /// An SD operation finished: a one-line summary, or why it failed.
    SdDone(Result<String, String>),
    /// Hypervisor state: the DOS status text, the F011 disk image start
    /// sectors for drives 0 and 1 (0 or $FFFFFFFF = none), and the raw $D68B control byte.
    Hyppo { text: String, image0: u32, image1: u32, ctrl: u8 },
    /// Virtual drive state: the image being served (None = stopped), the
    /// request counts, and a note when something notable happened.
    Vf011 { serving: Option<String>, reads: u64, writes: u64, note: Option<String> },
}

/// What an SD operation produced: a line for the user and, when the directory
/// shown may have changed, its fresh listing.
struct SdResult {
    message: String,
    listing: Option<SdDir>,
}

/// A listed directory: path, entries, free space, (name, first sector) of its disk images.
type SdDir = (String, Vec<DirEntry>, Option<(u64, u64, u64)>, Vec<(String, u32)>);

/// Hits sent to the UI per search step; the count is always exact.
const SEARCH_HITS_SHOWN: usize = 1000;

pub struct Backend {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<Event>,
    handle: Option<JoinHandle<()>>,
}

impl Backend {
    pub fn start(ctx: egui::Context) -> Self {
        let (tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("mega65-worker".into())
            .spawn(move || Worker::new(cmd_rx, ev_tx, ctx).run())
            .expect("spawn worker");
        Self { tx, rx, handle: Some(handle) }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Worker {
    rx: Receiver<Cmd>,
    tx: Sender<Event>,
    ctx: egui::Context,
    mach: Option<Machine>,
    views: BTreeMap<usize, (u32, usize)>,
    disasm: Option<(Option<u32>, usize)>,
    live: bool,
    live_screen: bool,
    interval: Duration,
    last_poll: Instant,
    last_regs: Option<Registers>,
    armed: bool,
    prof_on: bool,
    prof_flag: Option<u32>,
    prof_bucket: u32,
    prof_hist: std::collections::HashMap<u32, u32>,
    prof_total: u32,
    symbols: std::sync::Arc<mega65::symbols::SymbolTable>,
    watches: Vec<(u32, u8)>,
    search: Option<mega65::memsearch::Search>,
    vf011: Option<mega65::vf011::VirtualF011>,
    /// Keys `HoldKeys` has down, and when they were last changed.
    held_keys: Vec<u8>,
    keys_changed: Instant,
}

impl Worker {
    fn new(rx: Receiver<Cmd>, tx: Sender<Event>, ctx: egui::Context) -> Self {
        Self {
            rx,
            tx,
            ctx,
            mach: None,
            views: BTreeMap::new(),
            disasm: None,
            live: true,
            live_screen: false,
            interval: Duration::from_millis(500),
            last_poll: Instant::now(),
            last_regs: None,
            armed: false,
            prof_on: false,
            prof_flag: None,
            prof_bucket: 16,
            prof_hist: Default::default(),
            prof_total: 0,
            symbols: Default::default(),
            watches: Vec::new(),
            search: None,
            vf011: None,
            held_keys: Vec::new(),
            keys_changed: Instant::now(),
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }

    fn log(&self, s: impl Into<String>) {
        self.emit(Event::Log(s.into()));
    }

    /// Handle every command already queued (between profiler or virtual drive
    /// bursts, so a key press and its release don't each wait a burst). False
    /// when the worker should exit.
    fn drain_commands(&mut self, busy: bool) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(Cmd::Shutdown) => return false,
                Ok(cmd) => {
                    // Key presses are frequent and quick; don't flash the UI busy.
                    let busy = busy && !matches!(cmd, Cmd::HoldKeys(_));
                    if busy {
                        self.emit(Event::Busy(true));
                    }
                    if let Err(e) = self.handle(cmd) {
                        self.emit(Event::Error(e.to_string()));
                    }
                    if busy {
                        self.emit(Event::Busy(false));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            }
        }
    }

    fn run(mut self) {
        loop {
            if self.prof_on && self.mach.is_some() {
                // Profiling owns the link: sample for half a second, then look
                // for commands, and repeat.
                // (Not while keys are held, so their release isn't delayed.)
                if self.held_keys.is_empty() {
                    if let Err(e) = self.profile_burst() {
                        self.emit(Event::Error(e.to_string()));
                        self.prof_on = false;
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if !self.drain_commands(false) {
                    break;
                }
                continue;
            }
            if self.vf011.is_some() && self.mach.is_some() {
                // The virtual drive owns the link between commands: answer
                // requests for a while, then look for commands, and repeat.
                if let Err(e) = self.vf011_burst() {
                    self.emit(Event::Error(format!("virtual drive: {e}")));
                    self.vf011 = None;
                    self.emit(Event::Vf011 { serving: None, reads: 0, writes: 0, note: Some(format!("stopped: {e}")) });
                }
                if !self.drain_commands(true) {
                    break;
                }
                if self.poll_due() {
                    if let Err(e) = self.poll() {
                        self.emit(Event::Error(e.to_string()));
                    }
                    self.last_poll = Instant::now();
                }
                continue;
            }
            let wait = if self.live && self.mach.is_some() {
                self.interval.saturating_sub(self.last_poll.elapsed()).max(Duration::from_millis(10))
            } else {
                Duration::from_millis(200)
            };
            match self.rx.recv_timeout(wait) {
                Ok(Cmd::Shutdown) => break,
                Ok(cmd) => {
                    // Key presses are frequent and quick; don't flash the UI busy.
                    let quiet = matches!(cmd, Cmd::HoldKeys(_));
                    if !quiet {
                        self.emit(Event::Busy(true));
                    }
                    if let Err(e) = self.handle(cmd) {
                        self.emit(Event::Error(e.to_string()));
                        // A dead link is the likeliest cause; verify with a sync.
                        if let Some(m) = self.mach.as_mut() {
                            if m.mon.sync().is_err() {
                                self.mach = None;
                                self.emit(Event::Disconnected);
                            }
                        }
                    }
                    if !quiet {
                        self.emit(Event::Busy(false));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.poll_due() {
                if let Err(e) = self.poll() {
                    self.emit(Event::Error(e.to_string()));
                    if self.mach.as_mut().map(|m| m.mon.sync().is_err()).unwrap_or(false) {
                        self.mach = None;
                        self.emit(Event::Disconnected);
                    }
                }
                self.last_poll = Instant::now();
            }
        }
    }

    fn mach(&mut self) -> mega65::Result<&mut Machine> {
        self.mach.as_mut().ok_or_else(|| mega65::Error::Other("not connected".into()))
    }

    fn handle(&mut self, cmd: Cmd) -> mega65::Result<()> {
        match cmd {
            Cmd::Connect { device, baud } => {
                let mon = Monitor::connect(&device, baud)?;
                let mut m = Machine::new(mon);
                let versions = m.versions().ok();
                let xemu = m.mon.xemu;
                let xemu_ext = m.mon.xemu_ext;
                self.mach = Some(m);
                self.last_regs = None;
                self.held_keys.clear();
                self.emit(Event::Connected { device, xemu, xemu_ext, versions });
                self.refresh_all()?;
            }
            Cmd::Disconnect => {
                if !std::mem::take(&mut self.held_keys).is_empty() {
                    if let Some(m) = self.mach.as_mut() {
                        let _ = m.hold_keys(&[], &[]);
                    }
                }
                if self.vf011.take().is_some() {
                    if let Some(m) = self.mach.as_mut() {
                        let _ = mega65::vf011::VirtualF011::stop(m);
                    }
                    self.emit(Event::Vf011 { serving: None, reads: 0, writes: 0, note: None });
                }
                self.mach = None;
                self.emit(Event::Disconnected);
            }
            Cmd::Refresh => self.refresh_all()?,
            Cmd::Stop => {
                self.mach()?.mon.stop_cpu()?;
                self.refresh_all()?;
            }
            Cmd::Go => {
                self.mach()?.mon.start_cpu()?;
                self.armed = false;
                self.refresh_all()?;
            }
            Cmd::GoTo(pc) => {
                let m = self.mach()?;
                m.mon.set_pc(pc)?;
                m.mon.start_cpu()?;
                self.refresh_all()?;
            }
            Cmd::Step(n) => {
                for _ in 0..n {
                    let regs = self.mach()?.mon.step()?;
                    if n <= 8 {
                        self.log(regs.raw.lines().last().unwrap_or("").to_string());
                    }
                }
                self.refresh_all()?;
            }
            Cmd::Frames(n) => {
                let regs = self.mach()?.mon.xemu_frames(n)?;
                self.log(format!("{n} frame{} run; {}", if n == 1 { "" } else { "s" }, regs.raw.lines().last().unwrap_or("")));
                self.refresh_all()?;
            }
            Cmd::SetBreak(pc) => {
                self.mach()?.mon.set_breakpoint(pc)?;
                self.armed = pc.is_some();
                self.log(match pc {
                    Some(p) => format!("breakpoint set at ${p:04X}"),
                    None => "breakpoint cleared".into(),
                });
            }
            Cmd::SetWatch(a) => {
                self.mach()?.mon.set_watchpoint(a)?;
                self.armed = a.is_some();
                self.log(match a {
                    Some(p) => format!("watchpoint set at ${p:07X}"),
                    None => "watchpoint cleared".into(),
                });
            }
            Cmd::SetIrqs(on) => self.mach()?.mon.set_irqs(on)?,
            Cmd::Memory { id, addr, len } => {
                self.views.insert(id, (addr, len));
                self.refresh_view(id)?;
            }
            Cmd::WriteBytes { addr, data } => {
                self.mach()?.mon.set_bytes(addr, &data)?;
                self.refresh_views()?;
            }
            Cmd::Disasm { addr, pages } => {
                self.disasm = Some((addr, pages));
                self.refresh_disasm()?;
            }
            Cmd::Screenshot { pause } => self.screenshot(pause, None)?,
            Cmd::SavePng { path, pause } => self.screenshot(pause, Some(path))?,
            Cmd::LoadPrg { path, run, binary, jump } => {
                let bytes = std::fs::read(&path)?;
                let prg = Prg::parse(&bytes)?;
                let m = self.mach()?;
                let addr = match jump {
                    Some(j) => m.load_and_jump(&prg, Some(j))?,
                    None => m.load_prg(&prg, binary, run)?,
                };
                self.log(format!("loaded {} ({} bytes) at ${addr:04X}{}", path.display(), prg.data.len(), if run { ", RUN" } else { "" }));
                self.refresh_all()?;
            }
            Cmd::LoadBinary { path, addr } => {
                let bytes = std::fs::read(&path)?;
                self.mach()?.inject(addr, &bytes)?;
                self.log(format!("loaded {} ({} bytes) at ${addr:07X}", path.display(), bytes.len()));
                self.refresh_views()?;
            }
            Cmd::SaveMemory { path, start, end, prg, pause } => {
                if pause && end - start > 4096 {
                    self.log(format!("halting the CPU for the {} KB save", (end - start) / 1024));
                }
                let m = self.mach()?;
                let data = if pause { m.save_memory(start, end)? } else { m.mon.read_memory(start, (end - start) as usize)? };
                let mut out = Vec::with_capacity(data.len() + 2);
                if prg {
                    out.extend_from_slice(&(start as u16).to_le_bytes());
                }
                out.extend_from_slice(&data);
                std::fs::write(&path, &out)?;
                self.log(format!("saved ${start:07X}-${end:07X} ({} bytes) to {}", data.len(), path.display()));
            }
            Cmd::TypeText { text, ret, buffer } => {
                let m = self.mach()?;
                if buffer {
                    m.stuff_keybuffer(&format!("{text}{}", if ret { "\r" } else { "" }))?;
                } else {
                    m.type_text(&text, ret)?;
                }
            }
            Cmd::HoldKeys(keys) => {
                if keys == self.held_keys {
                    return Ok(());
                }
                // A quick tap arrives as press and release back to back; keep each
                // state long enough for the keyboard scanner to see it.
                let min = Duration::from_millis(40);
                let since = self.keys_changed.elapsed();
                if since < min {
                    std::thread::sleep(min - since);
                }
                let prev = std::mem::take(&mut self.held_keys);
                self.mach()?.hold_keys(&prev, &keys)?;
                self.held_keys = keys;
                self.keys_changed = Instant::now();
            }
            Cmd::Raw(line) => {
                let out = self.mach()?.mon.raw(&line)?;
                self.log(format!("> {line}\n{out}"));
            }
            Cmd::Reset => {
                self.mach()?.reset()?;
                self.log("reset");
                self.refresh_all()?;
            }
            Cmd::Go64 => {
                self.mach()?.go64()?;
                self.log("switched to C64 mode");
            }
            Cmd::Video { pal } => {
                self.mach()?.set_video(pal)?;
                self.log(if pal { "PAL" } else { "NTSC" });
            }
            Cmd::Profiler { enabled, frame_flag, bucket } => {
                if enabled && !self.prof_on {
                    self.mach()?.mon.sync()?;
                }
                self.prof_on = enabled;
                self.prof_flag = frame_flag;
                if self.prof_bucket != bucket.max(1) {
                    self.prof_hist.clear();
                    self.prof_total = 0;
                }
                self.prof_bucket = bucket.max(1);
                if !enabled {
                    self.refresh_all()?;
                }
            }
            Cmd::FindFrameFlag => {
                let was = self.prof_on;
                self.prof_on = false;
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let m = self.mach()?;
                let r = mega65::probe::find_frame_flags(&mut m.mon, 0x7770200, 0x777d000, 4, 100, 5.0, 130.0, |d, t| {
                    let _ = tx.send(Event::Progress(Some((d, t))));
                    ctx.request_repaint();
                });
                self.emit(Event::Progress(None));
                self.emit(Event::FrameFlagCandidates(r?));
                self.prof_on = was;
            }
            Cmd::Symbols(t) => {
                self.symbols = t;
                self.refresh_disasm()?;
            }
            Cmd::Watches(w) => {
                self.log(format!("watching {} address(es)", w.len()));
                self.watches = w;
                self.refresh_watches()?;
            }
            Cmd::DmaList { addr } => {
                let bytes = self.mach()?.mon.read_memory(addr, 512)?;
                let text = mega65::dma::decode(&bytes, true).describe(addr);
                self.emit(Event::DmaList { addr, text });
            }
            Cmd::SendBasic { source, opts, send } => {
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let m = self.mach()?;
                let r = m.send_basic(&source, &opts, &send, &mut |p| {
                    let _ = tx.send(Event::BasicProgress(p.stage, p.done, p.total));
                    ctx.request_repaint();
                });
                match &r {
                    Ok(rep) => self.log(format!("BASIC: {}", rep.summary())),
                    Err(e) => self.log(format!("BASIC: {e}")),
                }
                self.emit(Event::BasicDone(r.map_err(|e| e.to_string())));
                self.refresh_all()?;
            }
            Cmd::ProfilerReset => {
                self.prof_hist.clear();
                self.prof_total = 0;
            }
            Cmd::SearchStep { filter, start } => {
                if let Some((regions, width)) = start {
                    self.search = Some(mega65::memsearch::Search::new(regions, width));
                }
                let Some(mut search) = self.search.take() else {
                    return Err(mega65::Error::Other("no search in progress: start one first".into()));
                };
                // The profiler would fight the search for the link; pause it.
                let was = self.prof_on;
                self.prof_on = false;
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let r = match self.mach() {
                    Ok(m) => search.step(&mut m.mon, filter, &mut |d, t| {
                        let _ = tx.send(Event::Progress(Some((d, t))));
                        ctx.request_repaint();
                    }),
                    Err(e) => Err(e),
                };
                self.emit(Event::Progress(None));
                self.prof_on = was;
                let count = r?;
                self.log(format!("search: {}", search.history.last().cloned().unwrap_or_default()));
                self.emit(Event::Search {
                    count,
                    hits: search.hits().iter().take(SEARCH_HITS_SHOWN).copied().collect(),
                    width: search.width,
                    history: search.history.clone(),
                });
                self.search = Some(search);
            }
            Cmd::SearchReset => {
                self.search = None;
                self.emit(Event::Search { count: 0, hits: Vec::new(), width: 1, history: Vec::new() });
            }
            Cmd::SetLive { enabled, interval, screen } => {
                self.live = enabled;
                self.interval = interval;
                self.live_screen = screen;
            }
            Cmd::Sd { op, direct, force } => {
                // The profiler would fight the transfer for the link; pause it.
                let was = self.prof_on;
                self.prof_on = false;
                let serving = self.vf011.is_some();
                if serving {
                    // Raw sector data would look like drive requests; the drive waits.
                    self.log("note: the virtual drive cannot answer requests during an SD card operation");
                    if let Some(m) = self.mach.as_mut() {
                        m.mon.capture_vf011 = false;
                    }
                }
                let r = self.sd(&op, direct, force);
                if serving {
                    if let Some(m) = self.mach.as_mut() {
                        m.mon.capture_vf011 = true;
                    }
                }
                self.prof_on = was;
                self.emit(Event::SdProgress(None));
                match r {
                    Ok(res) => {
                        if let Some((path, entries, free, images)) = res.listing {
                            self.emit(Event::SdListing { path, entries, free, images });
                        }
                        if !res.message.is_empty() {
                            self.log(format!("SD: {}", res.message));
                        }
                        self.emit(Event::SdDone(Ok(res.message)));
                    }
                    Err(e) => {
                        self.log(format!("SD: {e}"));
                        self.emit(Event::SdDone(Err(e.to_string())));
                        // A dead link is the likeliest cause of a timeout; verify with a sync.
                        if self.mach.as_mut().map(|m| m.mon.sync().is_err()).unwrap_or(false) {
                            self.mach = None;
                            self.emit(Event::Disconnected);
                        }
                    }
                }
            }
            Cmd::Hyppo => {
                let m = self.mach()?;
                let text = m.hyppo_status()?.text;
                let regs = m.mon.read_memory(0xffd368b, 9)?;
                let image0 = u32::from_le_bytes([regs[1], regs[2], regs[3], regs[4]]);
                let image1 = u32::from_le_bytes([regs[5], regs[6], regs[7], regs[8]]);
                self.emit(Event::Hyppo { text, image0, image1, ctrl: regs[0] });
            }
            Cmd::Fill { start, end, value } => {
                if end <= start {
                    return Err(mega65::Error::Other("fill: end must be after start".into()));
                }
                self.mach()?.mon.fill_memory(start, end, value)?;
                self.log(format!("filled ${start:07X}-${end:07X} with ${value:02X}"));
                self.refresh_views()?;
            }
            Cmd::Vf011(path) => {
                if let Some(vf) = self.vf011.take() {
                    let m = self.mach()?;
                    mega65::vf011::VirtualF011::stop(m)?;
                    self.log(format!("virtual drive stopped after {} reads, {} writes; the MEGA65 restarted with the SD card's drive", vf.reads, vf.writes));
                    self.emit(Event::Vf011 { serving: None, reads: vf.reads, writes: vf.writes, note: None });
                }
                if let Some(p) = path {
                    let vf = mega65::vf011::VirtualF011::open(&p.display().to_string())?;
                    let m = self.mach()?;
                    mega65::vf011::VirtualF011::start(m)?;
                    self.log(format!("restarted the MEGA65 with {} as the internal drive", p.display()));
                    self.emit(Event::Vf011 { serving: Some(p.display().to_string()), reads: 0, writes: 0, note: None });
                    self.vf011 = Some(vf);
                }
            }
            Cmd::Shutdown => {}
        }
        Ok(())
    }

    /// Answer virtual-drive requests for up to 150 ms and report the counts
    /// when anything happened.
    fn vf011_burst(&mut self) -> mega65::Result<()> {
        let Some(mut vf) = self.vf011.take() else { return Ok(()) };
        let m = self.mach()?;
        let served = vf.poll(&mut m.mon, Duration::from_millis(150))?;
        let reset = served.contains(&mega65::vf011::Served::Reset);
        if !served.is_empty() {
            let note = reset.then(|| "the MEGA65 was reset: it needs a power cycle before the virtual drive works again; stopped serving".to_string());
            self.emit(Event::Vf011 { serving: if reset { None } else { Some(vf.path.clone()) }, reads: vf.reads, writes: vf.writes, note });
        }
        if !reset {
            self.vf011 = Some(vf);
        }
        Ok(())
    }

    /// Run one SD operation: open the card (through the helper or directly),
    /// do the work, close it again. The helper stays resident between calls so
    /// the next one starts quickly; `SdOp::Quit` ends it.
    fn sd(&mut self, op: &SdOp, direct: bool, force: bool) -> mega65::Result<SdResult> {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let mut progress = move |done: u64, total: u64| {
            let _ = tx.send(Event::SdProgress(Some((done, total))));
            ctx.request_repaint();
        };
        let m = self.mach()?;
        if direct {
            if matches!(op, SdOp::Quit) {
                return Ok(SdResult { message: String::new(), listing: None });
            }
            if matches!(op, SdOp::Mount { .. }) {
                return Err(mega65::Error::Other("mounting a disk image needs the fast (helper) mode".into()));
            }
            let dev = DirectSd::new(&mut m.mon);
            let mut fs = Fat32::open(dev)?;
            let r = sd_op(&mut fs, op, &mut progress);
            fs.close()?;
            return r;
        }
        let mut dev = HelperSd::start(m, force).map_err(|e| match e {
            mega65::Error::Other(ref s) if s == mega65::sdcard::PROGRAM_LOADED => {
                mega65::Error::Other(format!("{s}; tick \"replace the program in memory\" to go ahead"))
            }
            e => e,
        })?;
        match op {
            SdOp::Quit => {
                dev.quit()?;
                return Ok(SdResult { message: "SD helper stopped; the MEGA65 is back in BASIC".into(), listing: None });
            }
            SdOp::Mount { name } => {
                dev.mount_d81(name)?;
                return Ok(SdResult { message: format!("asked the Hypervisor to mount {name}"), listing: None });
            }
            _ => {}
        }
        let mut fs = Fat32::open(dev)?;
        let r = sd_op(&mut fs, op, &mut progress);
        fs.close()?;
        r
    }

    /// One profiler period: count frame-flag changes for 200 ms (if a flag is
    /// set), sample the PC for 300 ms, then emit a snapshot.
    fn profile_burst(&mut self) -> mega65::Result<()> {
        let bucket = self.prof_bucket;
        let flag = self.prof_flag;
        let m = self.mach()?;
        let mut fps = None;
        if let Some(a) = flag {
            let t0 = Instant::now();
            let (mut changes, mut last) = (0u32, None);
            while t0.elapsed() < Duration::from_millis(200) {
                let v = m.mon.peek_fast(a)?;
                if last.map(|l| l != v).unwrap_or(false) {
                    changes += 1;
                }
                last = Some(v);
            }
            fps = Some(changes as f32 / t0.elapsed().as_secs_f32());
        }
        let t1 = Instant::now();
        let mut pcs: Vec<u32> = Vec::with_capacity(256);
        let mut last_regs = None;
        while t1.elapsed() < Duration::from_millis(300) {
            let r = m.mon.registers_fast()?;
            pcs.push(r.pc as u32 / bucket * bucket);
            last_regs = Some(r);
        }
        let n = pcs.len() as u32;
        let rate = n as f32 / t1.elapsed().as_secs_f32();
        for pc in pcs {
            *self.prof_hist.entry(pc).or_default() += 1;
        }
        self.prof_total += n;
        if let Some(r) = last_regs {
            let stopped = self.mach()?.mon.cpu_stopped;
            self.last_regs = Some(r.clone());
            self.emit(Event::Registers { regs: r, stopped });
        }
        if !self.watches.is_empty() {
            let watches = self.watches.clone();
            let m = self.mach()?;
            let mut vals = Vec::with_capacity(watches.len());
            for (a, w) in watches {
                let lo = m.mon.peek_fast(a)? as u32;
                vals.push(if w >= 2 { lo | (m.mon.peek_fast(a + 1)? as u32) << 8 } else { lo });
            }
            self.emit(Event::Watches(vals));
        }
        let mut buckets: Vec<(u32, u32)> = self.prof_hist.iter().map(|(a, c)| (*a, *c)).collect();
        buckets.sort_by(|a, b| b.1.cmp(&a.1));
        buckets.truncate(64);
        self.emit(Event::Profile { total: self.prof_total, buckets, samples_per_sec: rate, fps });
        Ok(())
    }

    fn refresh_watches(&mut self) -> mega65::Result<()> {
        if self.watches.is_empty() {
            return Ok(());
        }
        let watches = self.watches.clone();
        let m = self.mach()?;
        let mut vals = Vec::with_capacity(watches.len());
        for (a, w) in watches {
            let b = m.mon.read_memory(a, w.max(1) as usize)?;
            vals.push(if w >= 2 { b[0] as u32 | (b[1] as u32) << 8 } else { b[0] as u32 });
        }
        self.emit(Event::Watches(vals));
        Ok(())
    }

    /// Time for a live poll? Not while keys are held: a poll (a screen capture
    /// especially) can take a second, and the key's release would wait behind
    /// it long enough for the MEGA65 to auto-repeat the key.
    fn poll_due(&self) -> bool {
        self.live && self.mach.is_some() && self.held_keys.is_empty() && self.last_poll.elapsed() >= self.interval
    }

    fn poll(&mut self) -> mega65::Result<()> {
        self.refresh_regs()?;
        self.refresh_views()?;
        self.refresh_watches()?;
        if let Some((None, _)) = self.disasm {
            self.refresh_disasm()?;
        }
        if self.live_screen {
            self.screenshot(false, None)?;
        }
        Ok(())
    }

    fn refresh_all(&mut self) -> mega65::Result<()> {
        self.refresh_regs()?;
        self.refresh_views()?;
        self.refresh_disasm()?;
        self.refresh_watches()?;
        Ok(())
    }

    fn refresh_regs(&mut self) -> mega65::Result<()> {
        let armed = self.armed;
        let prev_raw = self.last_regs.as_ref().map(|r| r.raw.clone());
        let m = self.mach()?;
        let regs = m.mon.registers()?;
        // A breakpoint/watchpoint stops the CPU without telling us (the report
        // can be swallowed), so notice when the registers freeze.
        let mut hit = false;
        if armed && !m.mon.cpu_stopped && prev_raw.as_deref() == Some(regs.raw.as_str()) {
            m.mon.cpu_stopped = true;
            hit = true;
        }
        let stopped = m.mon.cpu_stopped;
        if hit {
            self.armed = false;
            self.emit(Event::BreakHit(regs.clone()));
        }
        self.last_regs = Some(regs.clone());
        self.emit(Event::Registers { regs, stopped });
        Ok(())
    }

    fn refresh_views(&mut self) -> mega65::Result<()> {
        let ids: Vec<usize> = self.views.keys().copied().collect();
        for id in ids {
            self.refresh_view(id)?;
        }
        Ok(())
    }

    fn refresh_view(&mut self, id: usize) -> mega65::Result<()> {
        let Some(&(addr, len)) = self.views.get(&id) else { return Ok(()) };
        let data = self.mach()?.mon.read_memory(addr, len)?;
        self.emit(Event::Memory { id, addr, data });
        Ok(())
    }

    fn refresh_disasm(&mut self) -> mega65::Result<()> {
        let Some((addr, pages)) = self.disasm else { return Ok(()) };
        let pc = self.last_regs.as_ref().map(|r| r.pc as u32).unwrap_or(0);
        let m = self.mach()?;
        let start = match addr {
            Some(a) => a,
            None => 0x7770000 | pc,
        };
        // Read a window of memory and disassemble it here, so it works the same
        // on Xemu (no `D` command) and the real monitor.
        let len = 48 * pages.max(1);
        let mem = m.mon.read_memory(start, len)?;
        let wide = start >= 0x10000 && (start & 0xfff0000) != 0x7770000;
        let syms = self.symbols.clone();
        let mut text = String::new();
        for i in mega65::disasm::disassemble(start, &mem) {
            for n in syms.names_at(i.addr & 0xffff) {
                text.push_str(n);
                text.push_str(":\n");
            }
            text.push_str(&i.format_with(wide, Some(&syms)));
            text.push('\n');
        }
        self.emit(Event::Disasm { addr: start, text });
        Ok(())
    }

    fn screenshot(&mut self, pause: bool, png: Option<PathBuf>) -> mega65::Result<()> {
        let m = self.mach()?;
        if m.mon.xemu_ext {
            // Xemu's own frame (exact pixels, ~100 ms); the text still comes from screen RAM.
            let tmp = std::env::temp_dir().join(format!("m65-studio-screen-{}.png", std::process::id()));
            let p = m.mon.xemu_screenshot(&tmp)?;
            let (width, height, rgb) = decode_png_rgb(&p)?;
            if let Some(dst) = &png {
                std::fs::copy(&p, dst)?;
            }
            let _ = std::fs::remove_file(&p);
            let text = VideoState::capture(&mut m.mon)?.to_text();
            if let Some(dst) = png {
                self.log(format!("saved Xemu's frame to {}", dst.display()));
            }
            self.emit(Event::Screen { width, height, rgb, text });
            return Ok(());
        }
        let vs = if pause { m.mon.with_cpu_paused(VideoState::capture)? } else { VideoState::capture(&mut m.mon)? };
        let rgb = vs.render(&mut m.mon)?;
        if vs.flipped && !pause {
            self.log("the program flipped screen buffers during the capture, so parts of the picture come from a frame being drawn; tick \"Pause CPU while capturing\" for a clean frame".to_string());
        }
        if let Some(p) = png {
            let mut f = std::fs::File::create(&p)?;
            let mut enc = png::Encoder::new(&mut f, 720, vs.height() as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().map_err(|e| mega65::Error::Other(e.to_string()))?;
            w.write_image_data(&rgb).map_err(|e| mega65::Error::Other(e.to_string()))?;
            self.log(format!("saved screenshot to {}", p.display()));
        }
        self.emit(Event::Screen { width: 720, height: vs.height(), rgb, text: vs.to_text() });
        Ok(())
    }
}

/// The filesystem side of an SD operation, the same for either block device.
fn sd_op<D: BlockDevice>(fs: &mut Fat32<D>, op: &SdOp, progress: &mut impl FnMut(u64, u64)) -> mega65::Result<SdResult> {
    let listing = |fs: &mut Fat32<D>, path: &str| -> mega65::Result<SdDir> {
        let entries = fs.list(Some(path), false)?;
        let free = fs.df(false).ok();
        let images = entries
            .iter()
            .filter(|e| !e.is_dir() && e.cluster >= 2 && e.name.to_ascii_lowercase().ends_with(".d81"))
            .map(|e| (e.name.clone(), fs.cluster_sector(e.cluster)))
            .collect();
        Ok((path.to_string(), entries, free, images))
    };
    let join = |dir: &str, name: &str| if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") };
    match op {
        SdOp::List { path } => Ok(SdResult { message: String::new(), listing: Some(listing(fs, path)?) }),
        SdOp::Put { local, dir, alloc } => {
            let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.is_empty() {
                return Err(mega65::Error::Other(format!("{}: not a file", local.display())));
            }
            let data = std::fs::read(local)?;
            let remote = join(dir, &name);
            fs.write_file_with(&remote, &data, *alloc, &mut *progress)?;
            let e = fs.lookup(&remote)?.ok_or_else(|| mega65::Error::Other("file missing after upload".into()))?;
            let chain = fs.chain(e.cluster)?;
            let runs = 1 + chain.windows(2).filter(|w| w[1] != w[0] + 1).count();
            let layout = match runs {
                1 => format!("in one piece: {} clusters from {}", chain.len(), chain[0]),
                n => format!("fragmented into {n} pieces"),
            };
            Ok(SdResult { message: format!("uploaded {name} ({} bytes) {layout}", data.len()), listing: Some(listing(fs, dir)?) })
        }
        SdOp::Get { remote, local } => {
            let data = fs.read_file(remote, &mut *progress)?;
            std::fs::write(local, &data)?;
            Ok(SdResult { message: format!("downloaded {remote} ({} bytes) to {}", data.len(), local.display()), listing: None })
        }
        SdOp::Delete { path } => {
            fs.delete(path, true)?;
            let dir = parent_dir(path);
            Ok(SdResult { message: format!("deleted {path}"), listing: Some(listing(fs, &dir)?) })
        }
        SdOp::Mkdir { path } => {
            fs.mkdir(path)?;
            let dir = parent_dir(path);
            Ok(SdResult { message: format!("created {path}"), listing: Some(listing(fs, &dir)?) })
        }
        SdOp::Mount { .. } | SdOp::Quit => unreachable!("handled before the filesystem is opened"),
    }
}

/// `/A/B/C.D81` -> `/A/B`; `/X` -> `/`.
fn parent_dir(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((d, _)) => d.to_string(),
    }
}

/// Read a PNG file as packed 8-bit RGB.
fn decode_png_rgb(path: &std::path::Path) -> mega65::Result<(usize, usize, Vec<u8>)> {
    let other = |e: png::DecodingError| mega65::Error::Other(format!("{}: {e}", path.display()));
    let mut decoder = png::Decoder::new(std::fs::File::open(path)?);
    // Xemu writes palette PNGs for some screens; expand them to 8-bit RGB(A).
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(other)?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(other)?;
    buf.truncate(info.buffer_size());
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf.chunks(4).flat_map(|p| p[..3].to_vec()).collect(),
        other => return Err(mega65::Error::Other(format!("unexpected PNG colour type {other:?}"))),
    };
    Ok((info.width as usize, info.height as usize, rgb))
}
