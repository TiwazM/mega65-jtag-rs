//! `mega65`: talk to a MEGA65 over its USB JTAG adapter.
//!
//! The FTDI adapter exposes two things: a UART wired to the MEGA65's built-in
//! serial ("matrix mode") monitor, and a JTAG port wired to the FPGA. This crate
//! wraps both:
//!
//! * [`monitor::Monitor`] drives the serial monitor: memory read/write, CPU
//!   stop/step/continue, breakpoints, watchpoints, registers, reset.
//! * [`machine`] builds MEGA65-specific helpers on top (mode detection, PRG
//!   loading, keyboard injection, Hypervisor status, mode switching).
//! * [`screen`] captures the VIC-IV text screen as text or a PNG.
//! * [`vf011`] serves a local D81 image to the MEGA65's virtual floppy.
//! * [`jtag`] talks to the FPGA directly through FTDI MPSSE (IDCODE, bitstream load).

pub mod fat32;
pub mod jtag;
pub mod machine;
pub mod monitor;
pub mod petscii;
pub mod screen;
pub mod sdcard;
pub mod transport;
pub mod vf011;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not open {0}")]
    Open(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serial error: {0}")]
    Serial(#[from] serialport::Error),
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),
    #[error("timed out waiting for {0}")]
    Timeout(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Parse a hex number with optional `$`, `0x` or `h` decoration.
pub fn parse_hex(s: &str) -> std::result::Result<u32, String> {
    let t = s.trim();
    let t = t
        .strip_prefix('$')
        .or_else(|| t.strip_prefix("0x"))
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let t = t.strip_suffix('h').unwrap_or(t);
    u32::from_str_radix(t, 16).map_err(|e| format!("bad hex number '{s}': {e}"))
}

/// Parse a number: hex with `$`/`0x` prefix, otherwise decimal; `#` forces decimal.
pub fn parse_num(s: &str) -> std::result::Result<u32, String> {
    let t = s.trim();
    if t.starts_with('$') || t.starts_with("0x") || t.starts_with("0X") {
        parse_hex(t)
    } else if let Some(d) = t.strip_prefix('#') {
        d.parse().map_err(|e| format!("bad number '{s}': {e}"))
    } else {
        t.parse().map_err(|e| format!("bad number '{s}': {e}"))
    }
}

/// Classic hex dump with addresses.
pub fn hexdump(base: u32, data: &[u8]) -> String {
    let mut s = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        s.push_str(&format!("{:07X}: ", base as usize + i * 16));
        for j in 0..16 {
            match chunk.get(j) {
                Some(b) => s.push_str(&format!("{b:02X} ")),
                None => s.push_str("   "),
            }
        }
        s.push(' ');
        for &b in chunk {
            s.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        s.push('\n');
    }
    s
}
