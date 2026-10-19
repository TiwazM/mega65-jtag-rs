//! MEGA65-specific operations built on the serial monitor: mode detection,
//! program loading, keyboard injection, Hypervisor state, video mode.

use std::time::Duration;

use crate::monitor::Monitor;
use crate::{Error, Result};

// Frequently used 28-bit addresses.
pub const VIC_BASE: u32 = 0xffd3000;
pub const HYPERVISOR_BASE: u32 = 0xffd3640;
pub const HYPPO_ROM: u32 = 0xfff8000;
pub const COLOUR_RAM: u32 = 0xff80000;
pub const CHAR_ROM: u32 = 0xff7e000;
pub const BASIC_ROM: u32 = 0x20000;
pub const FLASH_MENU: u32 = 0x50000;
/// Virtual keyboard registers: $D615..$D617 hold up to three pressed matrix keys.
pub const VKEY_REG: u32 = 0xffd3615;
/// Matrix scan codes of the shift keys. Cursor up/left are right shift + down/right.
pub const KEY_LSHIFT: u8 = 0x0f;
pub const KEY_RSHIFT: u8 = 0x34;
/// Buffered UART ASCII input register.
pub const UART_ASCII_IN: u32 = 0xffd30e3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    C64,
    C65,
    /// OpenROMs always behave like C64 mode for loading.
    OpenRom,
    Unknown,
}

impl Mode {
    pub fn is_c64_like(self) -> bool {
        matches!(self, Mode::C64 | Mode::OpenRom)
    }
}

/// Which keyboard matrix position a key lives at (7 rows x 8 columns, $00-$3F).
/// `None` for keys that aren't on the matrix.
pub fn matrix_code(key: u8) -> Option<(u8, bool)> {
    // Returns (code, needs_shift).
    let shifted = |c: u8| Some((c, true));
    let plain = |c: u8| Some((c, false));
    match key {
        b'!' => shifted(0x38),
        b'"' => shifted(0x3b),
        b'#' => shifted(0x08),
        b'$' => shifted(0x0b),
        b'%' => shifted(0x10),
        b'&' => shifted(0x13),
        b'\'' => shifted(0x18),
        b'(' => shifted(0x1b),
        b')' => shifted(0x20),
        b'?' => shifted(0x37),
        b'<' => shifted(0x2f),
        b'>' => shifted(0x2c),
        b'[' => shifted(0x2d),
        b']' => shifted(0x32),
        b'A'..=b'Z' => Some((matrix_code(key.to_ascii_lowercase())?.0, true)),
        0x03 => plain(0x3f), // RUN/STOP
        0x1d => plain(0x02), // right
        0x9d => shifted(0x02), // left
        0x11 => plain(0x07), // down
        0x91 => shifted(0x07), // up
        0x0d | b'\n' => plain(0x01),
        0x14 | 0x7f | 0x08 => plain(0x00), // INST/DEL
        0xf1 => plain(0x04),
        0xf3 => plain(0x05),
        0xf5 => plain(0x06),
        0xf7 => plain(0x03),
        0x13 => plain(0x33), // HOME
        0x1b => plain(0x3f), // ESC -> RUN/STOP too
        b'3' => plain(0x08),
        b'w' => plain(0x09),
        b'a' => plain(0x0a),
        b'4' => plain(0x0b),
        b'z' => plain(0x0c),
        b's' => plain(0x0d),
        b'e' => plain(0x0e),
        b'5' => plain(0x10),
        b'r' => plain(0x11),
        b'd' => plain(0x12),
        b'6' => plain(0x13),
        b'c' => plain(0x14),
        b'f' => plain(0x15),
        b't' => plain(0x16),
        b'x' => plain(0x17),
        b'7' => plain(0x18),
        b'y' => plain(0x19),
        b'g' => plain(0x1a),
        b'8' => plain(0x1b),
        b'b' => plain(0x1c),
        b'h' => plain(0x1d),
        b'u' => plain(0x1e),
        b'v' => plain(0x1f),
        b'9' => plain(0x20),
        b'i' => plain(0x21),
        b'j' => plain(0x22),
        b'0' => plain(0x23),
        b'm' => plain(0x24),
        b'k' => plain(0x25),
        b'o' => plain(0x26),
        b'n' => plain(0x27),
        b'+' => plain(0x28),
        b'p' => plain(0x29),
        b'l' => plain(0x2a),
        b'-' => plain(0x2b),
        b'.' => plain(0x2c),
        b':' => plain(0x2d),
        b'@' => plain(0x2e),
        b',' => plain(0x2f),
        b'}' => plain(0x30), // pound
        b'*' => plain(0x31),
        b';' => plain(0x32),
        b'=' => plain(0x35),
        b'^' => plain(0x36), // up arrow
        b'/' => plain(0x37),
        b'1' => plain(0x38),
        b'_' => plain(0x39), // left arrow
        b'2' => plain(0x3b),
        b' ' => plain(0x3c),
        b'q' => plain(0x3e),
        _ => None,
    }
}

/// Key codes recognised in text given to [`Machine::type_text`]: `~M` RETURN, `~C` RUN/STOP,
/// `~D/~U/~L/~R` cursor, `~H` HOME, `~T` INST/DEL, `~1 ~3 ~5 ~7` F-keys, `~z`/`~Z` sleep 1/2 s.
fn escape_key(c: u8) -> Option<u8> {
    Some(match c {
        b'M' => 0x0d,
        b'C' => 0x03,
        b'D' => 0x11,
        b'U' => 0x91,
        b'L' => 0x9d,
        b'R' => 0x1d,
        b'H' => 0x13,
        b'T' => 0x14,
        b'1' => 0xf1,
        b'3' => 0xf3,
        b'5' => 0xf5,
        b'7' => 0xf7,
        _ => return None,
    })
}

/// A PRG file: two byte little-endian load address followed by data.
pub struct Prg {
    pub load_addr: u16,
    pub data: Vec<u8>,
}

impl Prg {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2 {
            return Err(Error::Other("PRG file too short".into()));
        }
        Ok(Prg { load_addr: u16::from_le_bytes([bytes[0], bytes[1]]), data: bytes[2..].to_vec() })
    }
}

/// Snapshot of the Hypervisor's work area, as `m65 --hyppostatus` prints it.
pub struct HyppoStatus {
    pub text: String,
}

pub struct Machine {
    pub mon: Monitor,
    pub mode: Mode,
}

impl Machine {
    pub fn new(mon: Monitor) -> Self {
        Self { mon, mode: Mode::Unknown }
    }

    // ---------------------------------------------------------- detection

    /// Is the CPU currently in Hypervisor mode?
    pub fn in_hypervisor(&mut self) -> Result<bool> {
        Ok(self.mon.registers()?.hypervisor)
    }

    /// Work out whether we're at a C64 or C65 BASIC prompt. Waits for the ROM to settle
    /// (up to `timeout`), as m65 does.
    pub fn detect_mode(&mut self, timeout: Duration) -> Result<Mode> {
        let start = std::time::Instant::now();
        // OpenROMs sign themselves at $20010.
        let sig = self.mon.read_memory(0x20010, 16)?;
        if sig[0] == b'V' || sig[0] == b'O' {
            let digits: String = sig[1..9].iter().map(|&b| b as char).collect();
            if let Ok(d) = digits.trim().parse::<u32>() {
                if d > 2_000_000 {
                    self.mode = Mode::OpenRom;
                    return Ok(self.mode);
                }
            }
        }
        while start.elapsed() < timeout {
            if self.in_hypervisor()? {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            let d030 = self.mon.peek(VIC_BASE + 0x30)?;
            if d030 & 1 != 0 {
                // ROM still being set up by Hyppo.
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let d054 = self.mon.peek(VIC_BASE + 0x54)?;
            if d054 & 7 != 0 {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            if d030 == 0x64 {
                self.mode = Mode::C65;
                return Ok(self.mode);
            }
            let scr = self.mon.read_memory(VIC_BASE + 0x60, 3)?;
            if scr[1] == 0x04 {
                let port01 = self.mon.peek(0x7770001)?;
                if port01 & 0xf == 0x07 {
                    self.mode = Mode::C64;
                    return Ok(self.mode);
                }
            }
            if scr[1] == 0x08 {
                self.mode = Mode::C65;
                return Ok(self.mode);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.mode = Mode::Unknown;
        Ok(self.mode)
    }

    // ----------------------------------------------------------- keyboard

    /// Push text into the KERNAL keyboard buffer (works at a BASIC prompt, fast).
    pub fn stuff_keybuffer(&mut self, text: &str) -> Result<()> {
        if self.mode == Mode::Unknown {
            self.detect_mode(Duration::from_secs(5))?;
        }
        let (buf, len) = if self.mode == Mode::C65 { (0x2b0u32, 0xd0u32) } else { (0x277, 0xc6) };
        let bytes = crate::petscii::ascii_to_keybuffer(text);
        self.mon.poke(len, 0)?;
        std::thread::sleep(Duration::from_millis(25));
        // Only 9 of the 10 buffer slots: using all ten garbles the first call.
        for chunk in bytes.chunks(9) {
            self.mon.set_bytes(buf, chunk)?;
            self.mon.poke(len, chunk.len() as u8)?;
            std::thread::sleep(Duration::from_millis(if chunk.len() == 9 { 60 } else { 25 }));
        }
        Ok(())
    }

    /// Press one key through the virtual keyboard (works anywhere, including in games).
    pub fn type_key(&mut self, key: u8) -> Result<()> {
        // Xemu turns a shifted letter into a graphics character, so letters go
        // unshifted there (BASIC accepts either case).
        let key = if self.mon.xemu && key.is_ascii_uppercase() { key.to_ascii_lowercase() } else { key };
        let Some((code, shift)) = matrix_code(key) else { return Ok(()) };
        let gap = Duration::from_millis(20);
        if shift {
            // The hardware scanner wants shift set in a separate write first.
            self.mon.set_bytes(VKEY_REG, &[0x0f, 0x7f])?;
            std::thread::sleep(gap);
            self.mon.set_bytes(VKEY_REG, &[0x0f, code])?;
        } else {
            self.mon.set_bytes(VKEY_REG, &[code, 0x7f])?;
        }
        std::thread::sleep(gap);
        self.mon.set_bytes(VKEY_REG, &[0x7f, 0x7f, 0x7f])?;
        std::thread::sleep(gap);
        Ok(())
    }

    /// The VIC-IV's CRT scanline emulation ($D054 bit 5, PALEMU), as on HDMI/VGA.
    pub fn scanlines(&mut self) -> Result<bool> {
        Ok(self.mon.peek(VIC_BASE + 0x54)? & 0x20 != 0)
    }

    /// Switch the CRT scanline emulation on or off. Lasts until a reset (which
    /// restores the configured setting) or a program rewrites $D054.
    pub fn set_scanlines(&mut self, on: bool) -> Result<()> {
        let v = self.mon.peek(VIC_BASE + 0x54)?;
        self.mon.set_bytes(VIC_BASE + 0x54, &[if on { v | 0x20 } else { v & !0x20 }])
    }

    /// Hold exactly these matrix keys down (scan codes; at most three), releasing any
    /// others, until the next call. `prev` is what the last call held: a newly pressed
    /// shift is written on its own first, as the scanner needs.
    pub fn hold_keys(&mut self, prev: &[u8], keys: &[u8]) -> Result<()> {
        let is_shift = |k: &u8| *k == KEY_LSHIFT || *k == KEY_RSHIFT;
        let regs = |it: &mut dyn Iterator<Item = &u8>| {
            let mut r = [0x7fu8; 3];
            for (slot, &k) in r.iter_mut().zip(it) {
                *slot = k;
            }
            r
        };
        let new_shift = keys.iter().any(|k| is_shift(k) && !prev.contains(k));
        let new_other = keys.iter().any(|k| !is_shift(k) && !prev.contains(k));
        if new_shift && new_other {
            self.mon.set_bytes(VKEY_REG, &regs(&mut keys.iter().filter(|k| is_shift(k))))?;
            std::thread::sleep(Duration::from_millis(20));
        }
        self.mon.set_bytes(VKEY_REG, &regs(&mut keys.iter()))
    }

    /// Type a string via the virtual keyboard. See [`escape_key`] for `~` sequences.
    pub fn type_text(&mut self, text: &str, add_return: bool) -> Result<()> {
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'~' && i + 1 < b.len() {
                match b[i + 1] {
                    b'z' => std::thread::sleep(Duration::from_secs(1)),
                    b'Z' => std::thread::sleep(Duration::from_secs(2)),
                    c => {
                        if let Some(k) = escape_key(c) {
                            self.type_key(k)?;
                        }
                    }
                }
                i += 2;
            } else {
                self.type_key(b[i])?;
                i += 1;
            }
        }
        if add_return {
            self.type_key(0x0d)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ loading

    /// Load a PRG into memory the way LOAD would, fixing up BASIC's end-of-program
    /// pointer, then optionally RUN it. With `honour_addr` false the load address is
    /// forced to the current mode's BASIC start.
    pub fn load_prg(&mut self, prg: &Prg, honour_addr: bool, run: bool) -> Result<u16> {
        self.detect_mode(Duration::from_secs(10))?;
        let load_addr = if honour_addr {
            prg.load_addr
        } else if self.mode.is_c64_like() {
            0x0801
        } else {
            0x2001
        };
        let end = load_addr as u32 + prg.data.len() as u32;
        if end > 0x10000 {
            return Err(Error::Other(format!("program of {} bytes at ${load_addr:04X} overflows 64K", prg.data.len())));
        }
        let _ = self.stuff_keybuffer(&format!("REM M65RS LOAD AT ${load_addr:04X}:\r"));
        std::thread::sleep(Duration::from_millis(20));
        let halt = !self.mon.xemu;
        if halt {
            self.mon.stop_cpu()?;
        }
        self.mon.write_memory(load_addr as u32, &prg.data)?;
        if !honour_addr || load_addr == 0x0801 || load_addr == 0x2001 {
            let ptr = if self.mode == Mode::C65 { 0x82u32 } else { 0x2d };
            self.mon.set_bytes(ptr, &(end as u16).to_le_bytes())?;
        }
        if halt {
            self.mon.start_cpu()?;
        }
        if run {
            std::thread::sleep(Duration::from_millis(50));
            self.stuff_keybuffer("RUN:\r")?;
        }
        Ok(load_addr)
    }

    /// Load a PRG and jump straight to it (`g<addr>`) rather than RUN, for machine
    /// code with its own entry point. Returns the entry used.
    pub fn load_and_jump(&mut self, prg: &Prg, entry: Option<u16>) -> Result<u16> {
        self.mon.stop_cpu()?;
        self.mon.write_memory(prg.load_addr as u32, &prg.data)?;
        let e = entry.unwrap_or(prg.load_addr);
        self.mon.set_pc(e)?;
        self.mon.start_cpu()?;
        Ok(e)
    }

    /// Write a raw binary at a 28-bit address, lifting ROM write protection if needed.
    pub fn inject(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let rom = (0x20000..0x40000).contains(&addr) || addr >= 0xff00000 && addr < 0xfffffff;
        if rom {
            self.enter_hypervisor()?;
            let p = self.mon.peek(HYPERVISOR_BASE + 0x3d)?;
            self.mon.poke(HYPERVISOR_BASE + 0x3d, p & !4)?;
            let r = self.mon.write_memory(addr, data);
            let p = self.mon.peek(HYPERVISOR_BASE + 0x3d)?;
            self.mon.poke(HYPERVISOR_BASE + 0x3d, p | 4)?;
            self.leave_hypervisor()?;
            r
        } else {
            self.mon.write_memory(addr, data)
        }
    }

    /// Save a memory range to a file. With `basic` the current BASIC program is
    /// saved as a PRG (with load address header).
    pub fn save_memory(&mut self, start: u32, end: u32) -> Result<Vec<u8>> {
        self.mon.with_cpu_paused(|m| m.read_memory(start, (end - start) as usize))
    }

    /// Current BASIC program range (start, end) for the detected mode.
    pub fn basic_range(&mut self) -> Result<(u16, u16)> {
        self.detect_mode(Duration::from_secs(5))?;
        // BASIC 65 programs always start at $2001; $82 holds the end.
        if self.mode == Mode::C65 {
            let e = self.mon.read_memory(0x82, 2)?;
            return Ok((0x2001, u16::from_le_bytes([e[0], e[1]])));
        }
        let s = self.mon.read_memory(0x2b, 2)?;
        let e = self.mon.read_memory(0x2d, 2)?;
        Ok((u16::from_le_bytes([s[0], s[1]]), u16::from_le_bytes([e[0], e[1]])))
    }

    // --------------------------------------------------------- hypervisor

    /// Trap into the Hypervisor (write to $D67E) with the CPU halted.
    pub fn enter_hypervisor(&mut self) -> Result<()> {
        self.mon.sync()?;
        self.mon.stop_cpu()?;
        self.mon.command("sffd367e 0")?;
        self.mon.command("")?;
        Ok(())
    }

    /// Return from the Hypervisor ($D67F) and resume.
    pub fn leave_hypervisor(&mut self) -> Result<()> {
        self.mon.sync()?;
        self.mon.command("sffd367f 0")?;
        self.mon.sync()?;
        self.mon.start_cpu()
    }

    /// Decode the Hypervisor's DOS work area and process descriptor.
    pub fn hyppo_status(&mut self) -> Result<HyppoStatus> {
        let disk = self.mon.read_memory(0xfffbb00, 0x100)?;
        let sys = self.mon.read_memory(0xfffbbc0, 0x40)?;
        let h = self.mon.read_memory(0xfffbc00, 0x100)?;
        let pd = self.mon.read_memory(0xfffbd00, 0x100)?;
        let u32at = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let cstr = |b: &[u8]| -> String {
            b.iter().take_while(|&&c| c != 0).map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' }).collect()
        };
        let mut t = String::new();
        t.push_str("HYPPO status:\n");
        t.push_str(&format!("Disk count = ${:02x}\n", h[1]));
        t.push_str(&format!("Default Disk = ${:02x}\n", h[2]));
        t.push_str(&format!("Current Disk = ${:02x}\n", h[3]));
        t.push_str(&format!("Cluster of current directory = ${:08x}\n", u32at(&h, 5)));
        t.push_str(&format!("opendir_cluster = ${:08x}  sector = ${:02x}  entry = ${:02x}\n", u32at(&h, 9), h[0xd], h[0xe]));
        t.push_str("dirent:\n");
        t.push_str(&format!("  Filename = '{}' (len ${:02x})\n", cstr(&h[0xf..0x4f]), h[0x4f]));
        t.push_str(&format!("  Short name = '{}'\n", cstr(&h[0x50..0x5d])));
        t.push_str(&format!("  Start cluster = ${:08x}  length = ${:08x}  attrib = ${:02x}\n", u32at(&h, 0x5d), u32at(&h, 0x61), h[0x65]));
        t.push_str(&format!("Requested filename = '{}' (len ${:02x})\n", cstr(&h[0x67..0xa9]), h[0x66]));
        t.push_str(&format!("sectorsread = ${:02x}{:02x}  bytes_remaining = ${:08x}\n", h[0xaa], h[0xa9], u32at(&h, 0xab)));
        t.push_str(&format!("current sector = ${:08x}  cluster = ${:08x}  sector-in-cluster = ${:02x}\n", u32at(&h, 0xaf), u32at(&h, 0xb3), h[0xb7]));
        for fd in 0..4 {
            let o = 0xb8 + fd * 0x10;
            t.push_str(&format!(
                "File descriptor #{fd}: disk=${:02x} mode=${:02x} start=${:08x} cur=${:08x} sec=${:02x} ofs=${:02x}{:02x}\n",
                h[o], h[o + 1], u32at(&h, o + 2), u32at(&h, o + 6), h[o + 10], h[o + 12], h[o + 11]
            ));
        }
        t.push_str("Disk Table:\n  Disk# Start     Size       FS FAT_Len   SysLen Rsv RootDir Clusters  CSz #FATs Cluster0\n");
        for d in 0..6 {
            let b = &disk[d * 32..d * 32 + 32];
            t.push_str(&format!(
                "   #{d}   ${:08x} ${:08x}  {:02x} ${:08x} ${:04x}  {:02x}  ${:04x}   ${:08x} {:02x}  {:02x}    ${:08x}\n",
                u32at(b, 0), u32at(b, 4), b[8], u32at(b, 9), u32at(b, 0xd) & 0xffff, b[0xf],
                u32at(b, 0x10) & 0xffff, u32at(b, 0x12), b[0x16], b[0x17], u32at(b, 0x18)
            ));
        }
        t.push_str(&format!("Current fd = ${:02x} ofs = ${:02x}  DOS error = ${:02x}  SYSPART error = ${:02x} present = ${:02x}\n", h[0xf4], h[0xf5], h[0xf6], h[0xf7], h[0xf8]));
        t.push_str(&format!("SYSPART start = ${:08x}\n", u32at(&sys, 0)));
        t.push_str(&format!("Process ID: ${:02x}  name: {}\n", pd[0], cstr(&pd[1..17])));
        Ok(HyppoStatus { text: t })
    }

    /// Mount a disk image on the SD card as drive 8 by typing BASIC 65's
    /// commands, so the machine stays in its native mode and ends at the
    /// READY prompt with the image attached: the MEGA65 is reset (unless it
    /// is already at the prompt, which also puts the SD card's working
    /// directory back at the root), then `CHDIR "folder",U12` for each folder
    /// of `path` and `MOUNT "name"`. A running program is lost. The working
    /// directory is left in the image's folder on purpose: the manual warns
    /// that the ROM un-mounts an image from a subfolder when the directory
    /// changes again, and a reset puts it back at the root anyway.
    /// Returns the F011 image register ($D68C) afterwards, which holds the
    /// image's first sector when the mount took.
    pub fn mount_image(&mut self, path: &str) -> Result<u32> {
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let name = parts.pop().ok_or_else(|| Error::Other("mount: empty path".into()))?;
        // Reset unless at the prompt with nothing typed since boot; a reset is
        // the only way back to the root directory we can be sure of.
        if !self.at_basic65_prompt()? || !parts.is_empty() {
            self.reset_to_prompt()?;
        }
        self.mode = Mode::C65;
        for dir in parts {
            self.type_basic65_line(&format!("chdir \"{}\",u12", dir.to_ascii_lowercase()))?;
        }
        self.type_basic65_line(&format!("mount \"{}\"", name.to_ascii_lowercase()))?;
        let r = self.mon.read_memory(0xffd368c, 4)?;
        Ok(u32::from_le_bytes([r[0], r[1], r[2], r[3]]))
    }

    /// Type one line at the BASIC 65 prompt and wait for READY to come back
    /// (up to 5 s; DOS commands take a moment).
    fn type_basic65_line(&mut self, line: &str) -> Result<()> {
        self.stuff_keybuffer(&format!("{line}\r"))?;
        let start = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if self.at_basic65_prompt()? || start.elapsed() > Duration::from_secs(5) {
                return Ok(());
            }
        }
    }

    /// Reset and wait until BASIC 65 is at its READY prompt. A mounted disk
    /// image survives this: the Hypervisor keeps it across a reset.
    pub fn reset_to_prompt(&mut self) -> Result<()> {
        self.reset()?;
        let start = std::time::Instant::now();
        while !self.at_basic65_prompt()? {
            if start.elapsed() > Duration::from_secs(15) {
                return Err(Error::Other("the MEGA65 did not come back to the BASIC 65 prompt after a reset".into()));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        self.mode = Mode::C65;
        Ok(())
    }

    /// Is BASIC 65 waiting at READY? ($3A, the high byte of the current line
    /// number, is $FF in direct mode.)
    pub fn at_basic65_prompt(&mut self) -> Result<bool> {
        if self.in_hypervisor()? {
            return Ok(false);
        }
        if self.mon.peek(VIC_BASE + 0x30)? & 1 != 0 {
            return Ok(false);
        }
        let d054 = self.mon.peek(VIC_BASE + 0x54)?;
        if d054 & 7 != 0 {
            return Ok(false);
        }
        if self.mon.peek(VIC_BASE + 0x61)? != 0x08 {
            return Ok(false);
        }
        Ok(self.mon.peek(0x3a)? == 0xff)
    }

    // ------------------------------------------------------------ control

    /// Reset via the monitor and wait for the ROM to come back up.
    pub fn reset(&mut self) -> Result<()> {
        self.mon.reset()?;
        self.mode = Mode::Unknown;
        std::thread::sleep(Duration::from_millis(1500));
        Ok(())
    }

    /// Type `GO64` + `Y` at the C65 prompt until C64 mode is detected.
    pub fn go64(&mut self) -> Result<()> {
        for _ in 0..10 {
            if self.detect_mode(Duration::from_secs(5))?.is_c64_like() {
                return Ok(());
            }
            self.mon.sync()?;
            self.stuff_keybuffer("\r\rGO64\rY\r")?;
            std::thread::sleep(Duration::from_millis(400));
        }
        Err(Error::Other("could not switch to C64 mode".into()))
    }

    /// Switch the VIC-IV between PAL and NTSC timing (same pokes as m65 --pal/--ntsc).
    pub fn set_video(&mut self, pal: bool) -> Result<()> {
        let m = &mut self.mon;
        let (d06f, d072, d048, d04a, d04e, tod) = if pal {
            (0x00, 0x00, 0x68, 0xf8, 0x68, true)
        } else {
            (0x87, 0x18, 0x2a, 0xb9, 0x2a, false)
        };
        m.poke(VIC_BASE + 0x6f, d06f)?;
        m.poke(VIC_BASE + 0x72, d072)?;
        m.poke(VIC_BASE + 0x48, d048)?;
        let v = m.peek(VIC_BASE + 0x49)?;
        m.poke(VIC_BASE + 0x49, v & 0xf0)?;
        m.poke(VIC_BASE + 0x4a, d04a)?;
        let v = m.peek(VIC_BASE + 0x4b)?;
        m.poke(VIC_BASE + 0x4b, 1 | (v & 0xf0))?;
        m.poke(VIC_BASE + 0x4e, d04e)?;
        let v = m.peek(VIC_BASE + 0x4f)?;
        m.poke(VIC_BASE + 0x4f, v & 0xf0)?;
        m.poke(VIC_BASE + 0x72, d072)?;
        for cia in [0xffd3c0eu32, 0xffd3d0e] {
            let v = m.peek(cia)?;
            m.poke(cia, if tod { v | 0x80 } else { v & 0x7f })?;
        }
        Ok(())
    }

    /// Ask the FPGA to reconfigure itself from a core slot (0-7) or flash address.
    /// This is a soft reboot into another core; it doesn't write flash.
    pub fn reconfigure_from_flash(&mut self, slot_or_addr: u32) -> Result<()> {
        let addr = if slot_or_addr < 8 { slot_or_addr * 0x80_0000 } else { slot_or_addr };
        self.mon.sync()?;
        self.mon.set_bytes(0xffd36c8, &[(addr >> 8) as u8, (addr >> 16) as u8, (addr >> 24) as u8, 0])?;
        self.mon.sync()?;
        self.mon.poke(0xffd36cf, 0x42)?;
        Ok(())
    }

    /// Core (from the monitor's `h` banner), hardware model ($D629) and ROM version.
    pub fn versions(&mut self) -> Result<Versions> {
        let help = self.mon.help().unwrap_or_default();
        let core = help
            .lines()
            .find_map(|l| l.split("build GIT: ").nth(1))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| help.lines().next().unwrap_or("").trim().to_string());
        let model_id = self.mon.peek(0xffd3629)?;
        let model = match model_id {
            1 => "MEGA65 R1",
            2 => "MEGA65 R2",
            3 => "MEGA65 R3",
            4 => "MEGA65 R4",
            5 => "MEGA65 R5",
            6 => "MEGA65 R6",
            33 => "MEGAphone R1 prototype",
            34 => "MEGAphone R4 prototype",
            64 => "Nexys 4 PSRAM",
            65 => "Nexys 4 DDR (no widget)",
            66 => "Nexys 4 DDR (widget)",
            253 => "QMTECH Wukong",
            254 => "simulated MEGA65 (Xemu)",
            _ => "unknown",
        };
        let rom = self.mon.read_memory(0x20016, 7)?;
        let rom = if rom[0] == b'V' && rom[1..].iter().all(|b| b.is_ascii_digit()) {
            format!("MEGA65 ROM {}", String::from_utf8_lossy(&rom))
        } else {
            "non-MEGA65 ROM".to_string()
        };
        Ok(Versions { core, model_id, model: model.to_string(), rom })
    }
}

#[derive(Debug, Clone)]
pub struct Versions {
    pub core: String,
    pub model_id: u8,
    pub model: String,
    pub rom: String,
}
