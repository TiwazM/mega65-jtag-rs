//! The hardware side of the GUI: one worker thread owns the `Machine` and runs
//! every monitor exchange, so the UI never blocks on the serial link and two
//! panels can never interleave commands. The UI sends [`Cmd`]s and receives
//! [`Event`]s over channels.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mega65::machine::{Machine, Prg, Versions};
use mega65::monitor::{Monitor, Registers};
use mega65::screen::VideoState;

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
    SaveMemory { path: PathBuf, start: u32, end: u32, prg: bool },
    TypeText { text: String, ret: bool, buffer: bool },
    Raw(String),
    Reset,
    Go64,
    Video { pal: bool },
    /// Live polling of registers/memory/disassembly (and optionally the screen).
    SetLive { enabled: bool, interval: Duration, screen: bool },
    Shutdown,
}

#[derive(Debug)]
pub enum Event {
    Connected { device: String, xemu: bool, versions: Option<Versions> },
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
}

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
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }

    fn log(&self, s: impl Into<String>) {
        self.emit(Event::Log(s.into()));
    }

    fn run(mut self) {
        loop {
            let wait = if self.live && self.mach.is_some() {
                self.interval.saturating_sub(self.last_poll.elapsed()).max(Duration::from_millis(10))
            } else {
                Duration::from_millis(200)
            };
            match self.rx.recv_timeout(wait) {
                Ok(Cmd::Shutdown) => break,
                Ok(cmd) => {
                    self.emit(Event::Busy(true));
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
                    self.emit(Event::Busy(false));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.live && self.mach.is_some() && self.last_poll.elapsed() >= self.interval {
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
                self.mach = Some(m);
                self.last_regs = None;
                self.emit(Event::Connected { device, xemu, versions });
                self.refresh_all()?;
            }
            Cmd::Disconnect => {
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
            Cmd::SaveMemory { path, start, end, prg } => {
                let data = self.mach()?.save_memory(start, end)?;
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
            Cmd::SetLive { enabled, interval, screen } => {
                self.live = enabled;
                self.interval = interval;
                self.live_screen = screen;
            }
            Cmd::Shutdown => {}
        }
        Ok(())
    }

    fn poll(&mut self) -> mega65::Result<()> {
        self.refresh_regs()?;
        self.refresh_views()?;
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
        let text: String = mega65::disasm::disassemble(start, &mem).iter().map(|i| i.format(wide) + "\n").collect();
        self.emit(Event::Disasm { addr: start, text });
        Ok(())
    }

    fn screenshot(&mut self, pause: bool, png: Option<PathBuf>) -> mega65::Result<()> {
        let m = self.mach()?;
        let vs = if pause { m.mon.with_cpu_paused(VideoState::capture)? } else { VideoState::capture(&mut m.mon)? };
        let rgb = vs.render(&mut m.mon)?;
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
