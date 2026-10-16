//! Xilinx 7-series specifics: IDCODE, `.bit` parsing, and the JTAG configuration
//! sequence from UG470 (as mega65-tools' fpgajtag does it).

use std::time::Duration;

use super::Jtag;
use crate::{Error, Result};

pub const IR_LEN: usize = 6;
pub const IR_IDCODE: u8 = 0x09;
pub const IR_CFG_IN: u8 = 0x05;
pub const IR_CFG_OUT: u8 = 0x04;
pub const IR_JPROGRAM: u8 = 0x0b;
pub const IR_JSTART: u8 = 0x0c;
pub const IR_ISC_NOOP: u8 = 0x14;
pub const IR_BYPASS: u8 = 0x3f;

/// Known Artix-7 IDCODEs (top 4 version bits masked off).
pub fn part_name(idcode: u32) -> &'static str {
    match idcode & 0x0fff_ffff {
        0x0362_d093 => "xc7a35t",
        0x0362_c093 => "xc7a50t",
        0x0363_2093 => "xc7a75t",
        0x0363_1093 => "xc7a100t (MEGA65 R2 / Nexys4)",
        0x0363_6093 => "xc7a200t (MEGA65 R1/R3/R4/R5/R6)",
        _ => "unknown",
    }
}

/// A parsed Xilinx bitstream.
pub struct Bitstream {
    pub design: String,
    pub part: String,
    pub date: String,
    pub time: String,
    /// The IDCODE the bitstream was built for, from its `30018001` packet.
    pub idcode: Option<u32>,
    /// Raw configuration words as stored (big-endian bytes).
    pub payload: Vec<u8>,
}

impl Bitstream {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        const MAGIC: [u8; 13] = [0x00, 0x09, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x00, 0x00, 0x01];
        let mut bs = Bitstream { design: String::new(), part: String::new(), date: String::new(), time: String::new(), idcode: None, payload: Vec::new() };
        if bytes.len() > 13 && bytes[..13] == MAGIC {
            let mut p = 13;
            loop {
                if p >= bytes.len() {
                    return Err(Error::Other("truncated .bit header".into()));
                }
                let key = bytes[p];
                p += 1;
                if key == b'e' {
                    p += 4;
                    bs.payload = bytes[p.min(bytes.len())..].to_vec();
                    break;
                }
                let len = u16::from_be_bytes([bytes[p], bytes[p + 1]]) as usize;
                p += 2;
                let field = String::from_utf8_lossy(&bytes[p..(p + len).min(bytes.len())]).trim_end_matches('\0').to_string();
                match key {
                    b'a' => bs.design = field,
                    b'b' => bs.part = field,
                    b'c' => bs.date = field,
                    b'd' => bs.time = field,
                    _ => {}
                }
                p += len;
            }
        } else {
            bs.payload = bytes.to_vec();
        }
        // Find the IDCODE write packet in the first 8K.
        let head = &bs.payload[..bs.payload.len().min(8192)];
        bs.idcode = head.windows(8).find(|w| w[..4] == [0x30, 0x01, 0x80, 0x01]).map(|w| u32::from_be_bytes([w[4], w[5], w[6], w[7]]));
        Ok(bs)
    }
}

impl Jtag {
    /// Read the IDCODE of the (single) device in the chain.
    pub fn idcode(&mut self) -> Result<u32> {
        self.reset();
        self.shift_ir(IR_IDCODE, IR_LEN)?;
        let r = self.shift_dr(&[0u8; 8], 64)?;
        Ok(u32::from_le_bytes([r[0], r[1], r[2], r[3]]))
    }

    /// Read a configuration register (UG470 Type-1 read) via CFG_IN/CFG_OUT.
    pub fn read_config_reg(&mut self, reg: u8) -> Result<u32> {
        let words: [u32; 10] = [
            0xffff_ffff,
            0xaa99_5566,
            0x2000_0000,
            0x2800_0001 | ((reg as u32) << 13),
            0x2000_0000,
            0x2000_0000,
            0x3000_8001,
            0x0000_000d, // CMD = DESYNC
            0x2000_0000,
            0x2000_0000,
        ];
        let mut data = Vec::with_capacity(40);
        for w in words {
            data.extend(w.to_be_bytes().iter().map(|b| b.reverse_bits()));
        }
        self.shift_ir(IR_CFG_IN, IR_LEN)?;
        self.shift_dr(&data, data.len() * 8)?;
        self.shift_ir(IR_CFG_OUT, IR_LEN)?;
        let r = self.shift_dr(&[0u8; 4], 32)?;
        self.shift_ir(IR_BYPASS, IR_LEN)?;
        Ok(u32::from_be_bytes([r[0].reverse_bits(), r[1].reverse_bits(), r[2].reverse_bits(), r[3].reverse_bits()]))
    }

    /// Configure the FPGA from a bitstream. This is volatile: the core in flash
    /// is not modified. `desync` leaves the config engine desynced afterwards
    /// (proper, but MEGAFLASH then can't tell the core was JTAG-loaded).
    pub fn program(&mut self, bs: &Bitstream, desync: bool, progress: &mut dyn FnMut(usize, usize)) -> Result<ProgramReport> {
        let id = self.idcode()?;
        if let Some(want) = bs.idcode {
            if id & 0x0fff_ffff != want & 0x0fff_ffff {
                return Err(Error::Other(format!(
                    "bitstream is for IDCODE {want:08x} ({}) but the FPGA reports {id:08x} ({})",
                    part_name(want),
                    part_name(id)
                )));
            }
        }
        self.reset();
        self.shift_ir(IR_JPROGRAM, IR_LEN)?;
        self.shift_ir(IR_ISC_NOOP, IR_LEN)?;
        self.wait(Duration::from_millis(100))?;
        // Wait for INIT_B (IR capture bit 4) after the JPROGRAM house-clean.
        let mut cap = 0;
        for _ in 0..50 {
            cap = self.shift_ir(IR_ISC_NOOP, IR_LEN)?;
            if cap & 0x10 != 0 {
                break;
            }
            self.wait(Duration::from_millis(10))?;
        }
        if cap & 0x10 == 0 {
            return Err(Error::Protocol(format!("FPGA did not become ready for configuration (IR capture {cap:02x})")));
        }
        self.shift_ir(IR_CFG_IN, IR_LEN)?;
        // Send 32 zero bits then the payload, each byte bit-reversed so config
        // words go in MSB first.
        let mut data = Vec::with_capacity(bs.payload.len() + 4);
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend(bs.payload.iter().map(|b| b.reverse_bits()));
        self.shift_dr_write(&data, progress)?;
        self.wait(Duration::from_millis(10))?;
        let bootsts = self.read_config_reg(0x16)?;
        self.shift_ir(IR_JSTART, IR_LEN)?;
        self.run_idle(2000);
        let cap = self.shift_ir(IR_BYPASS, IR_LEN)?;
        let done = cap & 0x20 != 0;
        let stat = self.read_config_reg(0x07)?;
        if !desync {
            // Leave the config engine synced (no DESYNC), as fpgajtag does by default,
            // so MEGAFLASH can detect a JTAG-loaded core.
            let words: [u32; 5] = [0xffff_ffff, 0xaa99_5566, 0x4000_0000, 0x2800_e001, 0x0000_0000];
            let mut d = Vec::new();
            for w in words {
                d.extend(w.to_be_bytes().iter().map(|b| b.reverse_bits()));
            }
            self.reset();
            self.shift_ir(IR_CFG_IN, IR_LEN)?;
            self.shift_dr(&d, d.len() * 8)?;
            self.shift_ir(IR_CFG_OUT, IR_LEN)?;
            self.shift_dr(&[0u8; 4], 32)?;
        }
        self.reset();
        self.m.flush()?;
        if !done || stat & 0x4000 == 0 {
            return Err(Error::Protocol(format!(
                "configuration did not complete: IR capture {cap:02x}, STAT {stat:08x}, BOOTSTS {bootsts:08x}"
            )));
        }
        Ok(ProgramReport { idcode: id, bootsts, stat })
    }
}

pub struct ProgramReport {
    pub idcode: u32,
    pub bootsts: u32,
    pub stat: u32,
}
