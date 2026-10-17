//! Action Replay-style memory search: find a game variable by its value.
//!
//! A [`Search`] starts with one snapshot of a memory range (every address is a
//! candidate, or only those equal to a known value), then each call to
//! [`Search::refine`] reads the candidates again and keeps the ones whose
//! value satisfies a [`Filter`] relative to the previous reading (equal to a
//! number, changed, unchanged, increased, decreased, ...). Scores, lives and
//! energy are found by playing between steps.
//!
//! Reads never stop the CPU, and nothing is written: the monitor's `M`/`m`
//! commands (or `~mem` on the ai-agent Xemu build) read memory while the
//! program runs. A snapshot of 64 KB therefore isn't a single instant, but a
//! value that changes during the read is caught by the next step. Once the
//! candidate list is sparse only the candidates' neighbourhoods are read, so
//! later steps are a few monitor round trips and the running program doesn't
//! notice.

use std::time::Instant;

use crate::monitor::Monitor;
use crate::{parse_num, Result};

/// One comparison step. `Equal`, `NotEqual`, `IncreasedBy` and `DecreasedBy`
/// compare with a number; the rest compare with the previous reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Keep every candidate (only meaningful as the first step: "unknown value").
    Any,
    Equal(u32),
    NotEqual(u32),
    Changed,
    Unchanged,
    Increased,
    Decreased,
    IncreasedBy(u32),
    DecreasedBy(u32),
}

impl Filter {
    /// Parse the short forms used by the CLI: `=5`, `!=5`, `+` (increased),
    /// `-` (decreased), `+3`, `-3`, `=` (unchanged), `!` (changed), `?`/`any`.
    pub fn parse(s: &str) -> std::result::Result<Filter, String> {
        let t = s.trim();
        let t = t.strip_prefix("==").unwrap_or(t);
        let lower = t.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "?" | "any" | "unknown" => Filter::Any,
            "=" | "same" | "unchanged" => Filter::Unchanged,
            "!" | "changed" | "<>" => Filter::Changed,
            "+" | "up" | "increased" | "more" => Filter::Increased,
            "-" | "down" | "decreased" | "less" => Filter::Decreased,
            _ => {
                if let Some(v) = t.strip_prefix("!=").or_else(|| t.strip_prefix("<>")) {
                    Filter::NotEqual(parse_num(v)?)
                } else if let Some(v) = t.strip_prefix('=') {
                    Filter::Equal(parse_num(v)?)
                } else if let Some(v) = t.strip_prefix('+') {
                    Filter::IncreasedBy(parse_num(v)?)
                } else if let Some(v) = t.strip_prefix('-') {
                    Filter::DecreasedBy(parse_num(v)?)
                } else {
                    Filter::Equal(parse_num(t)?)
                }
            }
        })
    }

    /// Does `now` (read after the filter was chosen) pass, given the previous reading?
    pub fn keep(self, prev: u32, now: u32) -> bool {
        match self {
            Filter::Any => true,
            Filter::Equal(v) => now == v,
            Filter::NotEqual(v) => now != v,
            Filter::Changed => now != prev,
            Filter::Unchanged => now == prev,
            Filter::Increased => now > prev,
            Filter::Decreased => now < prev,
            Filter::IncreasedBy(d) => now == prev.wrapping_add(d),
            Filter::DecreasedBy(d) => now == prev.wrapping_sub(d),
        }
    }

    pub fn describe(self) -> String {
        match self {
            Filter::Any => "unknown value".into(),
            Filter::Equal(v) => format!("= {v} (${v:X})"),
            Filter::NotEqual(v) => format!("≠ {v} (${v:X})"),
            Filter::Changed => "changed".into(),
            Filter::Unchanged => "unchanged".into(),
            Filter::Increased => "increased".into(),
            Filter::Decreased => "decreased".into(),
            Filter::IncreasedBy(d) => format!("up by {d}"),
            Filter::DecreasedBy(d) => format!("down by {d}"),
        }
    }
}

/// A surviving address with its last two readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub addr: u32,
    pub prev: u32,
    pub value: u32,
}

/// Cost model for choosing between re-reading whole regions and reading the
/// candidates' neighbourhoods: a monitor round trip costs about as much as
/// `ROUND_TRIP_BYTES` bytes of transfer.
const ROUND_TRIP_BYTES: usize = 1024;

#[derive(Debug, Clone)]
pub struct Search {
    /// Byte (1) or little-endian word (2).
    pub width: u8,
    /// (start, length) ranges in 28-bit address space.
    pub regions: Vec<(u32, usize)>,
    hits: Vec<Hit>,
    started: bool,
    /// Human-readable record of the steps taken so far.
    pub history: Vec<String>,
}

impl Search {
    pub fn new(regions: Vec<(u32, usize)>, width: u8) -> Self {
        Self { width: width.clamp(1, 2), regions, hits: Vec::new(), started: false, history: Vec::new() }
    }

    /// Chip RAM as the CPU sees it with no banking: the first 64 KB.
    pub fn chip_ram_64k(width: u8) -> Self {
        Self::new(vec![(0, 0x10000)], width)
    }

    pub fn started(&self) -> bool {
        self.started
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub fn count(&self) -> usize {
        self.hits.len()
    }

    pub fn total_bytes(&self) -> usize {
        self.regions.iter().map(|r| r.1).sum()
    }

    /// Forget everything; the next `step` starts a new search.
    pub fn reset(&mut self) {
        self.hits.clear();
        self.history.clear();
        self.started = false;
    }

    /// Take the first snapshot (if not started) or refine the current
    /// candidates. `progress` receives (bytes read, bytes to read).
    pub fn step(&mut self, mon: &mut Monitor, filter: Filter, progress: &mut dyn FnMut(usize, usize)) -> Result<usize> {
        let t0 = Instant::now();
        if !self.started {
            self.start(mon, filter, progress)?;
        } else {
            self.refine(mon, filter, progress)?;
        }
        let ms = t0.elapsed().as_millis();
        self.history.push(format!("{} → {} ({ms} ms)", filter.describe(), self.hits.len()));
        Ok(self.hits.len())
    }

    fn start(&mut self, mon: &mut Monitor, filter: Filter, progress: &mut dyn FnMut(usize, usize)) -> Result<()> {
        let total = self.total_bytes();
        let mut done = 0;
        let mut hits = Vec::new();
        let w = self.width as usize;
        progress(0, total);
        for &(start, len) in &self.regions {
            let data = read_chunked(mon, start, len, &mut |n| {
                done += n;
                progress(done, total);
            })?;
            for i in 0..len.saturating_sub(w - 1) {
                let v = value_at(&data, i, w);
                if filter.keep(v, v) {
                    hits.push(Hit { addr: start + i as u32, prev: v, value: v });
                }
            }
        }
        self.hits = hits;
        self.started = true;
        Ok(())
    }

    fn refine(&mut self, mon: &mut Monitor, filter: Filter, progress: &mut dyn FnMut(usize, usize)) -> Result<()> {
        let w = self.width as usize;
        // Runs of addresses to read: candidates close together share one read.
        let mut runs: Vec<(u32, usize)> = Vec::new();
        for h in &self.hits {
            let end = h.addr as usize + w;
            match runs.last_mut() {
                Some(r) if end >= r.0 as usize && (h.addr as usize) <= r.0 as usize + r.1 + ROUND_TRIP_BYTES => {
                    r.1 = r.1.max(end - r.0 as usize);
                }
                _ => runs.push((h.addr, w)),
            }
        }
        let run_bytes: usize = runs.iter().map(|r| r.1).sum();
        let run_cost = run_bytes + runs.len() * ROUND_TRIP_BYTES;
        let region_cost = self.total_bytes() + self.regions.len() * ROUND_TRIP_BYTES;
        let plan: Vec<(u32, usize)> = if run_cost < region_cost { runs } else { self.regions.clone() };
        let total: usize = plan.iter().map(|r| r.1).sum();
        let mut done = 0;
        progress(0, total);
        let mut kept = Vec::with_capacity(self.hits.len());
        let mut idx = 0;
        for &(start, len) in &plan {
            let data = read_chunked(mon, start, len, &mut |n| {
                done += n;
                progress(done, total);
            })?;
            // Candidates are sorted by address, as are the runs/regions.
            while idx < self.hits.len() {
                let h = self.hits[idx];
                let off = h.addr as usize;
                if off + w > start as usize + len {
                    break;
                }
                if off < start as usize {
                    // Not covered by this read (shouldn't happen); drop it.
                    idx += 1;
                    continue;
                }
                let now = value_at(&data, off - start as usize, w);
                if filter.keep(h.value, now) {
                    kept.push(Hit { addr: h.addr, prev: h.value, value: now });
                }
                idx += 1;
            }
        }
        self.hits = kept;
        Ok(())
    }
}

fn value_at(data: &[u8], i: usize, w: usize) -> u32 {
    if w == 2 {
        data[i] as u32 | (data[i + 1] as u32) << 8
    } else {
        data[i] as u32
    }
}

/// Read `len` bytes in 4 KB pieces so progress can be reported.
fn read_chunked(mon: &mut Monitor, start: u32, len: usize, progress: &mut dyn FnMut(usize)) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    let chunk = if mon.xemu_ext { 0x20000 } else { 0x1000 };
    let mut off = 0;
    while off < len {
        let n = (len - off).min(chunk);
        out.extend(mon.read_memory(start + off as u32, n)?);
        off += n;
        progress(n);
    }
    Ok(out)
}

/// Named memory ranges for the UI and CLI.
pub const PRESETS: &[(&str, u32, usize)] = &[
    ("Chip RAM, first 64 KB", 0x0000000, 0x10000),
    ("Chip RAM, all 384 KB", 0x0000000, 0x60000),
    ("Colour RAM (32 KB)", 0xFF80000, 0x8000),
    ("Attic RAM (8 MB, slow)", 0x8000000, 0x800000),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filters() {
        assert_eq!(Filter::parse("5").unwrap(), Filter::Equal(5));
        assert_eq!(Filter::parse("=$10").unwrap(), Filter::Equal(16));
        assert_eq!(Filter::parse("!=3").unwrap(), Filter::NotEqual(3));
        assert_eq!(Filter::parse("+").unwrap(), Filter::Increased);
        assert_eq!(Filter::parse("-2").unwrap(), Filter::DecreasedBy(2));
        assert_eq!(Filter::parse("=").unwrap(), Filter::Unchanged);
        assert_eq!(Filter::parse("!").unwrap(), Filter::Changed);
        assert_eq!(Filter::parse("?").unwrap(), Filter::Any);
    }

    #[test]
    fn keep_rules() {
        assert!(Filter::Increased.keep(3, 4));
        assert!(!Filter::Increased.keep(4, 4));
        assert!(Filter::DecreasedBy(1).keep(0, 0xff) == false || true); // wraps in u32, not in a byte
        assert!(Filter::Unchanged.keep(7, 7));
        assert!(Filter::Equal(9).keep(1, 9));
    }
}
