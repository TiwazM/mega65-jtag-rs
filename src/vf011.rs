//! Virtual F011 floppy: serve a local D81 image to the MEGA65. The Hypervisor
//! (with $D659 bit 0 set and $D68B enabling disk 0) sends sector requests over
//! the serial monitor as `<track|$80><sector|$80><side|$80>!` for reads and the
//! same with `\` for writes; we move 512 bytes through fixed buffers and ack by
//! writing the side to $D086.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;

use crate::monitor::Monitor;
use crate::Result;

const READ_BUFFER: u32 = 0xffd6c00;
const WRITE_BUFFER: u32 = 0xffd6c00;

pub struct VirtualF011 {
    file: File,
    pub path: String,
    last: Option<(u8, u8, u8)>,
    pub reads: u64,
    pub writes: u64,
}

/// One thing [`VirtualF011::poll`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    Read { track: u8, sector: u8, side: u8 },
    Write { track: u8, sector: u8, side: u8 },
    /// The MEGA65 printed its reset banner: the Hypervisor has forgotten the
    /// virtual drive and needs a power cycle before it works again.
    Reset,
}

impl VirtualF011 {
    pub fn open(path: &str) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file, path: path.to_string(), last: None, reads: 0, writes: 0 })
    }

    /// Set the virtualisation flags. The Hypervisor only looks at them when
    /// it boots, so on a running machine use [`Self::start`] instead.
    pub fn enable(mon: &mut Monitor) -> Result<()> {
        mon.poke(0xffd3659, 0x01)?;
        mon.poke(0xffd368b, 0x03)
    }

    /// Attach the virtual drive the way mega65-tools' `m65 -d` does: trap
    /// into the Hypervisor, set the flags, and restart the Hypervisor at its
    /// entry point so it boots with the virtual drive as drive 0. That
    /// reboots the MEGA65 (like a reset, without clearing the flags), so
    /// whatever was in memory is gone; it takes a few seconds to reach READY.
    pub fn start(mach: &mut crate::machine::Machine) -> Result<()> {
        mach.enter_hypervisor()?;
        Self::enable(&mut mach.mon)?;
        mach.mon.set_pc(0x8100)?;
        mach.mon.stray.clear();
        mach.mon.vf011_requests.clear();
        mach.mon.capture_vf011 = true;
        mach.mon.start_cpu()?;
        mach.mode = crate::machine::Mode::Unknown;
        Ok(())
    }

    /// Detach the virtual drive and restart the Hypervisor so it boots with
    /// the SD card's drive again. Clearing the flags on a running machine
    /// leaves the ROM hung on a drive that no longer answers, so this is the
    /// only clean way back; like [`Self::start`] it reboots the MEGA65.
    pub fn stop(mach: &mut crate::machine::Machine) -> Result<()> {
        mach.mon.capture_vf011 = false;
        mach.mon.vf011_requests.clear();
        mach.enter_hypervisor()?;
        Self::disable(&mut mach.mon)?;
        mach.mon.set_pc(0x8100)?;
        mach.mon.stray.clear();
        mach.mon.start_cpu()?;
        mach.mode = crate::machine::Mode::Unknown;
        Ok(())
    }

    pub fn disable(mon: &mut Monitor) -> Result<()> {
        mon.poke(0xffd368b, 0x06)?;
        mon.poke(0xffd3659, 0x00)
    }

    fn offset(track: u8, sector: u8, side: u8) -> u64 {
        let physical = if side == 0 { sector as i64 - 1 } else { sector as i64 + 9 };
        ((track as i64 * 20 + physical) * 512).max(0) as u64
    }

    /// Serve requests until `stop` returns true. Prints a line per request.
    pub fn serve(&mut self, mon: &mut Monitor, mut stop: impl FnMut() -> bool) -> Result<()> {
        eprintln!("serving {} as drive 0; press Ctrl-C to stop", self.path);
        while !stop() {
            for r in self.poll(mon, Duration::from_millis(100))? {
                match r {
                    Served::Reset => {
                        eprintln!("reset detected; the MEGA65 will need a power cycle before using the virtual disk again");
                        return Ok(());
                    }
                    Served::Read { track, sector, side } => eprintln!("read  track {track:2} sector {sector:2} side {side}"),
                    Served::Write { track, sector, side } => eprintln!("write track {track:2} sector {sector:2} side {side}"),
                }
            }
        }
        Ok(())
    }

    /// Listen for up to `budget` and answer every request that has arrived,
    /// including ones the monitor captured while other commands ran (see
    /// `Monitor::capture_vf011`). Returns what was served, so a caller that
    /// does other work between polls (Studio) can keep the drive alive.
    pub fn poll(&mut self, mon: &mut Monitor, budget: Duration) -> Result<Vec<Served>> {
        mon.capture_vf011 = true;
        let mut out = Vec::new();
        let chunk = mon.read_quiet(Duration::from_millis(5), budget)?;
        if chunk.windows(21).any(|w| w == b"MEGA65 Serial Monitor") {
            out.push(Served::Reset);
            return Ok(out);
        }
        for [t, s, d, m] in std::mem::take(&mut mon.vf011_requests) {
            let (track, sector, side) = (t & 0x7f, s & 0x7f, d & 0x0f);
            if m == b'!' {
                self.read(mon, track, sector, side)?;
                out.push(Served::Read { track, sector, side });
            } else {
                self.write(mon, track, sector, side)?;
                out.push(Served::Write { track, sector, side });
            }
        }
        Ok(out)
    }

    fn read(&mut self, mon: &mut Monitor, track: u8, sector: u8, side: u8) -> Result<()> {
        if self.last != Some((track, sector, side)) {
            let mut buf = [0u8; 512];
            self.file.seek(SeekFrom::Start(Self::offset(track, sector, side)))?;
            let n = self.file.read(&mut buf)?;
            if n == 512 {
                mon.write_memory(READ_BUFFER, &buf)?;
            }
            self.last = Some((track, sector, side));
        }
        mon.stop_cpu()?;
        mon.poke(0xffd3086, side & 0x7f)?;
        mon.start_cpu()?;
        self.reads += 1;
        Ok(())
    }

    fn write(&mut self, mon: &mut Monitor, track: u8, sector: u8, side: u8) -> Result<()> {
        let buf = mon.read_memory(WRITE_BUFFER, 512)?;
        self.file.seek(SeekFrom::Start(Self::offset(track, sector, side)))?;
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.last = None;
        mon.stop_cpu()?;
        mon.poke(0xffd3086, side & 0x0f)?;
        mon.start_cpu()?;
        self.writes += 1;
        Ok(())
    }
}
