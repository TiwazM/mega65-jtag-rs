//! FTDI MPSSE driver over raw USB (rusb), enough for JTAG: open interface A of
//! an FT2232H/FT4232H/FT232H, put it into MPSSE mode and shift bits.

use std::time::Duration;

use rusb::{Context, Device, DeviceHandle, UsbContext};

use crate::{Error, Result};

/// FTDI VID and the PIDs of chips with an MPSSE engine.
pub const FTDI_VID: u16 = 0x0403;
pub const FTDI_PIDS: &[u16] = &[0x6010, 0x6011, 0x6014];

const SIO_RESET: u8 = 0;
const SIO_SET_BAUD: u8 = 3;
const SIO_SET_LATENCY: u8 = 9;
const SIO_SET_BITMODE: u8 = 11;
const REQ_OUT: u8 = 0x40;
const TIMEOUT: Duration = Duration::from_millis(5000);

// MPSSE opcodes (all LSB first, write on -ve edge, read on -ve edge like fpgajtag).
pub const OP_WRITE_BYTES: u8 = 0x19;
pub const OP_READ_BYTES: u8 = 0x2c;
pub const OP_RW_BYTES: u8 = 0x3d;
pub const OP_WRITE_BITS: u8 = 0x1b;
pub const OP_READ_BITS: u8 = 0x2e;
pub const OP_RW_BITS: u8 = 0x3f;
pub const OP_TMS: u8 = 0x4b;
pub const OP_TMS_READ: u8 = 0x6f;
pub const OP_SET_LOW: u8 = 0x80;
pub const OP_SET_HIGH: u8 = 0x82;
pub const OP_LOOPBACK_OFF: u8 = 0x85;
pub const OP_CLOCK_DIVISOR: u8 = 0x86;
pub const OP_SEND_IMMEDIATE: u8 = 0x87;
pub const OP_DIS_DIV5: u8 = 0x8a;
pub const OP_CLOCK_BYTES: u8 = 0x8f;

#[derive(Debug, Clone)]
pub struct FtdiInfo {
    pub bus: u8,
    pub address: u8,
    pub port_numbers: Vec<u8>,
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: u16,
    pub product: String,
    pub serial: String,
}

impl std::fmt::Display for FtdiInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bus {} addr {} {:04x}:{:04x} rev {:04x} '{}'{}",
            self.bus,
            self.address,
            self.vid,
            self.pid,
            self.bcd_device,
            self.product,
            if self.serial.is_empty() { String::new() } else { format!(" serial {}", self.serial) }
        )
    }
}

/// Enumerate FTDI devices that could be a MEGA65 JTAG adapter.
pub fn list_devices() -> Result<Vec<FtdiInfo>> {
    let ctx = Context::new()?;
    let mut out = Vec::new();
    for dev in ctx.devices()?.iter() {
        let d = dev.device_descriptor()?;
        if d.vendor_id() != FTDI_VID || !FTDI_PIDS.contains(&d.product_id()) {
            continue;
        }
        let (product, serial) = match dev.open() {
            Ok(h) => (
                h.read_product_string_ascii(&d).unwrap_or_default(),
                h.read_serial_number_string_ascii(&d).unwrap_or_default(),
            ),
            Err(_) => (String::new(), String::new()),
        };
        out.push(FtdiInfo {
            bus: dev.bus_number(),
            address: dev.address(),
            port_numbers: dev.port_numbers().unwrap_or_default(),
            vid: d.vendor_id(),
            pid: d.product_id(),
            bcd_device: { let v = d.device_version(); ((v.major() as u16) << 8) | ((v.minor() as u16) << 4) | v.sub_minor() as u16 },
            product,
            serial,
        });
    }
    Ok(out)
}

pub struct Mpsse {
    h: DeviceHandle<Context>,
    ep_out: u8,
    ep_in: u8,
    pub info: FtdiInfo,
    wbuf: Vec<u8>,
    claimed: bool,
}

impl Mpsse {
    /// Open interface A of the given (or the only) FTDI device and enter MPSSE mode.
    pub fn open(select: Option<&str>) -> Result<Self> {
        let ctx = Context::new()?;
        let mut chosen: Option<(Device<Context>, FtdiInfo)> = None;
        for dev in ctx.devices()?.iter() {
            let d = dev.device_descriptor()?;
            if d.vendor_id() != FTDI_VID || !FTDI_PIDS.contains(&d.product_id()) {
                continue;
            }
            let h = dev.open()?;
            let info = FtdiInfo {
                bus: dev.bus_number(),
                address: dev.address(),
                port_numbers: dev.port_numbers().unwrap_or_default(),
                vid: d.vendor_id(),
                pid: d.product_id(),
                bcd_device: { let v = d.device_version(); ((v.major() as u16) << 8) | ((v.minor() as u16) << 4) | v.sub_minor() as u16 },
                product: h.read_product_string_ascii(&d).unwrap_or_default(),
                serial: h.read_serial_number_string_ascii(&d).unwrap_or_default(),
            };
            let matches = match select {
                None => true,
                Some(s) => info.serial == s || format!("{}:{}", info.bus, info.address) == s,
            };
            if matches {
                chosen = Some((dev, info));
                break;
            }
        }
        let (dev, info) = chosen.ok_or_else(|| Error::Open("no FTDI JTAG adapter found (VID 0403, PID 6010/6011/6014)".into()))?;
        let h = dev.open()?;
        let _ = h.set_auto_detach_kernel_driver(true);
        let mut claimed = true;
        if let Err(e) = h.claim_interface(0) {
            // On macOS the Apple FTDI driver may hold the interface. We can still
            // issue control/bulk transfers in many cases, so carry on and let the
            // first transfer tell us if it really is unusable.
            if e != rusb::Error::Busy && e != rusb::Error::Access {
                return Err(e.into());
            }
            claimed = false;
        }
        let mut m = Mpsse { h, ep_out: 0x02, ep_in: 0x81, info, wbuf: Vec::with_capacity(8192), claimed };
        m.init()?;
        Ok(m)
    }

    fn ctrl(&self, req: u8, value: u16) -> Result<()> {
        self.h.write_control(REQ_OUT, req, value, 1, &[], TIMEOUT)?;
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        self.ctrl(SIO_RESET, 0)?;
        self.ctrl(SIO_SET_BAUD, 0x04e2)?;
        self.ctrl(SIO_SET_LATENCY, 2)?;
        self.ctrl(SIO_SET_BITMODE, 0x0000)?;
        self.ctrl(SIO_SET_BITMODE, 0x0200)?;
        self.ctrl(SIO_RESET, 1)?;
        self.ctrl(SIO_RESET, 2)?;
        std::thread::sleep(Duration::from_millis(20));
        // Sync: a bad opcode is echoed back as FA xx.
        for &op in &[0xaau8, 0xaa, 0xaa, 0xaa, 0xab] {
            self.write_now(&[op, OP_SEND_IMMEDIATE])?;
            let r = self.read_exact(2, Duration::from_millis(500));
            match r {
                Ok(r) if r == [0xfa, op] => {}
                Ok(r) => return Err(Error::Protocol(format!("MPSSE sync: expected FA {op:02X}, got {r:02X?}"))),
                Err(e) => return Err(Error::Protocol(format!("MPSSE sync failed ({e}); is another program using the JTAG port?"))),
            }
        }
        // 60 MHz base, divisor 1 -> 15 MHz TCK. Low byte: TCK/TDI/TMS out, TDO in; TMS high.
        let mut setup = vec![OP_LOOPBACK_OFF, OP_DIS_DIV5, OP_CLOCK_DIVISOR, 0x01, 0x00, OP_SET_LOW, 0xe8, 0xeb];
        if self.info.bcd_device == 0x0700 {
            setup.extend_from_slice(&[OP_SET_HIGH, 0x20, 0x30, OP_SET_HIGH, 0x30, 0x00, OP_SET_HIGH, 0x00, 0x00]);
        } else {
            setup.extend_from_slice(&[OP_SET_HIGH, 0x00, 0x60]);
        }
        self.write_now(&setup)?;
        Ok(())
    }

    /// Queue bytes to send.
    pub fn queue(&mut self, data: &[u8]) {
        self.wbuf.extend_from_slice(data);
    }

    /// Send queued bytes.
    pub fn flush(&mut self) -> Result<()> {
        if self.wbuf.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.wbuf);
        let r = self.write_now(&buf);
        self.wbuf = buf;
        self.wbuf.clear();
        r
    }

    fn write_now(&mut self, data: &[u8]) -> Result<()> {
        let mut off = 0;
        while off < data.len() {
            let n = self.h.write_bulk(self.ep_out, &data[off..], TIMEOUT)?;
            if n == 0 {
                return Err(Error::Protocol("USB write stalled".into()));
            }
            off += n;
        }
        Ok(())
    }

    /// Flush and then read exactly `n` payload bytes (FTDI status bytes stripped).
    pub fn read_exact(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>> {
        self.flush()?;
        let mut out = Vec::with_capacity(n);
        let mut buf = [0u8; 4096];
        let start = std::time::Instant::now();
        while out.len() < n {
            if start.elapsed() > timeout {
                return Err(Error::Timeout(format!("JTAG read ({} of {n} bytes)", out.len())));
            }
            let got = match self.h.read_bulk(self.ep_in, &mut buf, Duration::from_millis(100)) {
                Ok(g) => g,
                Err(rusb::Error::Timeout) => 0,
                Err(e) => return Err(e.into()),
            };
            if got > 2 {
                out.extend_from_slice(&buf[2..got]);
            }
        }
        out.truncate(n);
        Ok(out)
    }

    /// Queue a read of `n` bytes result and perform it.
    pub fn exchange(&mut self, n: usize) -> Result<Vec<u8>> {
        self.queue(&[OP_SEND_IMMEDIATE]);
        self.read_exact(n, Duration::from_secs(3))
    }

    /// Queue a GPIO write to the low byte (ADBUS).
    pub fn set_low(&mut self, value: u8, dir: u8) {
        self.queue(&[OP_SET_LOW, value, dir]);
    }

    /// Clock `n` TCK cycles without data (in multiples of 8).
    pub fn clock_bytes(&mut self, nbytes: usize) {
        let mut left = nbytes;
        while left > 0 {
            let n = left.min(65536);
            self.queue(&[OP_CLOCK_BYTES, ((n - 1) & 0xff) as u8, ((n - 1) >> 8) as u8]);
            left -= n;
        }
    }

    pub fn was_claimed(&self) -> bool {
        self.claimed
    }
}

impl Drop for Mpsse {
    fn drop(&mut self) {
        let _ = self.flush();
        // Leave MPSSE mode so the port behaves if something else opens it.
        let _ = self.ctrl(SIO_SET_BITMODE, 0x0000);
        if self.claimed {
            let _ = self.h.release_interface(0);
        }
    }
}
