//! Decoder for MEGA65 DMAgic job lists (F018A 11-byte and F018B 12-byte jobs,
//! enhanced-mode option bytes, chained lists), as found in memory at the
//! address a program writes to $D700-$D705.
//!
//! A job whose command byte has bit 2 set is followed by another list, which
//! may itself start with option bytes (the MEGA65 user guide: "Chain, i.e.,
//! another DMA list follows").

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaOption {
    pub code: u8,
    pub arg: Option<u8>,
}

impl DmaOption {
    pub fn describe(&self) -> String {
        let a = self.arg.unwrap_or(0);
        match self.code {
            0x06 => "transparency off".into(),
            0x07 => "transparency on".into(),
            0x0a => "F018A job format".into(),
            0x0b => "F018B job format".into(),
            0x80 => format!("source megabyte ${a:02X}"),
            0x81 => format!("destination megabyte ${a:02X}"),
            0x82 => format!("source skip fraction {a}/256"),
            0x83 => format!("source skip {a} bytes"),
            0x84 => format!("destination skip fraction {a}/256"),
            0x85 => format!("destination skip {a} bytes"),
            0x86 => format!("transparent value ${a:02X}"),
            0x87 => format!("dest line: X column bytes low ${a:02X}"),
            0x88 => format!("dest line: X column bytes high ${a:02X}"),
            0x89 => format!("dest line: Y row bytes low ${a:02X}"),
            0x8a => format!("dest line: Y row bytes high ${a:02X}"),
            0x8b => format!("dest line: slope low ${a:02X}"),
            0x8c => format!("dest line: slope high ${a:02X}"),
            0x8d => format!("dest line: slope accumulator low ${a:02X}"),
            0x8e => format!("dest line: slope accumulator high ${a:02X}"),
            0x8f => format!(
                "dest line mode {} (dir {}, slope {})",
                if a & 0x80 != 0 { "ON" } else { "off" },
                if a & 0x40 != 0 { "Y" } else { "X" },
                if a & 0x20 != 0 { "negative" } else { "positive" }
            ),
            0x97..=0x9f => format!("source line option ${:02X} = ${a:02X}", self.code),
            c => format!("option ${c:02X}{}", self.arg.map(|v| format!(" = ${v:02X}")).unwrap_or_default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaJob {
    pub offset: usize,
    /// Option bytes that preceded this job (the first job's, or a chained list's).
    pub options: Vec<DmaOption>,
    pub command: u8,
    pub count: u16,
    pub src: u32,
    pub src_flags: u8,
    pub dst: u32,
    pub dst_flags: u8,
    pub modulo: u16,
    pub chained: bool,
}

impl DmaJob {
    pub fn kind(&self) -> &'static str {
        match self.command & 3 {
            0 => "COPY",
            1 => "MIX",
            2 => "SWAP",
            _ => "FILL",
        }
    }

    /// BANK/FLAGS byte: bit 4 hold, 5 modulo, 6 direction (decrement), 7 I/O visible.
    fn flags(f: u8) -> String {
        let mut v = Vec::new();
        if f & 0x10 != 0 {
            v.push("hold");
        }
        if f & 0x20 != 0 {
            v.push("modulo");
        }
        if f & 0x40 != 0 {
            v.push("backwards");
        }
        if f & 0x80 != 0 {
            v.push("io-visible");
        }
        v.join(",")
    }

    pub fn describe(&self) -> String {
        let mut s = if self.command & 3 == 3 {
            format!("{:4} {:6} bytes  value ${:02X} -> ${:07X}", self.kind(), self.count, self.src & 0xff, self.dst)
        } else {
            format!("{:4} {:6} bytes  ${:07X} -> ${:07X}", self.kind(), self.count, self.src, self.dst)
        };
        let sf = Self::flags(self.src_flags);
        let df = Self::flags(self.dst_flags);
        if !sf.is_empty() {
            s.push_str(&format!("  src:{sf}"));
        }
        if !df.is_empty() {
            s.push_str(&format!("  dst:{df}"));
        }
        if self.command & 8 != 0 {
            s.push_str("  yield");
        }
        if self.modulo != 0 {
            s.push_str(&format!("  modulo {}", self.modulo));
        }
        if self.chained {
            s.push_str("  (chained)");
        }
        s
    }
}

#[derive(Debug, Clone, Default)]
pub struct DmaList {
    pub jobs: Vec<DmaJob>,
    pub total_bytes: u64,
    /// True if the list ran past the end of the supplied bytes.
    pub truncated: bool,
}

/// Does a list start with enhanced option bytes here? Job command bytes are
/// $00-$0F and overlap the $06/$07/$0A/$0B options, so look at what follows.
fn starts_with_options(bytes: &[u8]) -> bool {
    let is_opt = |b: u8| b == 0x0a || b == 0x0b || (0x80..=0x9f).contains(&b);
    match bytes.first().copied() {
        Some(b) if is_opt(b) => true,
        Some(0x06) | Some(0x07) => bytes.get(1).map(|&n| is_opt(n) || n == 0).unwrap_or(false),
        _ => false,
    }
}

/// Decode a list starting at `bytes[0]`. `f018b` is the controller's default
/// job format (the $D703 bit 0 setting) when a list sets no $0A/$0B option.
pub fn decode(bytes: &[u8], f018b: bool) -> DmaList {
    let mut list = DmaList::default();
    let mut p = 0usize;
    let mut fmt_b = f018b;
    let (mut src_mb, mut dst_mb) = (0u32, 0u32);
    loop {
        let mut options = Vec::new();
        if starts_with_options(&bytes[p.min(bytes.len())..]) {
            while p < bytes.len() {
                let code = bytes[p];
                p += 1;
                if code == 0 {
                    break;
                }
                let takes_arg = (0x80..=0x9f).contains(&code);
                let arg = if takes_arg { bytes.get(p).copied() } else { None };
                if takes_arg {
                    p += 1;
                }
                match (code, arg) {
                    (0x0a, _) => fmt_b = false,
                    (0x0b, _) => fmt_b = true,
                    (0x80, Some(v)) => src_mb = v as u32,
                    (0x81, Some(v)) => dst_mb = v as u32,
                    _ => {}
                }
                options.push(DmaOption { code, arg });
            }
        }
        let need = if fmt_b { 12 } else { 11 };
        if p + need > bytes.len() {
            list.truncated = true;
            break;
        }
        let j = &bytes[p..p + need];
        let modulo = if fmt_b { u16::from_le_bytes([j[10], j[11]]) } else { u16::from_le_bytes([j[9], j[10]]) };
        let job = DmaJob {
            offset: p,
            options,
            command: j[0],
            count: u16::from_le_bytes([j[1], j[2]]),
            src: (src_mb << 20) | ((j[5] as u32 & 0xf) << 16) | u16::from_le_bytes([j[3], j[4]]) as u32,
            src_flags: j[5] & 0xf0,
            dst: (dst_mb << 20) | ((j[8] as u32 & 0xf) << 16) | u16::from_le_bytes([j[6], j[7]]) as u32,
            dst_flags: j[8] & 0xf0,
            modulo,
            chained: j[0] & 4 != 0,
        };
        list.total_bytes += job.count as u64;
        let chained = job.chained;
        list.jobs.push(job);
        p += need;
        if !chained || list.jobs.len() > 256 {
            break;
        }
    }
    list
}

impl DmaList {
    pub fn describe(&self, base: u32) -> String {
        let mut s = String::new();
        for (i, j) in self.jobs.iter().enumerate() {
            if i == 0 && j.options.is_empty() {
                s.push_str("  (no option bytes: job format from $D703)\n");
            }
            for o in &j.options {
                s.push_str(&format!("  option ${:02X}: {}\n", o.code, o.describe()));
            }
            s.push_str(&format!("${:07X}: {}\n", base + j.offset as u32, j.describe()));
        }
        s.push_str(&format!(
            "{} job(s), {} bytes total{}\n",
            self.jobs.len(),
            self.total_bytes,
            if self.truncated { " (list runs past the decoded bytes)" } else { "" }
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f018b_with_options_and_chained_dummy_list() {
        // From Space Commander: a transparent blit from attic RAM, chained to a
        // second enhanced list holding a 1-byte dummy copy.
        let bytes = [
            0x0b, 0x07, 0x86, 0x00, 0x80, 0x80, 0x81, 0x00, 0x82, 0x00, 0x83, 0x01, 0x84, 0x00, 0x85, 0x01, 0x00, //
            0x04, 0x08, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, //
            0x0b, 0x06, 0x80, 0x00, 0x81, 0x00, 0x00, //
            0x00, 0x01, 0x00, 0x3f, 0x90, 0x00, 0x3f, 0x90, 0x00, 0x00, 0x00, 0x00,
        ];
        let l = decode(&bytes, true);
        assert_eq!(l.jobs.len(), 2);
        assert_eq!(l.jobs[0].options.len(), 9);
        assert_eq!((l.jobs[0].count, l.jobs[0].src, l.jobs[0].dst), (8, 0x8024000, 0x40000));
        assert!(l.jobs[0].chained);
        assert_eq!(l.jobs[1].options.len(), 4);
        assert_eq!((l.jobs[1].count, l.jobs[1].src, l.jobs[1].dst), (1, 0x903f, 0x903f));
        assert!(!l.jobs[1].chained);
        assert_eq!(l.total_bytes, 9);
        assert!(!l.truncated);
        assert!(l.describe(0x900f).contains("transparency on"));
    }

    #[test]
    fn plain_f018b_job_and_flags() {
        let bytes = [0x00, 0x58, 0x02, 0x00, 0x18, 0x01, 0x00, 0xc0, 0x10, 0x00, 0x00, 0x20];
        let l = decode(&bytes, true);
        assert_eq!(l.jobs.len(), 1);
        assert_eq!((l.jobs[0].count, l.jobs[0].src, l.jobs[0].dst, l.jobs[0].modulo), (600, 0x11800, 0xc000, 8192));
        assert!(l.jobs[0].describe().contains("dst:hold"));
    }
}
