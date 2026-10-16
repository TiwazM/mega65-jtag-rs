//! JTAG access to the MEGA65's Xilinx Artix-7 FPGA through the FTDI adapter:
//! read the IDCODE, and configure the FPGA from a `.bit` file (volatile; the
//! core in flash is untouched and comes back at the next power cycle).

pub mod mpsse;
pub mod xilinx;

use std::time::Duration;

use mpsse::*;

use crate::Result;

/// JTAG TAP states we track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapState {
    Reset,
    Idle,
    ShiftDr,
    ShiftIr,
    Exit1Dr,
    Exit1Ir,
    PauseDr,
    PauseIr,
}

pub struct Jtag {
    pub m: Mpsse,
    state: TapState,
}

impl Jtag {
    pub fn open(select: Option<&str>) -> Result<Self> {
        let m = Mpsse::open(select)?;
        let mut j = Jtag { m, state: TapState::Reset };
        j.reset();
        Ok(j)
    }

    /// Queue a TMS sequence (LSB first, up to 7 bits) with TDI held at `tdi`.
    fn tms(&mut self, bits: u8, n: u8, tdi: bool) {
        self.m.queue(&[OP_TMS, n - 1, bits | if tdi { 0x80 } else { 0 }]);
    }

    /// Go to Test-Logic-Reset.
    pub fn reset(&mut self) {
        self.tms(0x1f, 5, false);
        self.state = TapState::Reset;
    }

    pub fn idle(&mut self) {
        match self.state {
            TapState::Reset => self.tms(0x00, 1, false),
            TapState::Idle => {}
            TapState::Exit1Dr | TapState::Exit1Ir => self.tms(0x01, 2, false),
            TapState::PauseDr | TapState::PauseIr => self.tms(0x03, 3, false),
            TapState::ShiftDr | TapState::ShiftIr => self.tms(0x03, 3, false),
        }
        self.state = TapState::Idle;
    }

    fn shift_ir_state(&mut self) {
        self.idle();
        self.tms(0x03, 4, false);
        self.state = TapState::ShiftIr;
    }

    fn shift_dr_state(&mut self) {
        self.idle();
        self.tms(0x01, 3, false);
        self.state = TapState::ShiftDr;
    }

    /// Run `n` TCK cycles in Run-Test/Idle.
    pub fn run_idle(&mut self, n: usize) {
        self.idle();
        let mut left = n;
        while left >= 7 {
            self.tms(0x00, 7, false);
            left -= 7;
        }
        if left > 0 {
            self.tms(0x00, left as u8, false);
        }
    }

    /// Shift `nbits` from `data` (LSB first) through the current shift register,
    /// finishing in Exit1. With `read`, the captured bits are returned (LSB first).
    fn shift(&mut self, data: &[u8], nbits: usize, read: bool) -> Result<Vec<u8>> {
        assert!(nbits >= 1);
        let full_bytes = (nbits - 1) / 8;
        let rem_bits = (nbits - 1) % 8;
        let last_bit = (data[(nbits - 1) / 8] >> ((nbits - 1) % 8)) & 1 != 0;
        let mut expect = 0usize;
        // Whole bytes, in chunks the MPSSE length field can hold.
        let mut off = 0;
        while off < full_bytes {
            let n = (full_bytes - off).min(4096);
            self.m.queue(&[if read { OP_RW_BYTES } else { OP_WRITE_BYTES }, ((n - 1) & 0xff) as u8, ((n - 1) >> 8) as u8]);
            self.m.queue(&data[off..off + n]);
            off += n;
            if read {
                expect += n;
            }
        }
        if rem_bits > 0 {
            self.m.queue(&[if read { OP_RW_BITS } else { OP_WRITE_BITS }, (rem_bits - 1) as u8, data[full_bytes]]);
            if read {
                expect += 1;
            }
        }
        // Last bit goes out with TMS=1 -> Exit1.
        self.m.queue(&[if read { OP_TMS_READ } else { OP_TMS }, 0, 0x01 | if last_bit { 0x80 } else { 0 }]);
        if read {
            expect += 1;
        }
        self.state = match self.state {
            TapState::ShiftDr => TapState::Exit1Dr,
            TapState::ShiftIr => TapState::Exit1Ir,
            s => s,
        };
        if !read {
            return Ok(Vec::new());
        }
        let raw = self.m.exchange(expect)?;
        // Reassemble: full bytes, then rem_bits in the top of one byte, then 1 bit at bit 7.
        let mut out = vec![0u8; nbits.div_ceil(8)];
        let mut bit = 0usize;
        let mut push = |v: bool, bit: &mut usize| {
            if v {
                out[*bit / 8] |= 1 << (*bit % 8);
            }
            *bit += 1;
        };
        let mut i = 0;
        for _ in 0..full_bytes {
            for b in 0..8 {
                push((raw[i] >> b) & 1 != 0, &mut bit);
            }
            i += 1;
        }
        if rem_bits > 0 {
            let v = raw[i] >> (8 - rem_bits);
            for b in 0..rem_bits {
                push((v >> b) & 1 != 0, &mut bit);
            }
            i += 1;
        }
        push(raw[i] & 0x80 != 0, &mut bit);
        Ok(out)
    }

    /// Load an instruction (6 bits for Artix-7) and return the captured IR bits.
    pub fn shift_ir(&mut self, ir: u8, ir_len: usize) -> Result<u8> {
        self.shift_ir_state();
        let r = self.shift(&[ir], ir_len, true)?;
        self.idle();
        Ok(r[0])
    }

    /// Shift a data register and return what came out.
    pub fn shift_dr(&mut self, data: &[u8], nbits: usize) -> Result<Vec<u8>> {
        self.shift_dr_state();
        let r = self.shift(data, nbits, true)?;
        self.idle();
        Ok(r)
    }

    /// Shift a large data register without reading back, with the last bit on TMS.
    pub fn shift_dr_write(&mut self, data: &[u8], progress: &mut dyn FnMut(usize, usize)) -> Result<()> {
        self.shift_dr_state();
        let nbits = data.len() * 8;
        let full = data.len() - 1;
        let mut off = 0;
        while off < full {
            let n = (full - off).min(4096);
            self.m.queue(&[OP_WRITE_BYTES, ((n - 1) & 0xff) as u8, ((n - 1) >> 8) as u8]);
            self.m.queue(&data[off..off + n]);
            self.m.flush()?;
            off += n;
            progress(off, data.len());
        }
        let last = data[full];
        self.m.queue(&[OP_WRITE_BITS, 6, last]);
        self.m.queue(&[OP_TMS, 0, 0x01 | if last & 0x80 != 0 { 0x80 } else { 0 }]);
        self.state = TapState::Exit1Dr;
        let _ = nbits;
        self.idle();
        self.m.flush()?;
        progress(data.len(), data.len());
        Ok(())
    }

    /// Timed wait: fpgajtag "pulses" the DONE GPIO and clocks for ~100ms. A sleep
    /// after flushing is equivalent for our purposes.
    pub fn wait(&mut self, d: Duration) -> Result<()> {
        self.m.flush()?;
        std::thread::sleep(d);
        Ok(())
    }
}
