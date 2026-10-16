//! The MEGA65 serial ("matrix mode") monitor protocol.
//!
//! The monitor is a line based text protocol at 2 Mbit/s by default. Every
//! command is echoed, its output follows, and a `.` prompt ends it. Relevant commands:
//!
//! | cmd | meaning |
//! |-----|---------|
//! | `#<hex>` | no-op used for synchronisation (echoed back) |
//! | `M<addr>` / `m<addr>` | dump 256 / 16 bytes as `:AAAAAAAA:<hex>` lines (28-bit addresses) |
//! | `l<start> <end+1 & $FFFF>` | fast binary load: raw bytes follow the command |
//! | `s<addr> <b> <b> ...` | set bytes |
//! | `f<start> <end+1> <val>` | fill |
//! | `r` | show registers |
//! | `t0` / `t1` / `tc` | run / halt / continuous trace |
//! | `<CR>` (while `t1`) | single step |
//! | `b[<addr>]` | set/clear PC breakpoint |
//! | `w[<addr>]` | set/clear write watchpoint (28-bit) |
//! | `e[<setclr>]` | flag watchpoint |
//! | `g<addr>` | set PC |
//! | `i0` / `i1` | clear / set the CPU I flag |
//! | `d<addr>` / `D<addr>` | disassemble one / 16 instructions |
//! | `z` | CPU history |
//! | `!` | reset the machine |
//! | `+<div>` | change UART divisor |
//!
//! When a breakpoint or watchpoint triggers, the monitor prints a register
//! dump (followed by `!` lines on some cores) without being asked.

use std::time::{Duration, Instant};

use crate::transport::Transport;
use crate::{Error, Result};

/// Largest chunk the `l` command accepts in one go.
const LOAD_CHUNK: usize = 4096;

/// Parsed output of the `r` command.
#[derive(Debug, Clone, Default)]
pub struct Registers {
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub b: u8,
    pub sp: u16,
    pub maph: u16,
    pub mapl: u16,
    pub flags: u8,
    pub flags_str: String,
    pub last_op: String,
    pub hypervisor: bool,
    /// The raw text the monitor printed, header line included.
    pub raw: String,
}

pub struct Monitor {
    t: Box<dyn Transport>,
    rx: Vec<u8>,
    /// Whether the monitor has an RX FIFO (all but very old bitstreams). With
    /// it we can pipeline commands; without it every byte needs pacing.
    pub rx_buffered: bool,
    /// Our belief about whether the CPU is halted (`t1`).
    pub cpu_stopped: bool,
    /// Talking to Xemu's emulated monitor (one command at a time, no `#`).
    pub xemu: bool,
    /// Pace single bytes (old bitstreams without an RX FIFO, serial only).
    pace_writes: bool,
    pub debug: bool,
    /// Any unsolicited output (breakpoint/watch hits) seen while waiting for other things.
    pub events: Vec<String>,
}

impl Monitor {
    pub fn new(t: Box<dyn Transport>) -> Self {
        Self { t, rx: Vec::new(), rx_buffered: true, cpu_stopped: false, xemu: false, pace_writes: false, debug: false, events: Vec::new() }
    }

    /// Open the transport and synchronise with the monitor.
    pub fn connect(device: &str, baud: u32) -> Result<Self> {
        if device.contains("usbserial") || device.contains("ttyUSB") || device.starts_with("COM") {
            // 1 ms FTDI latency instead of 16 ms: every monitor round trip gets faster.
            let _ = crate::transport::set_ftdi_latency(1);
        }
        let t = crate::transport::open(device, baud)?;
        let mut m = Self::new(t);
        if std::env::var_os("M65_DEBUG_SERIAL").is_some() {
            m.debug = true;
        }
        m.sync()?;
        Ok(m)
    }

    pub fn describe(&self) -> String {
        self.t.describe()
    }

    // ---------------------------------------------------------------- raw I/O

    /// Write bytes. Without an RX buffer the monitor drops characters, so pace them.
    pub fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        if self.debug {
            eprintln!(">> {:?}", String::from_utf8_lossy(data));
        }
        if !self.pace_writes {
            self.t.write_all(data)
        } else {
            for b in data {
                self.t.write_all(std::slice::from_ref(b))?;
                std::thread::sleep(Duration::from_micros(2000));
            }
            Ok(())
        }
    }

    /// Pull whatever is pending into the internal buffer.
    fn fill(&mut self, timeout: Duration) -> Result<usize> {
        let mut buf = [0u8; 8192];
        let n = self.t.read_some(&mut buf, timeout)?;
        if n > 0 {
            if self.debug {
                eprintln!("<< {:?}", String::from_utf8_lossy(&buf[..n]));
            }
            self.rx.extend_from_slice(&buf[..n]);
        }
        Ok(n)
    }

    /// Discard pending input, keeping note of any breakpoint reports in it.
    pub fn purge(&mut self) -> Result<()> {
        loop {
            if self.fill(Duration::from_millis(5))? == 0 {
                break;
            }
        }
        let junk = std::mem::take(&mut self.rx);
        self.note_events(&junk);
        Ok(())
    }

    fn note_events(&mut self, bytes: &[u8]) {
        let s = String::from_utf8_lossy(bytes);
        if s.contains("!\r\n") || s.contains("\n!") {
            self.events.push(s.into_owned());
        }
    }

    /// Read until `needle` appears. Returns everything up to and including it;
    /// leaves the remainder buffered.
    pub fn read_until(&mut self, needle: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = find(&self.rx, needle) {
                let end = pos + needle.len();
                let out: Vec<u8> = self.rx.drain(..end).collect();
                return Ok(out);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout(format!("{:?}", String::from_utf8_lossy(needle))));
            }
            self.fill((deadline - now).min(Duration::from_millis(20)))?;
        }
    }

    /// Read whatever arrives within `quiet` of silence, up to `max`.
    pub fn read_quiet(&mut self, quiet: Duration, max: Duration) -> Result<Vec<u8>> {
        let start = Instant::now();
        let mut last = Instant::now();
        while start.elapsed() < max {
            if self.fill(Duration::from_millis(5))? > 0 {
                last = Instant::now();
            } else if last.elapsed() >= quiet {
                break;
            }
        }
        Ok(std::mem::take(&mut self.rx))
    }

    // ------------------------------------------------------------- framing

    /// Synchronise: clear any half typed command, then send `#<token>` until it echoes.
    pub fn sync(&mut self) -> Result<()> {
        // ^U clears the line buffer, '#' makes it a harmless no-op line.
        self.t.write_all(b"\x15#\r")?;
        std::thread::sleep(Duration::from_millis(20));
        self.purge()?;
        for attempt in 0..10 {
            let token = format!("#{:08x}", rand::random::<u32>());
            self.write_raw(format!("{token}\r").as_bytes())?;
            match self.read_until(token.as_bytes(), Duration::from_millis(1000)) {
                Ok(_) => {
                    // Swallow the rest of the line and prompt.
                    let rest = self.read_quiet(Duration::from_millis(3), Duration::from_millis(30)).unwrap_or_default();
                    if find(&rest, b"not implemented").is_some() {
                        // Xemu's monitor: no `#` command, and it only handles one
                        // command per read, so we can't pipeline.
                        self.xemu = true;
                        self.rx_buffered = false;
                    }
                    return Ok(());
                }
                Err(Error::Timeout(_)) => {
                    // Someone may have left an `l` load half finished: feed it zeroes
                    // so it completes, then retry. Also fall back to slow writes.
                    if attempt == 2 {
                        self.rx_buffered = false;
                        self.pace_writes = true;
                    }
                    if attempt >= 3 {
                        let zeros = [0u8; 256];
                        for _ in 0..16 {
                            self.t.write_all(&zeros)?;
                        }
                        self.t.write_all(b"\x15#\r")?;
                    }
                    self.purge()?;
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::Timeout("monitor sync (is the right serial port selected, and is the MEGA65 on?)".into()))
    }

    /// Run a monitor command and return its textual output (echo and prompt removed).
    pub fn command(&mut self, cmd: &str) -> Result<String> {
        self.command_timeout(cmd, Duration::from_secs(3))
    }

    pub fn command_timeout(&mut self, cmd: &str, timeout: Duration) -> Result<String> {
        self.purge()?;
        if !self.rx_buffered {
            // One command at a time: send it and wait for the echo, then the prompt.
            self.write_raw(format!("{cmd}\r").as_bytes())?;
            let echo = if cmd.is_empty() { b"\r".to_vec() } else { cmd.as_bytes().to_vec() };
            self.read_until(&echo, timeout)?;
            let raw = self.read_until(b"\n.", timeout)?;
            let _ = self.read_quiet(Duration::from_millis(2), Duration::from_millis(20));
            let text = String::from_utf8_lossy(&raw[..raw.len() - 2]).replace('\r', "");
            return Ok(text.trim_matches('\n').to_string());
        }
        let token = format!("#{:08x}", rand::random::<u32>());
        // The token command after the real one marks the end of its output.
        self.write_raw(format!("{cmd}\r").as_bytes())?;
        if !self.rx_buffered {
            std::thread::sleep(Duration::from_millis(30));
        }
        self.write_raw(format!("{token}\r").as_bytes())?;
        let raw = self.read_until(token.as_bytes(), timeout)?;
        let _ = self.read_quiet(Duration::from_millis(2), Duration::from_millis(20));
        let mut text = String::from_utf8_lossy(&raw[..raw.len() - token.len()]).into_owned();
        // Drop the echoed command.
        if let Some(p) = text.find(cmd) {
            text.drain(..p + cmd.len());
        }
        let text = text.replace('\r', "");
        // Drop trailing prompt.
        let text = text.trim_end().trim_end_matches('.').trim_matches('\n').to_string();
        Ok(text)
    }

    // --------------------------------------------------------------- CPU

    /// Halt the CPU (`t1`).
    pub fn stop_cpu(&mut self) -> Result<()> {
        self.command("t1")?;
        self.cpu_stopped = true;
        Ok(())
    }

    /// Let the CPU run (`t0`). Also used to resume after a breakpoint.
    pub fn start_cpu(&mut self) -> Result<()> {
        if self.xemu && self.cpu_stopped {
            // Xemu (at least the 2026 "next" builds) crashes with SIGBUS when it
            // leaves trace mode, so refuse rather than kill the emulator.
            return Err(Error::Other(
                "this Xemu build crashes when the CPU is resumed after a halt or breakpoint; restart Xemu instead".into(),
            ));
        }
        self.command("t0")?;
        self.cpu_stopped = false;
        Ok(())
    }

    /// Run `f` with the CPU halted, restoring the previous run state afterwards.
    /// On Xemu (which can't resume) the CPU is left running.
    pub fn with_cpu_paused<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let was_stopped = self.cpu_stopped;
        if !was_stopped && !self.xemu {
            self.stop_cpu()?;
        }
        let r = f(self);
        if !was_stopped && !self.xemu {
            self.start_cpu()?;
        }
        r
    }

    /// Continuous trace mode (`tc`): the monitor prints every instruction.
    pub fn trace_continuous(&mut self) -> Result<()> {
        self.write_raw(b"tc\r")
    }

    /// Execute one instruction (CPU must be stopped) and return the register dump.
    pub fn step(&mut self) -> Result<Registers> {
        if self.xemu {
            return Err(Error::Other("single-stepping crashes this Xemu build's monitor".into()));
        }
        if !self.cpu_stopped {
            self.stop_cpu()?;
        }
        self.purge()?;
        self.write_raw(b"\r")?;
        let out = self.read_quiet(Duration::from_millis(15), Duration::from_millis(500))?;
        let text = String::from_utf8_lossy(&out).replace('\r', "");
        Registers::parse(&text).ok_or_else(|| Error::Protocol(format!("could not parse step output: {text:?}")))
    }

    /// Send a command and return its output as soon as the prompt arrives,
    /// without the sync token or the settle time of [`Self::command`]. Only
    /// for commands with short, well-terminated replies (`r`, `m`).
    pub fn command_fast(&mut self, cmd: &str, timeout: Duration) -> Result<String> {
        self.rx.clear();
        self.write_raw(format!("{cmd}\r").as_bytes())?;
        let raw = self.read_until(b"\n.", timeout)?;
        let text = String::from_utf8_lossy(&raw[..raw.len() - 2]).replace('\r', "");
        Ok(text)
    }

    /// Registers via the fast path (a few ms per call); used by the profiler.
    pub fn registers_fast(&mut self) -> Result<Registers> {
        let text = self.command_fast("r", Duration::from_secs(2))?;
        Registers::parse(&text).ok_or_else(|| Error::Protocol(format!("could not parse registers: {text:?}")))
    }

    /// One byte via the fast path.
    pub fn peek_fast(&mut self, addr: u32) -> Result<u8> {
        let text = self.command_fast(&format!("m{addr:X}"), Duration::from_secs(2))?;
        let want = format!(":{addr:08X}:");
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix(&want) {
                if rest.len() >= 2 {
                    return u8::from_str_radix(&rest[..2], 16).map_err(|_| Error::Protocol(format!("bad hex in {line:?}")));
                }
            }
        }
        Err(Error::Protocol(format!("no data for ${addr:X} in {text:?}")))
    }

    pub fn registers(&mut self) -> Result<Registers> {
        let text = self.command("r")?;
        Registers::parse(&text).ok_or_else(|| Error::Protocol(format!("could not parse registers: {text:?}")))
    }

    /// Set the program counter.
    pub fn set_pc(&mut self, pc: u16) -> Result<()> {
        self.command(&format!("g{pc:x}"))?;
        Ok(())
    }

    /// Set (Some) or clear (None) the PC breakpoint.
    pub fn set_breakpoint(&mut self, pc: Option<u16>) -> Result<()> {
        match pc {
            Some(pc) => self.command(&format!("b{pc:x}"))?,
            None => self.command("b")?,
        };
        Ok(())
    }

    /// Set (Some) or clear (None) the memory write watchpoint (28-bit address).
    pub fn set_watchpoint(&mut self, addr: Option<u32>) -> Result<()> {
        match addr {
            Some(a) => self.command(&format!("w{a:x}"))?,
            None => self.command("w")?,
        };
        Ok(())
    }

    /// Flag watchpoint: trigger when any of `set` flags are set or any of `clear` flags are clear.
    pub fn set_flag_watch(&mut self, setclr: Option<(u8, u8)>) -> Result<()> {
        match setclr {
            Some((s, c)) => self.command(&format!("e{s:02x}{c:02x}"))?,
            None => self.command("e")?,
        };
        Ok(())
    }

    /// Enable (true) or disable (false) IRQs via the CPU I flag.
    pub fn set_irqs(&mut self, enabled: bool) -> Result<()> {
        self.command(if enabled { "i0" } else { "i1" })?;
        Ok(())
    }

    /// Wait for a breakpoint/watchpoint to trigger. The monitor reports the stop
    /// with an unsolicited register dump, but it can arrive before we start
    /// listening, so we also poll `r` and treat a CPU whose registers stop
    /// changing as halted. Returns the registers at the stop.
    pub fn wait_for_break(&mut self, timeout: Option<Duration>) -> Result<Registers> {
        let start = Instant::now();
        let mut last_poll = Instant::now() - Duration::from_secs(1);
        let mut previous: Option<Registers> = None;
        loop {
            if let Some(t) = timeout {
                if start.elapsed() > t {
                    return Err(Error::Timeout("breakpoint".into()));
                }
            }
            self.fill(Duration::from_millis(20))?;
            if last_poll.elapsed() > Duration::from_millis(300) {
                self.purge()?;
                self.write_raw(b"r\r")?;
                last_poll = Instant::now();
                let out = self.read_quiet(Duration::from_millis(10), Duration::from_millis(300))?;
                let text = String::from_utf8_lossy(&out).replace('\r', "");
                if let Some(regs) = Registers::parse(&text) {
                    let same = previous
                        .as_ref()
                        .map(|p| p.pc == regs.pc && p.a == regs.a && p.x == regs.x && p.y == regs.y && p.raw == regs.raw)
                        .unwrap_or(false);
                    if same {
                        self.cpu_stopped = true;
                        return Ok(regs);
                    }
                    previous = Some(regs);
                }
            }
        }
    }

    /// Reset the machine (`!`). The CPU restarts through the Hypervisor.
    pub fn reset(&mut self) -> Result<()> {
        self.write_raw(b"\r!\r")?;
        std::thread::sleep(Duration::from_millis(500));
        self.cpu_stopped = false;
        self.sync()
    }

    /// Monitor's identification text (`?`).
    pub fn help(&mut self) -> Result<String> {
        self.command("?")
    }

    // ------------------------------------------------------------ memory

    /// Read `len` bytes starting at 28-bit `addr`.
    pub fn read_memory(&mut self, addr: u32, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut cur = addr;
        let end = addr as u64 + len as u64;
        while (cur as u64) < end {
            let remaining = (end - cur as u64) as usize;
            let big = remaining > 16;
            let cmd = if big { format!("M{cur:X}") } else { format!("m{cur:X}") };
            let lines = if big { 16 } else { 1 };
            let text = self.command(&cmd)?;
            let mut got = 0;
            for line in text.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix(':') else { continue };
                if rest.len() < 9 + 32 {
                    continue;
                }
                let Ok(line_addr) = u32::from_str_radix(&rest[..8], 16) else { continue };
                if line_addr != cur.wrapping_add(16 * got) {
                    continue;
                }
                let hex = &rest[9..9 + 32];
                for i in 0..16 {
                    if out.len() >= len {
                        break;
                    }
                    let b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                        .map_err(|_| Error::Protocol(format!("bad hex in {line:?}")))?;
                    out.push(b);
                }
                got += 1;
                if got == lines {
                    break;
                }
            }
            if got == 0 {
                return Err(Error::Protocol(format!("no memory data for {cmd}: {text:?}")));
            }
            cur = cur.wrapping_add(16 * got);
        }
        out.truncate(len);
        Ok(out)
    }

    pub fn peek(&mut self, addr: u32) -> Result<u8> {
        Ok(self.read_memory(addr, 1)?[0])
    }

    pub fn poke(&mut self, addr: u32, value: u8) -> Result<()> {
        self.command(&format!("s{addr:x} {value:x}"))?;
        Ok(())
    }

    /// Set a handful of bytes with the `s` command (good for I/O registers,
    /// where the order and width of writes matter).
    pub fn set_bytes(&mut self, addr: u32, values: &[u8]) -> Result<()> {
        for (i, chunk) in values.chunks(16).enumerate() {
            let mut cmd = format!("s{:x}", addr + i as u32 * 16);
            for v in chunk {
                cmd.push_str(&format!(" {v:x}"));
            }
            self.command(&cmd)?;
        }
        Ok(())
    }

    /// Write a block with the fast binary `l` command. The CPU is halted during the
    /// transfer (the monitor can't keep up with 2 Mbit/s otherwise) and restored after.
    pub fn write_memory(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        if data.len() <= 8 || self.xemu {
            // Xemu's monitor has no `l`; `s` with 16 bytes per line is what's left.
            return self.set_bytes(addr, data);
        }
        // The real monitor can't keep up at 2 Mbit/s with the CPU running; Xemu
        // doesn't care, and can't resume from a halt anyway.
        let was_stopped = self.cpu_stopped || self.xemu;
        if !was_stopped {
            self.stop_cpu()?;
        }
        let mut off = 0usize;
        while off < data.len() {
            let a = addr + off as u32;
            // Stay inside one 64KB bank, and never end exactly on the bank boundary
            // (the end address is given as 16 bits).
            let room = 0xffff - (a & 0xffff) as usize;
            let n = (data.len() - off).min(LOAD_CHUNK).min(room.max(1));
            let cmd = format!("l{:x} {:x}", a, (a + n as u32) & 0xffff);
            self.purge()?;
            self.write_raw(format!("{cmd}\r").as_bytes())?;
            if !self.rx_buffered {
                std::thread::sleep(Duration::from_millis(2));
            }
            if self.xemu {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.t.write_all(&data[off..off + n])?;
            self.read_until(cmd.as_bytes(), Duration::from_secs(3))?;
            // Prompt after the data is consumed.
            self.read_until(b".", Duration::from_secs(3))?;
            off += n;
        }
        if !was_stopped {
            self.start_cpu()?;
        }
        Ok(())
    }

    /// Fill `[start, end)` with `value`.
    pub fn fill_memory(&mut self, start: u32, end: u32, value: u8) -> Result<()> {
        self.command_timeout(&format!("f{start:x} {end:x} {value:x}"), Duration::from_secs(20))?;
        Ok(())
    }

    /// Disassemble `count` instructions at a 28-bit address (prefix `777` for the CPU's view).
    pub fn disassemble(&mut self, addr: u32, sixteen: bool) -> Result<String> {
        self.command(&format!("{}{addr:x}", if sixteen { 'D' } else { 'd' }))
    }

    /// CPU history (`z`), if the core supports it.
    pub fn history(&mut self) -> Result<String> {
        self.command_timeout("z", Duration::from_secs(5))
    }

    /// Send a raw command line and return its output.
    pub fn raw(&mut self, line: &str) -> Result<String> {
        self.command_timeout(line, Duration::from_secs(10))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

impl Registers {
    /// Parse the monitor's register display. It is a header line naming the
    /// columns followed by a value line, e.g.
    ///
    /// ```text
    /// PC   A  X  Y  Z  B  SP   MAPH MAPL LAST-OP In     P  P-FLAGS   RGP uS IO ws h RECA8LHC
    /// ,0777E1A3 00 00 00 00 00 01F6 0000 0000 A5 FD    00 21 ..E....C ...
    /// ```
    ///
    /// The value line starts with `,` and the PC is preceded by `0777` (or `077`).
    pub fn parse(text: &str) -> Option<Self> {
        let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
        // Use the last register block in the text: header, value line, then an
        // optional ",0777PPPP  <bytes> <disassembly>" line for the last op.
        let hi = lines.iter().rposition(|l| l.trim_start().starts_with("PC ") && l.contains("SP"))?;
        let header = lines[hi];
        let value = *lines.get(hi + 1)?;
        let disasm = lines.get(hi + 2).filter(|l| l.starts_with(',')).map(|l| l.to_string());
        let mut r = Registers { raw: format!("{header}\n{value}{}", disasm.as_ref().map(|d| format!("\n{d}")).unwrap_or_default()), ..Default::default() };

        let names: Vec<&str> = header.split_whitespace().collect();
        let vals: Vec<&str> = value.split_whitespace().collect();
        if vals.len() < 8 {
            return None;
        }
        let col = |name: &str| names.iter().position(|n| *n == name);
        let h8 = |i: Option<usize>| i.and_then(|i| vals.get(i)).and_then(|v| u8::from_str_radix(v, 16).ok()).unwrap_or(0);
        let h16 = |i: Option<usize>| i.and_then(|i| vals.get(i)).and_then(|v| u16::from_str_radix(v, 16).ok()).unwrap_or(0);
        r.pc = h16(col("PC"));
        r.a = h8(col("A"));
        r.x = h8(col("X"));
        r.y = h8(col("Y"));
        r.z = h8(col("Z"));
        r.b = h8(col("B"));
        r.sp = h16(col("SP"));
        r.maph = h16(col("MAPH"));
        r.mapl = h16(col("MAPL"));
        // LAST-OP is variable width (1-3 bytes shown as one hex string, or blank),
        // which shifts later columns; so locate the flags by shape instead.
        r.last_op = disasm.clone().unwrap_or_default();
        if let Some(f) = vals.iter().find(|v| v.len() == 8 && v.chars().all(|c| "NVEBDIZC.-".contains(c))) {
            r.flags_str = f.to_string();
            for (i, c) in f.chars().enumerate() {
                if c != '.' && c != '-' {
                    r.flags |= 0x80 >> i;
                }
            }
            // "P" (hex flags) is the column right before P-FLAGS.
            if let Some(pi) = vals.iter().position(|v| v == f) {
                if pi >= 1 {
                    r.flags = u8::from_str_radix(vals[pi - 1], 16).unwrap_or(r.flags);
                }
                // After P-FLAGS: RGP, uS, IO, ws, h.
                r.hypervisor = vals.get(pi + 5).map(|v| *v == "H").unwrap_or(false);
            }
        }
        Some(r)
    }
}

impl std::fmt::Display for Registers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PC=${:04X} A=${:02X} X=${:02X} Y=${:02X} Z=${:02X} B=${:02X} SP=${:04X} P={}{}",
            self.pc,
            self.a,
            self.x,
            self.y,
            self.z,
            self.b,
            self.sp,
            if self.flags_str.is_empty() { format!("${:02X}", self.flags) } else { self.flags_str.clone() },
            if self.hypervisor { " [HYPERVISOR]" } else { "" }
        )
    }
}
