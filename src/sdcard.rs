//! Access to the MEGA65's SD card through the serial monitor.
//!
//! Two backends implement [`BlockDevice`]:
//!
//! * [`DirectSd`] drives the SD controller registers ($D680-$D689) and reads the
//!   sector buffer at $FFD6E00 through plain monitor memory commands. It needs
//!   nothing on the MEGA65 side, but costs several round trips per sector.
//! * [`HelperSd`] uploads mega65-tools' `remotesd` helper program to the MEGA65
//!   and talks to it with the job protocol mega65_ftp uses: batched reads stream
//!   raw sectors over the UART, batched writes go via a staging area at $50000.
//!   This replaces whatever program was in memory and needs C64 mode.
//!
//! A write-back [`SectorCache`] sits on top of either.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::machine::{Machine, Mode};
use crate::monitor::Monitor;
use crate::{Error, Result};

pub const SECTOR: usize = 512;
const SD_CTRL: u32 = 0xffd3680;
const SD_ADDR: u32 = 0xffd3681;
const SD_BUFSEL: u32 = 0xffd3689;
const SD_BUFFER: u32 = 0xffd6e00;

/// The `remotesd` helper (GPL, from mega65-tools) as a C64 PRG with a `SYS 2061` stub.
pub const REMOTESD_PRG: &[u8] = include_bytes!("../assets/remotesd.prg");
const HELPER_JOBS: u32 = 0xc000;
/// Two 64 KB staging banks: one being written to the card while the other fills.
const HELPER_STAGING: [u32; 2] = [0x50000, 0x40000];

pub trait BlockDevice {
    fn read_sectors(&mut self, first: u32, count: usize) -> Result<Vec<u8>>;
    fn write_sector(&mut self, sector: u32, data: &[u8]) -> Result<()>;
    /// Called when the caller is done with a batch of writes.
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
    /// Largest number of sectors worth asking for in one read.
    fn preferred_batch(&self) -> usize {
        1
    }
    fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

// --------------------------------------------------------------- direct

pub struct DirectSd<'a> {
    pub mon: &'a mut Monitor,
    prepared: bool,
}

impl<'a> DirectSd<'a> {
    pub fn new(mon: &'a mut Monitor) -> Self {
        Self { mon, prepared: false }
    }

    fn prepare(&mut self) -> Result<()> {
        if self.prepared {
            return Ok(());
        }
        if !self.mon.cpu_stopped {
            self.mon.stop_cpu()?;
        }
        let v = self.mon.peek(SD_BUFSEL)?;
        self.mon.poke(SD_BUFSEL, v | 0x80)?;
        self.wait_ready()?;
        self.prepared = true;
        Ok(())
    }

    fn wait_ready(&mut self) -> Result<()> {
        let start = Instant::now();
        let mut reset_done = false;
        loop {
            let st = self.mon.peek(SD_CTRL)?;
            if st & 3 == 0 {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(2) && !reset_done {
                self.mon.poke(SD_CTRL, 0)?;
                self.mon.poke(SD_CTRL, 1)?;
                std::thread::sleep(Duration::from_secs(1));
                reset_done = true;
            } else if start.elapsed() > Duration::from_secs(5) {
                return Err(Error::Timeout(format!("SD controller ready (status ${st:02x})")));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl BlockDevice for DirectSd<'_> {
    fn read_sectors(&mut self, first: u32, count: usize) -> Result<Vec<u8>> {
        self.prepare()?;
        let mut out = Vec::with_capacity(count * SECTOR);
        for i in 0..count as u32 {
            let s = first + i;
            self.wait_ready()?;
            self.mon.set_bytes(SD_ADDR, &s.to_le_bytes())?;
            self.mon.poke(SD_CTRL, 2)?;
            self.wait_ready()?;
            out.extend(self.mon.read_memory(SD_BUFFER, SECTOR)?);
        }
        Ok(out)
    }

    fn write_sector(&mut self, sector: u32, data: &[u8]) -> Result<()> {
        self.prepare()?;
        let mut buf = data.to_vec();
        buf.resize(SECTOR, 0);
        self.wait_ready()?;
        self.mon.write_memory(SD_BUFFER, &buf)?;
        self.mon.set_bytes(SD_ADDR, &sector.to_le_bytes())?;
        self.mon.poke(SD_CTRL, 0x57)?;
        self.mon.poke(SD_CTRL, 3)?;
        self.wait_ready()?;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if self.prepared {
            self.mon.start_cpu()?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------- helper

pub struct HelperSd<'a> {
    pub mon: &'a mut Monitor,
    pending_writes: BTreeMap<u32, Vec<u8>>,
    /// Use SD multi-sector writes for runs of consecutive sectors
    /// (set `M65_SD_SINGLE_WRITES=1` to disable).
    pub multi_write: bool,
    /// Which staging bank the next batch uses.
    bank: usize,
    /// A write batch the helper is still executing.
    inflight: bool,
    /// Pipeline uploads with card writes (`M65_SD_NO_PIPELINE=1` disables).
    pub pipeline: bool,
}

impl<'a> HelperSd<'a> {
    /// Upload and start the helper unless it is already running.
    pub fn start(mach: &'a mut Machine, force: bool) -> Result<Self> {
        let mon = &mut mach.mon;
        mon.sync()?;
        // Is a helper already resident? Job 0x13 answers with its version string.
        mon.purge()?;
        mon.write_raw(b"sc001 13\rsc000 1\r")?;
        let reply = mon.read_quiet(Duration::from_millis(200), Duration::from_millis(1500))?;
        if find(&reply, b"MEGA65FT1.0") {
            return Ok(Self::wrap(&mut mach.mon));
        }
        mach.detect_mode(Duration::from_secs(10))?;
        if !force {
            // Refuse to clobber a BASIC program in memory.
            let p = if mach.mode.is_c64_like() { 0x0801 } else { 0x2001 };
            let link = mach.mon.read_memory(p, 2)?;
            if link != [0, 0] {
                return Err(Error::Other(
                    "a program is loaded on the MEGA65; the SD helper would overwrite it (use --force)".into(),
                ));
            }
        }
        if !mach.mode.is_c64_like() {
            mach.go64()?;
        }
        let openrom = mach.mode == Mode::OpenRom;
        mach.mon.stop_cpu()?;
        mach.mon.write_memory(0x0801, &REMOTESD_PRG[2..])?;
        if openrom {
            mach.mon.start_cpu()?;
            mach.stuff_keybuffer("RUN\r")?;
        } else {
            mach.mon.set_pc(0x080d)?;
            mach.mon.start_cpu()?;
        }
        std::thread::sleep(Duration::from_millis(600));
        let mon = &mut mach.mon;
        mon.purge()?;
        mon.write_raw(b"sc001 13\rsc000 1\r")?;
        let reply = mon.read_quiet(Duration::from_millis(200), Duration::from_millis(2000))?;
        if !find(&reply, b"MEGA65FT1.0") {
            return Err(Error::Protocol("SD helper did not start".into()));
        }
        Ok(Self::wrap(&mut mach.mon))
    }

    /// Run a batch of jobs and collect any raw data they stream back.
    fn execute(&mut self, jobs: &[u8], njobs: usize, expect_bytes: usize) -> Result<Vec<u8>> {
        if jobs.len() > 0xf00 {
            return Err(Error::Other("job batch too large".into()));
        }
        self.wait_inflight()?;
        self.mon.write_memory(HELPER_JOBS + 1, jobs)?;
        self.mon.purge()?;
        self.mon.write_raw(format!("sc000 {njobs:x}\r").as_bytes())?;
        let mut data = Vec::with_capacity(expect_bytes);
        let mut raw: Vec<u8> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10 + expect_bytes as u64 / 20_000);
        let mut remaining = 0usize;
        loop {
            let chunk = self.mon.read_quiet(Duration::from_millis(2), Duration::from_millis(50))?;
            if chunk.is_empty() {
                if Instant::now() > deadline {
                    return Err(Error::Timeout(format!("SD helper batch ({} of {} bytes)", data.len(), expect_bytes)));
                }
                continue;
            }
            let mut i = 0;
            while i < chunk.len() {
                if remaining > 0 {
                    let n = remaining.min(chunk.len() - i);
                    data.extend_from_slice(&chunk[i..i + n]);
                    remaining -= n;
                    i += n;
                    continue;
                }
                raw.push(chunk[i]);
                i += 1;
                // Header: FTJOBDATR:XXXX:XXXXXXXX:
                if raw.ends_with(b":") && raw.len() >= 24 {
                    let tail = &raw[raw.len() - 24..];
                    if tail.starts_with(b"FTJOBDATR:") {
                        if let Ok(n) = usize::from_str_radix(std::str::from_utf8(&tail[15..23]).unwrap_or("x"), 16) {
                            remaining = n;
                            raw.clear();
                            continue;
                        }
                    }
                }
                if raw.ends_with(b"FTBATCHDONE") {
                    if data.len() < expect_bytes {
                        return Err(Error::Protocol(format!("short SD read: {} of {} bytes", data.len(), expect_bytes)));
                    }
                    data.truncate(expect_bytes);
                    return Ok(data);
                }
                if raw.len() > 64 {
                    raw.drain(..32);
                }
            }
        }
    }
}

impl<'a> HelperSd<'a> {
    fn wrap(mon: &'a mut Monitor) -> Self {
        let pipeline = std::env::var_os("M65_SD_NO_PIPELINE").is_none();
        mon.load_without_halt = pipeline;
        Self {
            mon,
            pending_writes: BTreeMap::new(),
            multi_write: std::env::var_os("M65_SD_SINGLE_WRITES").is_none(),
            bank: 0,
            inflight: false,
            pipeline,
        }
    }

    /// Wait for the helper to finish the batch in flight, if any.
    fn wait_inflight(&mut self) -> Result<()> {
        if !self.inflight {
            return Ok(());
        }
        self.inflight = false;
        // It may have answered while an upload was being echoed.
        if find(&self.mon.stray, b"FTBATCHDONE") {
            self.mon.stray.clear();
            return Ok(());
        }
        self.mon.stray.clear();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut raw: Vec<u8> = Vec::new();
        loop {
            let chunk = self.mon.read_quiet(Duration::from_millis(2), Duration::from_millis(50))?;
            raw.extend_from_slice(&chunk);
            if find(&raw, b"FTBATCHDONE") {
                return Ok(());
            }
            if raw.len() > 4096 {
                raw.drain(..2048);
            }
            if Instant::now() > deadline {
                return Err(Error::Timeout("SD helper write batch".into()));
            }
        }
    }
}

impl BlockDevice for HelperSd<'_> {
    fn read_sectors(&mut self, first: u32, count: usize) -> Result<Vec<u8>> {
        self.flush()?;
        self.wait_inflight()?;
        let mut out = Vec::with_capacity(count * SECTOR);
        let mut s = first;
        let mut left = count;
        while left > 0 {
            let n = left.min(256);
            let mut job = vec![0x04u8];
            job.extend_from_slice(&(n as u16).to_le_bytes());
            job.extend_from_slice(&s.to_le_bytes());
            out.extend(self.execute(&job, 1, n * SECTOR)?);
            s += n as u32;
            left -= n;
        }
        Ok(out)
    }

    fn write_sector(&mut self, sector: u32, data: &[u8]) -> Result<()> {
        let mut buf = data.to_vec();
        buf.resize(SECTOR, 0);
        self.pending_writes.insert(sector, buf);
        // 128 sectors = the whole 64 KB staging bank in one upload and one batch.
        if self.pending_writes.len() >= 128 {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pending_writes.is_empty() {
            return Ok(());
        }
        let writes = std::mem::take(&mut self.pending_writes);
        let mut staging = Vec::with_capacity(writes.len() * SECTOR);
        let mut jobs = Vec::new();
        let entries: Vec<(u32, &Vec<u8>)> = writes.iter().map(|(s, d)| (*s, d)).collect();
        // Runs of consecutive sectors go out as SD multi-sector writes (helper
        // jobs 05 first / 06 middle / 07 last -> $D680 commands $04/$05/$06),
        // which the card handles far faster than one command per sector.
        let multi = self.multi_write;
        let mut i = 0;
        while i < entries.len() {
            let mut run = 1;
            while i + run < entries.len() && entries[i + run].0 == entries[i].0 + run as u32 {
                run += 1;
            }
            for k in 0..run {
                let (sector, data) = entries[i + k];
                let addr = HELPER_STAGING[0] + ((i + k) as u32) * SECTOR as u32;
                staging.extend_from_slice(data);
                let job = if multi && run >= 4 {
                    if k == 0 {
                        0x05
                    } else if k == run - 1 {
                        0x07
                    } else {
                        0x06
                    }
                } else {
                    0x02
                };
                jobs.push(job);
                jobs.extend_from_slice(&addr.to_le_bytes());
                jobs.extend_from_slice(&sector.to_le_bytes());
            }
            i += run;
        }
        let bank = self.bank;
        let base = HELPER_STAGING[bank];
        // Patch the staging addresses for the chosen bank.
        for j in 0..writes.len() {
            let addr = base + (j as u32) * SECTOR as u32;
            jobs[j * 9 + 1..j * 9 + 5].copy_from_slice(&addr.to_le_bytes());
        }
        if self.pipeline {
            // Upload while the previous batch is still being written, then wait
            // for it before handing the helper the next job list.
            self.mon.write_memory(base, &staging)?;
            self.wait_inflight()?;
            self.mon.write_memory(HELPER_JOBS + 1, &jobs)?;
            self.mon.purge()?;
            self.mon.stray.clear();
            self.mon.write_raw(format!("sc000 {:x}\r", writes.len()).as_bytes())?;
            self.inflight = true;
            self.bank ^= 1;
        } else {
            self.mon.write_memory(base, &staging)?;
            self.execute(&jobs, writes.len(), 0)?;
        }
        Ok(())
    }

    fn preferred_batch(&self) -> usize {
        256
    }

    fn close(&mut self) -> Result<()> {
        self.flush()?;
        self.wait_inflight()
    }
}

impl HelperSd<'_> {
    /// Write consecutive sectors right now (used by the multi-write self-check).
    pub fn write_run_now(&mut self, first: u32, data: &[u8]) -> Result<()> {
        self.flush()?;
        for (i, chunk) in data.chunks(SECTOR).enumerate() {
            self.pending_writes.insert(first + i as u32, {
                let mut v = chunk.to_vec();
                v.resize(SECTOR, 0);
                v
            });
        }
        self.flush()?;
        self.wait_inflight()
    }

    /// Ask the helper to mount a D81 from the SD card root (helper job 0x12).
    pub fn mount_d81(&mut self, name: &str) -> Result<()> {
        self.flush()?;
        self.wait_inflight()?;
        let mut job = vec![0x12u8];
        // The helper wants PETSCII: swap letter case.
        for b in name.bytes().take(63) {
            job.push(if b.is_ascii_alphabetic() { b ^ 0x20 } else { b });
        }
        job.push(0);
        self.execute(&job, 1, 0)?;
        Ok(())
    }

    /// Tell the helper to exit back to BASIC (job 0xFF).
    pub fn quit(&mut self) -> Result<()> {
        self.flush()?;
        self.wait_inflight()?;
        self.mon.write_raw(b"sc001 ff\rsc000 1\r")?;
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------- cache

/// Write-back cache with read-ahead in front of a [`BlockDevice`].
pub struct SectorCache<D: BlockDevice> {
    pub dev: D,
    clean: HashMap<u32, Vec<u8>>,
    dirty: BTreeMap<u32, Vec<u8>>,
    pub read_ahead: usize,
}

impl<D: BlockDevice> SectorCache<D> {
    pub fn new(dev: D) -> Self {
        let read_ahead = dev.preferred_batch().max(1);
        Self { dev, clean: HashMap::new(), dirty: BTreeMap::new(), read_ahead }
    }

    pub fn read(&mut self, sector: u32) -> Result<Vec<u8>> {
        if let Some(d) = self.dirty.get(&sector) {
            return Ok(d.clone());
        }
        if let Some(d) = self.clean.get(&sector) {
            return Ok(d.clone());
        }
        self.read_many(sector, self.read_ahead)?;
        Ok(self.clean[&sector].clone())
    }

    /// Fetch `count` sectors (one batch) into the cache; dirty sectors are kept.
    pub fn read_many(&mut self, first: u32, count: usize) -> Result<()> {
        if self.clean.len() > 8192 {
            self.clean.clear();
        }
        let data = self.dev.read_sectors(first, count)?;
        for (i, chunk) in data.chunks(SECTOR).enumerate() {
            let s = first + i as u32;
            if !self.dirty.contains_key(&s) {
                self.clean.insert(s, chunk.to_vec());
            }
        }
        Ok(())
    }

    pub fn write(&mut self, sector: u32, data: &[u8]) -> Result<()> {
        let mut d = data.to_vec();
        d.resize(SECTOR, 0);
        if self.clean.get(&sector) == Some(&d) && !self.dirty.contains_key(&sector) {
            return Ok(());
        }
        self.clean.remove(&sector);
        self.dirty.insert(sector, d);
        if self.dirty.len() >= 128 {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let dirty = std::mem::take(&mut self.dirty);
        for (s, d) in dirty {
            self.dev.write_sector(s, &d)?;
            self.clean.insert(s, d);
        }
        self.dev.flush()
    }

    pub fn close(&mut self) -> Result<()> {
        self.flush()?;
        self.dev.close()
    }
}
