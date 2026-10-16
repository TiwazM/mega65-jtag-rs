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
    path: String,
    last: Option<(u8, u8, u8)>,
    pub reads: u64,
    pub writes: u64,
}

impl VirtualF011 {
    pub fn open(path: &str) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file, path: path.to_string(), last: None, reads: 0, writes: 0 })
    }

    /// Enable virtualisation on the MEGA65 side.
    pub fn enable(mon: &mut Monitor) -> Result<()> {
        mon.poke(0xffd3659, 0x01)?;
        mon.poke(0xffd368b, 0x03)
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
        let mut recent: Vec<u8> = Vec::new();
        eprintln!("serving {} as drive 0; press Ctrl-C to stop", self.path);
        while !stop() {
            let chunk = mon.read_quiet(Duration::from_millis(5), Duration::from_millis(100))?;
            if chunk.is_empty() {
                continue;
            }
            if chunk.windows(21).any(|w| w == b"MEGA65 Serial Monitor") {
                eprintln!("reset detected; the MEGA65 will need a power cycle before using the virtual disk again");
                break;
            }
            recent.extend_from_slice(&chunk);
            if recent.len() > 64 {
                recent.drain(..recent.len() - 64);
            }
            let mut i = 3;
            while i < recent.len() {
                let (t, s, d, m) = (recent[i - 3], recent[i - 2], recent[i - 1], recent[i]);
                if (m == b'!' || m == b'\\') && (t & s & d & 0x80) != 0 {
                    let (track, sector, side) = (t & 0x7f, s & 0x7f, d & 0x0f);
                    if m == b'!' {
                        self.read(mon, track, sector, side)?;
                    } else {
                        self.write(mon, track, sector, side)?;
                    }
                    recent.drain(..=i);
                    i = 3;
                    continue;
                }
                i += 1;
            }
        }
        Ok(())
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
        eprintln!("read  track {track:2} sector {sector:2} side {side}");
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
        eprintln!("write track {track:2} sector {sector:2} side {side}");
        Ok(())
    }
}
