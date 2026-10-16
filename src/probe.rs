//! Heuristics that look for interesting bytes in a running program, such as a
//! frame counter or buffer-flip flag for the profiler's frame-rate graph.

use std::time::{Duration, Instant};

use crate::monitor::Monitor;
use crate::Result;

#[derive(Debug, Clone)]
pub struct FlagCandidate {
    pub addr: u32,
    /// Changes per second measured with fast single-byte sampling.
    pub rate: f32,
    /// Distinct values seen during the fast sampling.
    pub distinct: usize,
    /// True if every change was +1 (a counter).
    pub counter: bool,
    /// Number of consecutive bytes merged into this entry (1 = a single byte).
    pub span: u32,
}

impl FlagCandidate {
    pub fn describe(&self) -> String {
        let kind = if self.counter {
            "counter"
        } else if self.distinct == 2 {
            "toggle"
        } else {
            "variable"
        };
        let a = if self.addr & 0xfff_0000 == 0x777_0000 { self.addr & 0xffff } else { self.addr };
        if self.span > 1 {
            format!("${a:04X}-${:04X}  {:5.1}/s  {kind}s ({} bytes)", a + self.span - 1, self.rate, self.span)
        } else {
            format!("${a:04X}  {:5.1}/s  {kind} ({} values)", self.rate, self.distinct)
        }
    }
}

/// Find bytes in `[start, end)` that change once per frame. Phase 1 reads the
/// range `passes` times and keeps bytes that changed between every pass;
/// phase 2 samples each survivor for `sample_ms` and keeps rates between
/// `min_rate` and `max_rate` changes per second. `progress` is called with
/// (done, total) units of work. The CPU is never stopped.
pub fn find_frame_flags(
    mon: &mut Monitor,
    start: u32,
    end: u32,
    passes: usize,
    sample_ms: u64,
    min_rate: f32,
    max_rate: f32,
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<FlagCandidate>> {
    let len = (end - start) as usize;
    let passes = passes.max(2);
    let mut keep = vec![true; len];
    let mut prev = mon.read_memory(start, len)?;
    progress(1, passes + 1);
    for p in 1..passes {
        let cur = mon.read_memory(start, len)?;
        for i in 0..len {
            if cur[i] == prev[i] {
                keep[i] = false;
            }
        }
        prev = cur;
        progress(p + 1, passes + 1);
    }
    // Skip I/O and the stack page: registers change constantly and aren't flags.
    let mut candidates: Vec<u32> = (0..len)
        .filter(|&i| keep[i])
        .map(|i| start + i as u32)
        .filter(|a| !(0xd000..0xe000).contains(&(a & 0xffff)) && (a & 0xff00) != 0x0100)
        .collect();
    candidates.truncate(250);
    let total = passes + 1 + candidates.len();
    let mut out = Vec::new();
    mon.sync()?;
    for (k, &a) in candidates.iter().enumerate() {
        let t0 = Instant::now();
        let (mut changes, mut last) = (0u32, None::<u8>);
        let mut seen = std::collections::BTreeSet::new();
        let mut counter = true;
        while t0.elapsed() < Duration::from_millis(sample_ms) {
            let v = mon.peek_fast(a)?;
            if let Some(l) = last {
                if l != v {
                    changes += 1;
                    if v != l.wrapping_add(1) {
                        counter = false;
                    }
                }
            }
            seen.insert(v);
            last = Some(v);
        }
        let rate = changes as f32 / t0.elapsed().as_secs_f32();
        if rate >= min_rate && rate <= max_rate {
            out.push(FlagCandidate { addr: a, rate, distinct: seen.len(), counter: counter && changes > 2, span: 1 });
        }
        progress(passes + 1 + k + 1, total);
    }
    // Merge runs of neighbouring bytes of the same kind and rate (tables of
    // per-object flags) into one entry, so single flags stand out.
    out.sort_by_key(|c| c.addr);
    let mut merged: Vec<FlagCandidate> = Vec::new();
    for c in out {
        if let Some(last) = merged.last_mut() {
            let same_kind = last.counter == c.counter && (last.distinct == 2) == (c.distinct == 2);
            if same_kind && c.addr <= last.addr + last.span + 2 && (c.rate - last.rate).abs() < 3.0 {
                last.span = c.addr - last.addr + 1;
                continue;
            }
        }
        merged.push(c);
    }
    let mut out = merged;
    // Most useful first: single toggles and counters, then by how "frame-like" the rate is.
    out.sort_by(|x, y| {
        let score = |c: &FlagCandidate| ((c.span > 2) as u8, if c.distinct == 2 || c.counter { 0 } else { 1 }, (c.rate - 60.0).abs() as i32);
        score(x).cmp(&score(y))
    });
    Ok(out)
}
