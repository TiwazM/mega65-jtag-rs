//! Minimal FAT32 implementation on top of a [`SectorCache`], enough for the
//! file operations mega65_ftp offers: list, get, put, delete, mkdir, rename.

use crate::sdcard::{BlockDevice, SectorCache, SECTOR};
use crate::{Error, Result};

const EOC: u32 = 0x0fff_fff8;
const ATTR_DIR: u8 = 0x10;
const ATTR_LFN: u8 = 0x0f;

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub short_name: String,
    pub attr: u8,
    pub cluster: u32,
    pub size: u32,
    pub mtime: String,
    /// Sector and 32-byte index of the 8.3 entry, and of the first LFN entry (if any).
    loc: (u32, usize),
    lfn_start: Option<(u32, usize)>,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.attr & ATTR_DIR != 0
    }
}

pub struct Partition {
    pub start: u32,
    pub size: u32,
    pub kind: u8,
}

pub struct Fat32<D: BlockDevice> {
    pub cache: SectorCache<D>,
    pub partitions: Vec<Partition>,
    pub part_start: u32,
    pub spc: u32,
    reserved: u32,
    pub spf: u32,
    pub root_cluster: u32,
    pub fsinfo: u32,
    first_data: u32,
    pub cwd: String,
    pub cwd_cluster: u32,
    /// Clusters allocated minus freed since the last FSInfo update.
    alloc_delta: i64,
    next_free_hint: u32,
}

fn u16at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl<D: BlockDevice> Fat32<D> {
    pub fn open(dev: D) -> Result<Self> {
        let mut cache = SectorCache::new(dev);
        let mbr = cache.read(0)?;
        let mut partitions = Vec::new();
        for i in 0..4 {
            let e = &mbr[0x1be + i * 16..0x1be + i * 16 + 16];
            if e[4] != 0 {
                partitions.push(Partition { start: u32at(e, 8), size: u32at(e, 12), kind: e[4] });
            }
        }
        let part_start = match partitions.iter().rev().find(|p| p.kind == 0x0b || p.kind == 0x0c) {
            Some(p) => p.start,
            None if &mbr[0x52..0x57] == b"FAT32" => 0,
            None => return Err(Error::Other("no FAT32 partition found in MBR".into())),
        };
        let vbr = cache.read(part_start)?;
        if vbr[510] != 0x55 || vbr[511] != 0xaa || u16at(&vbr, 11) != 512 {
            return Err(Error::Other("bad FAT32 volume boot record".into()));
        }
        let spc = vbr[13] as u32;
        let reserved = u16at(&vbr, 14) as u32;
        let nfats = vbr[16] as u32;
        let spf = u32at(&vbr, 0x24);
        let root_cluster = u32at(&vbr, 0x2c);
        let fsinfo = u16at(&vbr, 0x30) as u32;
        let first_data = reserved + nfats * spf;
        let mut fs = Self {
            cache,
            partitions,
            part_start,
            spc,
            reserved,
            spf,
            root_cluster,
            fsinfo,
            first_data,
            cwd: "/".into(),
            cwd_cluster: root_cluster,
            alloc_delta: 0,
            next_free_hint: 2,
        };
        if let Some((_, next)) = fs.fsinfo()? {
            if next >= 2 && next < fs.total_clusters() {
                fs.next_free_hint = next;
            }
        }
        Ok(fs)
    }

    pub fn close(&mut self) -> Result<()> {
        self.cache.close()
    }

    pub fn cluster_sector(&self, cluster: u32) -> u32 {
        self.part_start + self.first_data + (cluster - 2) * self.spc
    }

    // ------------------------------------------------------------- FAT

    fn fat_entry(&mut self, cluster: u32) -> Result<u32> {
        let s = self.part_start + self.reserved + cluster / 128;
        let sec = self.cache.read(s)?;
        Ok(u32at(&sec, (cluster as usize * 4) & 511) & 0x0fff_ffff)
    }

    fn set_fat_entry(&mut self, cluster: u32, value: u32) -> Result<()> {
        let s = self.part_start + self.reserved + cluster / 128;
        let mut sec = self.cache.read(s)?;
        let o = (cluster as usize * 4) & 511;
        let old = u32at(&sec, o) & 0x0fff_ffff;
        match (old == 0, value & 0x0fff_ffff == 0) {
            (true, false) => self.alloc_delta += 1,
            (false, true) => self.alloc_delta -= 1,
            _ => {}
        }
        let v = (u32at(&sec, o) & 0xf000_0000) | (value & 0x0fff_ffff);
        sec[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self.cache.write(s, &sec)?;
        self.cache.write(s + self.spf, &sec)
    }

    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>> {
        let n = self.fat_entry(cluster)?;
        Ok(if n >= EOC || n < 2 { None } else { Some(n) })
    }

    pub fn chain(&mut self, mut cluster: u32) -> Result<Vec<u32>> {
        let mut v = Vec::new();
        while cluster >= 2 && cluster < EOC {
            v.push(cluster);
            if v.len() > 1 << 22 {
                return Err(Error::Other("cluster chain loop".into()));
            }
            match self.next_cluster(cluster)? {
                Some(n) => cluster = n,
                None => break,
            }
        }
        Ok(v)
    }

    fn total_clusters(&self) -> u32 {
        self.spf * 128
    }

    fn find_free_cluster(&mut self, from: u32) -> Result<u32> {
        let total = self.total_clusters();
        let start = from.max(2).max(self.next_free_hint);
        for c in (start..total).chain(2..start) {
            if self.fat_entry(c)? == 0 {
                self.next_free_hint = c;
                return Ok(c);
            }
        }
        Err(Error::Other("SD card is full".into()))
    }

    fn free_chain(&mut self, cluster: u32) -> Result<()> {
        for c in self.chain(cluster)? {
            self.set_fat_entry(c, 0)?;
        }
        Ok(())
    }

    /// Read the FSInfo sector's (free count, next free hint), if valid.
    fn fsinfo(&mut self) -> Result<Option<(u32, u32)>> {
        if self.fsinfo == 0 {
            return Ok(None);
        }
        let sec = self.cache.read(self.part_start + self.fsinfo)?;
        if u32at(&sec, 0) != 0x4161_5252 || u32at(&sec, 0x1e4) != 0x6141_7272 {
            return Ok(None);
        }
        let free = u32at(&sec, 0x1e8);
        let next = u32at(&sec, 0x1ec);
        Ok(Some((free, next)))
    }

    /// Apply the net change in allocated clusters to the FSInfo free count.
    fn update_fsinfo(&mut self) -> Result<()> {
        let delta = std::mem::take(&mut self.alloc_delta);
        if delta == 0 {
            return Ok(());
        }
        let s = self.part_start + self.fsinfo;
        if let Some((free, _)) = self.fsinfo()? {
            let mut sec = self.cache.read(s)?;
            if free != 0xffff_ffff {
                let nf = (free as i64 - delta).clamp(0, self.total_clusters() as i64) as u32;
                sec[0x1e8..0x1ec].copy_from_slice(&nf.to_le_bytes());
            }
            sec[0x1ec..0x1f0].copy_from_slice(&self.next_free_hint.to_le_bytes());
            self.cache.write(s, &sec)?;
        }
        Ok(())
    }

    /// (free clusters, total clusters, bytes per cluster). Uses the FSInfo count
    /// unless `scan` is set, which walks the whole FAT (slow over serial).
    pub fn df(&mut self, scan: bool) -> Result<(u64, u64, u64)> {
        let total = self.total_clusters();
        if !scan {
            if let Some((free, _)) = self.fsinfo()? {
                if free != 0xffff_ffff && free <= total {
                    return Ok((free as u64, (total - 2) as u64, self.spc as u64 * SECTOR as u64));
                }
            }
        }
        let mut free = 0u64;
        let fat0 = self.part_start + self.reserved;
        for s in 0..self.spf {
            if s % 128 == 0 {
                self.cache.read_many(fat0 + s, (self.spf - s).min(128) as usize)?;
            }
            let sec = self.cache.read(fat0 + s)?;
            for i in 0..128 {
                let c = s * 128 + i as u32;
                if c >= 2 && c < total && u32at(&sec, i * 4) & 0x0fff_ffff == 0 {
                    free += 1;
                }
            }
        }
        // Store the count we just computed so the next df is instant.
        if self.fsinfo()?.is_some() {
            let s = self.part_start + self.fsinfo;
            let mut sec = self.cache.read(s)?;
            sec[0x1e8..0x1ec].copy_from_slice(&(free as u32).to_le_bytes());
            self.cache.write(s, &sec)?;
            self.cache.flush()?;
        }
        Ok((free, (total - 2) as u64, self.spc as u64 * SECTOR as u64))
    }

    // ------------------------------------------------------- directories

    /// Read all entries of the directory starting at `cluster`.
    pub fn read_dir(&mut self, cluster: u32) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        let mut lfn: Vec<(u8, String)> = Vec::new();
        let mut lfn_start = None;
        for c in self.chain(cluster)? {
            let base = self.cluster_sector(c);
            self.cache.read_many(base, self.spc as usize)?;
            for k in 0..self.spc {
                let s = base + k;
                let sec = self.cache.read(s)?;
                for i in 0..16 {
                    let e = &sec[i * 32..i * 32 + 32];
                    if e[0] == 0 {
                        return Ok(out);
                    }
                    if e[0] == 0xe5 {
                        lfn.clear();
                        lfn_start = None;
                        continue;
                    }
                    if e[11] == ATTR_LFN {
                        let seq = e[0] & 0x1f;
                        let mut part = String::new();
                        for &o in &[1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                            let u = u16at(e, o);
                            if u == 0 || u == 0xffff {
                                break;
                            }
                            part.push(char::from_u32(u as u32).unwrap_or('?'));
                        }
                        if lfn_start.is_none() {
                            lfn_start = Some((s, i));
                        }
                        lfn.push((seq, part));
                        continue;
                    }
                    let mut long = String::new();
                    if !lfn.is_empty() {
                        lfn.sort_by_key(|(seq, _)| *seq);
                        for (_, p) in &lfn {
                            long.push_str(p);
                        }
                    }
                    let short = short_name(e);
                    let attr = e[11];
                    let entry = DirEntry {
                        name: if long.is_empty() { short.clone() } else { long },
                        short_name: short,
                        attr,
                        cluster: (u16at(e, 0x1a) as u32) | (u16at(e, 0x14) as u32) << 16,
                        size: u32at(e, 0x1c),
                        mtime: fmt_time(u16at(e, 0x16), u16at(e, 0x18)),
                        loc: (s, i),
                        lfn_start,
                    };
                    lfn.clear();
                    lfn_start = None;
                    if attr & 0x08 != 0 {
                        continue; // volume label
                    }
                    out.push(entry);
                }
            }
        }
        Ok(out)
    }

    fn normalise(&self, path: &str) -> String {
        let mut parts: Vec<&str> = if path.starts_with('/') { Vec::new() } else { self.cwd.split('/').filter(|s| !s.is_empty()).collect() };
        for p in path.split('/') {
            match p {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        format!("/{}", parts.join("/"))
    }

    /// Resolve a path to its directory entry (None for the root).
    pub fn lookup(&mut self, path: &str) -> Result<Option<DirEntry>> {
        let full = self.normalise(path);
        let mut cluster = self.root_cluster;
        let mut last = None;
        for seg in full.split('/').filter(|s| !s.is_empty()) {
            let entries = self.read_dir(cluster)?;
            let e = entries
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(seg) || e.short_name.eq_ignore_ascii_case(seg))
                .ok_or_else(|| Error::Other(format!("{full}: not found")))?;
            cluster = if e.cluster == 0 { self.root_cluster } else { e.cluster };
            last = Some(e);
        }
        Ok(last)
    }

    /// Directory cluster for a path, which must be a directory.
    pub fn dir_cluster(&mut self, path: &str) -> Result<u32> {
        match self.lookup(path)? {
            None => Ok(self.root_cluster),
            Some(e) if e.is_dir() => Ok(if e.cluster == 0 { self.root_cluster } else { e.cluster }),
            Some(_) => Err(Error::Other(format!("{path}: not a directory"))),
        }
    }

    pub fn chdir(&mut self, path: &str) -> Result<()> {
        let c = self.dir_cluster(path)?;
        self.cwd = self.normalise(path);
        self.cwd_cluster = c;
        Ok(())
    }

    /// List a directory. Without `all`, entries starting with `.` (including `.`
    /// and `..`) and hidden ones are left out, as mega65_ftp does.
    pub fn list(&mut self, path: Option<&str>, all: bool) -> Result<Vec<DirEntry>> {
        let c = match path {
            Some(p) => self.dir_cluster(p)?,
            None => self.cwd_cluster,
        };
        let mut v = self.read_dir(c)?;
        if !all {
            v.retain(|e| !e.name.starts_with('.') && e.attr & 0x02 == 0);
        }
        v.sort_by(|a, b| b.is_dir().cmp(&a.is_dir()).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(v)
    }

    // ------------------------------------------------------------- files

    pub fn read_file(&mut self, path: &str, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>> {
        let e = self.lookup(path)?.ok_or_else(|| Error::Other("cannot read the root directory as a file".into()))?;
        if e.is_dir() {
            return Err(Error::Other(format!("{path}: is a directory")));
        }
        let mut out = Vec::with_capacity(e.size as usize);
        let chain = self.chain(e.cluster)?;
        let mut remaining = e.size as usize;
        // Read runs of contiguous clusters in one batch.
        let mut i = 0;
        while i < chain.len() && remaining > 0 {
            let mut j = i;
            while j + 1 < chain.len() && chain[j + 1] == chain[j] + 1 && (j - i + 2) * self.spc as usize <= 128 {
                j += 1;
            }
            let first = self.cluster_sector(chain[i]);
            let nsec = ((j - i + 1) * self.spc as usize).min(remaining.div_ceil(SECTOR));
            let data = self.cache.dev.read_sectors(first, nsec)?;
            let n = data.len().min(remaining);
            out.extend_from_slice(&data[..n]);
            remaining -= n;
            progress(out.len() as u64, e.size as u64);
            i = j + 1;
        }
        if remaining > 0 {
            return Err(Error::Other(format!("{path}: cluster chain ends before file size")));
        }
        Ok(out)
    }

    /// Write a file, replacing any existing one with the same name.
    pub fn write_file(&mut self, path: &str, data: &[u8], mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let full = self.normalise(path);
        let (dir, name) = full.rsplit_once('/').unwrap();
        let dir = if dir.is_empty() { "/" } else { dir };
        if name.is_empty() {
            return Err(Error::Other("empty file name".into()));
        }
        let dir_cluster = self.dir_cluster(dir)?;
        if let Some(old) = self.read_dir(dir_cluster)?.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)) {
            if old.is_dir() {
                return Err(Error::Other(format!("{full}: is a directory")));
            }
            self.remove_entry(&old)?;
        }
        let nclusters = (data.len() as u32).div_ceil(self.spc * SECTOR as u32).max(1);
        // Allocate the chain, preferring a contiguous run.
        let mut clusters = Vec::with_capacity(nclusters as usize);
        let mut c = self.next_free_hint;
        while (clusters.len() as u32) < nclusters {
            c = self.find_free_cluster(c)?;
            clusters.push(c);
            c += 1;
        }
        for (i, &c) in clusters.iter().enumerate() {
            let next = clusters.get(i + 1).copied().unwrap_or(0x0fff_ffff);
            self.set_fat_entry(c, next)?;
        }
        let entries = self.read_dir(dir_cluster)?;
        self.create_entry(dir_cluster, &entries, name, 0x20, clusters[0], data.len() as u32)?;
        // Data.
        let mut off = 0usize;
        for &c in &clusters {
            let base = self.cluster_sector(c);
            for k in 0..self.spc {
                if off >= data.len() {
                    break;
                }
                let end = (off + SECTOR).min(data.len());
                self.cache.write(base + k, &data[off..end])?;
                off = end;
                progress(off as u64, data.len() as u64);
            }
        }
        self.update_fsinfo()?;
        self.cache.flush()
    }

    pub fn delete(&mut self, path: &str, recursive: bool) -> Result<()> {
        let e = self.lookup(path)?.ok_or_else(|| Error::Other("cannot delete the root directory".into()))?;
        if e.is_dir() {
            let children: Vec<DirEntry> =
                self.read_dir(e.cluster)?.into_iter().filter(|c| c.short_name != "." && c.short_name != "..").collect();
            if !children.is_empty() {
                if !recursive {
                    return Err(Error::Other(format!("{path}: directory not empty")));
                }
                for c in children {
                    self.delete(&format!("{}/{}", self.normalise(path), c.name), true)?;
                }
            }
        }
        self.remove_entry(&e)?;
        self.update_fsinfo()?;
        self.cache.flush()
    }

    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        let full = self.normalise(path);
        let (dir, name) = full.rsplit_once('/').unwrap();
        let dir = if dir.is_empty() { "/" } else { dir };
        let parent = self.dir_cluster(dir)?;
        let entries = self.read_dir(parent)?;
        if entries.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
            return Err(Error::Other(format!("{full}: already exists")));
        }
        let c = self.find_free_cluster(2)?;
        self.set_fat_entry(c, 0x0fff_ffff)?;
        let base = self.cluster_sector(c);
        let mut sec = vec![0u8; SECTOR];
        let (t, d) = now_fat();
        let dot = |name: &[u8; 11], cl: u32| {
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(name);
            e[11] = ATTR_DIR;
            e[0x0e..0x10].copy_from_slice(&t.to_le_bytes());
            e[0x10..0x12].copy_from_slice(&d.to_le_bytes());
            e[0x16..0x18].copy_from_slice(&t.to_le_bytes());
            e[0x18..0x1a].copy_from_slice(&d.to_le_bytes());
            e[0x1a..0x1c].copy_from_slice(&(cl as u16).to_le_bytes());
            e[0x14..0x16].copy_from_slice(&((cl >> 16) as u16).to_le_bytes());
            e
        };
        sec[..32].copy_from_slice(&dot(b".          ", c));
        let pc = if parent == self.root_cluster { 0 } else { parent };
        sec[32..64].copy_from_slice(&dot(b"..         ", pc));
        self.cache.write(base, &sec)?;
        for k in 1..self.spc {
            self.cache.write(base + k, &[0u8; SECTOR])?;
        }
        self.create_entry(parent, &entries, name, ATTR_DIR, c, 0)?;
        self.update_fsinfo()?;
        self.cache.flush()
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let e = self.lookup(from)?.ok_or_else(|| Error::Other("cannot rename the root".into()))?;
        let full = self.normalise(to);
        let (dir, name) = full.rsplit_once('/').unwrap();
        let dir = if dir.is_empty() { "/" } else { dir };
        let dir_cluster = self.dir_cluster(dir)?;
        let entries = self.read_dir(dir_cluster)?;
        if entries.iter().any(|x| x.name.eq_ignore_ascii_case(name)) {
            return Err(Error::Other(format!("{full}: already exists")));
        }
        self.wipe_entry(&e)?;
        let entries = self.read_dir(dir_cluster)?;
        self.create_entry(dir_cluster, &entries, name, e.attr, e.cluster, e.size)?;
        self.cache.flush()
    }

    // ---------------------------------------------------- entry plumbing

    fn wipe_entry(&mut self, e: &DirEntry) -> Result<()> {
        let mut locs = Vec::new();
        if let Some(start) = e.lfn_start {
            // Walk from the LFN start to the 8.3 entry.
            let (mut s, mut i) = start;
            loop {
                locs.push((s, i));
                if (s, i) == e.loc {
                    break;
                }
                i += 1;
                if i == 16 {
                    i = 0;
                    s = self.next_dir_sector(s)?;
                }
            }
        } else {
            locs.push(e.loc);
        }
        for (s, i) in locs {
            let mut sec = self.cache.read(s)?;
            sec[i * 32] = 0xe5;
            self.cache.write(s, &sec)?;
        }
        Ok(())
    }

    fn remove_entry(&mut self, e: &DirEntry) -> Result<()> {
        self.wipe_entry(e)?;
        if e.cluster >= 2 {
            self.free_chain(e.cluster)?;
        }
        Ok(())
    }

    /// The sector after `s` within a directory, following the cluster chain.
    fn next_dir_sector(&mut self, s: u32) -> Result<u32> {
        let rel = s - self.part_start - self.first_data;
        let cluster = rel / self.spc + 2;
        if (rel + 1) % self.spc != 0 {
            return Ok(s + 1);
        }
        match self.next_cluster(cluster)? {
            Some(n) => Ok(self.cluster_sector(n)),
            None => Err(Error::Other("directory ends unexpectedly".into())),
        }
    }

    /// Create a directory entry (with LFN entries if needed) in `dir_cluster`.
    fn create_entry(&mut self, dir_cluster: u32, existing: &[DirEntry], name: &str, attr: u8, cluster: u32, size: u32) -> Result<()> {
        let (short, needs_lfn) = make_short_name(name, existing);
        let nlfn = if needs_lfn { name.chars().count().div_ceil(13) } else { 0 };
        let needed = nlfn + 1;
        // Find `needed` free consecutive slots.
        let mut slots: Vec<(u32, usize)> = Vec::new();
        let mut chain = self.chain(dir_cluster)?;
        'outer: loop {
            for &c in &chain {
                let base = self.cluster_sector(c);
                for k in 0..self.spc {
                    let s = base + k;
                    let sec = self.cache.read(s)?;
                    for i in 0..16 {
                        let b = sec[i * 32];
                        if b == 0 || b == 0xe5 {
                            slots.push((s, i));
                            if slots.len() == needed {
                                break 'outer;
                            }
                        } else {
                            slots.clear();
                        }
                    }
                }
            }
            // Grow the directory by one cluster.
            let last = *chain.last().unwrap();
            let n = self.find_free_cluster(last)?;
            self.set_fat_entry(n, 0x0fff_ffff)?;
            self.set_fat_entry(last, n)?;
            let base = self.cluster_sector(n);
            for k in 0..self.spc {
                self.cache.write(base + k, &[0u8; SECTOR])?;
            }
            chain.push(n);
            slots.clear();
        }
        let csum = lfn_checksum(&short);
        let chars: Vec<u16> = name.encode_utf16().collect();
        for (n, slot) in slots.iter().take(nlfn).enumerate() {
            let seq = nlfn - n;
            let mut e = [0xffu8; 32];
            e[0] = seq as u8 | if n == 0 { 0x40 } else { 0 };
            e[11] = ATTR_LFN;
            e[12] = 0;
            e[13] = csum;
            e[26] = 0;
            e[27] = 0;
            let offs = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
            for (j, &o) in offs.iter().enumerate() {
                let idx = (seq - 1) * 13 + j;
                let u = match chars.get(idx) {
                    Some(&u) => u,
                    None if idx == chars.len() => 0,
                    None => 0xffff,
                };
                e[o..o + 2].copy_from_slice(&u.to_le_bytes());
            }
            self.put_entry(*slot, &e)?;
        }
        let (t, d) = now_fat();
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(&short);
        e[11] = attr;
        e[0x0e..0x10].copy_from_slice(&t.to_le_bytes());
        e[0x10..0x12].copy_from_slice(&d.to_le_bytes());
        e[0x16..0x18].copy_from_slice(&t.to_le_bytes());
        e[0x18..0x1a].copy_from_slice(&d.to_le_bytes());
        e[0x12..0x14].copy_from_slice(&d.to_le_bytes());
        e[0x1a..0x1c].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[0x14..0x16].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[0x1c..0x20].copy_from_slice(&size.to_le_bytes());
        self.put_entry(slots[nlfn], &e)
    }

    fn put_entry(&mut self, (s, i): (u32, usize), e: &[u8; 32]) -> Result<()> {
        let mut sec = self.cache.read(s)?;
        sec[i * 32..i * 32 + 32].copy_from_slice(e);
        self.cache.write(s, &sec)
    }

    /// Hex dump helper used by the `sector` command.
    pub fn read_sector(&mut self, sector: u32) -> Result<Vec<u8>> {
        self.cache.read(sector)
    }
}

fn short_name(e: &[u8]) -> String {
    let base: String = e[..8].iter().map(|&b| b as char).collect::<String>().trim_end().to_string();
    let ext: String = e[8..11].iter().map(|&b| b as char).collect::<String>().trim_end().to_string();
    let base = if e[12] & 0x08 != 0 { base.to_lowercase() } else { base };
    let ext = if e[12] & 0x10 != 0 { ext.to_lowercase() } else { ext };
    let base = if base.starts_with('\u{5}') { format!("\u{e5}{}", &base[1..]) } else { base };
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

fn fmt_time(t: u16, d: u16) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        1980 + (d >> 9),
        (d >> 5) & 0xf,
        d & 0x1f,
        t >> 11,
        (t >> 5) & 0x3f,
        (t & 0x1f) * 2
    )
}

fn now_fat() -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let date = (((now.year() - 1980).max(0) as u16) << 9) | ((now.month() as u16) << 5) | now.day() as u16;
    let time = ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2);
    (time, date)
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &c| ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c))
}

/// Build an 8.3 name for `name`, unique among `existing`. Returns (name, needs_lfn).
fn make_short_name(name: &str, existing: &[DirEntry]) -> ([u8; 11], bool) {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) if !b.is_empty() => (b, e),
        _ => (name, ""),
    };
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '.')
            .map(|c| if c.is_ascii_alphanumeric() || "$%'-_@~`!(){}^#&".contains(c) { c.to_ascii_uppercase() } else { '_' })
            .collect()
    };
    let cb = clean(base);
    let ce = clean(ext);
    let lossy = cb.len() != base.len() || ce.len() != ext.len() || name.matches('.').count() > 1;
    let fits = cb.len() <= 8 && ce.len() <= 3 && !lossy;
    let needs_lfn = !fits || name != name.to_ascii_uppercase() && name != name.to_ascii_lowercase() || name.chars().any(|c| c.is_lowercase());
    let mut out = [b' '; 11];
    let ext_part: Vec<u8> = ce.bytes().take(3).collect();
    out[8..8 + ext_part.len()].copy_from_slice(&ext_part);
    if fits {
        out[..cb.len()].copy_from_slice(cb.as_bytes());
        return (out, needs_lfn);
    }
    for n in 1..100_000u32 {
        let suffix = format!("~{n}");
        let keep = (8 - suffix.len()).min(cb.len());
        let mut b = cb.as_bytes()[..keep].to_vec();
        b.extend_from_slice(suffix.as_bytes());
        out[..8].fill(b' ');
        out[..b.len()].copy_from_slice(&b);
        let candidate = short_name(&{
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(&out);
            e
        });
        if !existing.iter().any(|e| e.short_name.eq_ignore_ascii_case(&candidate)) {
            return (out, true);
        }
    }
    (out, true)
}
