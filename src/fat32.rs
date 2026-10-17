//! Minimal FAT32 implementation on top of a [`SectorCache`], enough for the
//! file operations mega65_ftp offers: list, get, put, delete, mkdir, rename.

use std::collections::HashSet;

use crate::sdcard::{BlockDevice, SectorCache, SECTOR};
use crate::{Error, Result};

const EOC: u32 = 0x0fff_fff8;
const ATTR_DIR: u8 = 0x10;
const ATTR_LFN: u8 = 0x0f;

/// How [`Fat32::write_file_with`] picks the clusters of a new file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Allocation {
    /// One unbroken run of clusters if the card has one, otherwise free
    /// clusters in order from the FSInfo next-free hint.
    #[default]
    PreferContiguous,
    /// One unbroken run of clusters, or fail without touching the card. Disk
    /// images (D81 etc.) need this: the Hypervisor mounts them by start sector.
    Contiguous,
    /// Free clusters in order from the next-free hint, even if a run exists
    /// (how uploads worked before contiguous allocation was added).
    Any,
}

impl Allocation {
    /// The default for a file name: `Contiguous` for disk images, which the
    /// MEGA65 cannot mount when fragmented, `PreferContiguous` for the rest.
    pub fn for_name(name: &str) -> Self {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        if matches!(ext.as_str(), "d81" | "d64" | "d71" | "d65") {
            Allocation::Contiguous
        } else {
            Allocation::PreferContiguous
        }
    }
}

/// Where a new directory entry will go: its slots, and the cluster to add to
/// the directory first if it has no room (with the directory's last cluster),
/// and end-of-directory markers before the entry that must become "deleted"
/// so directory readers still reach it.
struct EntryPlan {
    slots: Vec<(u32, usize)>,
    grow: Option<(u32, u32)>,
    unterminate: Vec<(u32, usize)>,
}

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
    /// One past the last cluster that exists on the volume.
    end_cluster: u32,
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
        // The last FAT sector usually has entries past the end of the volume;
        // they read as free, so never hand them out.
        let total_sectors = match u16at(&vbr, 0x13) {
            0 => u32at(&vbr, 0x20),
            n => n as u32,
        };
        let mut end_cluster = spf * 128;
        if spc > 0 && total_sectors > first_data {
            end_cluster = end_cluster.min((total_sectors - first_data) / spc + 2);
        }
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
            end_cluster,
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
        self.end_cluster
    }

    /// FAT entry lookup for scans: keeps the current FAT sector in `last`.
    fn fat_entry_scan(&mut self, last: &mut Option<(u32, Vec<u8>)>, cluster: u32) -> Result<u32> {
        let s = self.part_start + self.reserved + cluster / 128;
        if last.as_ref().map(|(ls, _)| *ls) != Some(s) {
            *last = Some((s, self.cache.read(s)?));
        }
        let sec = &last.as_ref().unwrap().1;
        Ok(u32at(sec, (cluster as usize * 4) & 511) & 0x0fff_ffff)
    }

    /// Free for this operation: unused in the FAT or in `reusable` (the chain
    /// of a file about to be replaced), and not already promised (`taken`).
    fn is_free(&mut self, last: &mut Option<(u32, Vec<u8>)>, c: u32, reusable: &HashSet<u32>, taken: &HashSet<u32>) -> Result<bool> {
        if taken.contains(&c) {
            return Ok(false);
        }
        if reusable.contains(&c) {
            return Ok(true);
        }
        Ok(self.fat_entry_scan(last, c)? == 0)
    }

    /// Choose `n` clusters for a file. Reads the FAT but writes nothing, so a
    /// failure here leaves the filesystem as it was.
    fn plan_clusters(&mut self, n: u32, alloc: Allocation, reusable: &HashSet<u32>, taken: &HashSet<u32>) -> Result<Vec<u32>> {
        match alloc {
            Allocation::Any => self.plan_any(n, reusable, taken),
            Allocation::Contiguous => self.plan_run(n, reusable, taken)?.map_err(|longest| {
                Error::Other(format!("no run of {n} contiguous free clusters on the SD card (longest is {longest})"))
            }),
            Allocation::PreferContiguous => match self.plan_run(n, reusable, taken)? {
                Ok(v) => Ok(v),
                Err(_) => self.plan_any(n, reusable, taken),
            },
        }
    }

    fn hint(&self) -> u32 {
        if (2..self.total_clusters()).contains(&self.next_free_hint) {
            self.next_free_hint
        } else {
            2
        }
    }

    /// The first `n` free clusters from the next-free hint, wrapping round.
    fn plan_any(&mut self, n: u32, reusable: &HashSet<u32>, taken: &HashSet<u32>) -> Result<Vec<u32>> {
        let (end, hint) = (self.total_clusters(), self.hint());
        let mut last = None;
        let mut out = Vec::with_capacity(n as usize);
        for c in (hint..end).chain(2..hint) {
            if self.is_free(&mut last, c, reusable, taken)? {
                out.push(c);
                if out.len() as u32 == n {
                    return Ok(out);
                }
            }
        }
        Err(Error::Other(format!("SD card is full: need {n} clusters, {} free", out.len())))
    }

    /// The first run of `n` free clusters, or the longest run seen if there is
    /// none. Like mega65_ftp's find_contiguous_clusters, except that it starts
    /// at the next-free hint (free space is usually there, and each FAT sector
    /// costs a serial round trip) and then searches from cluster 2 up to the
    /// hint, so a run straddling the hint is found too.
    fn plan_run(&mut self, n: u32, reusable: &HashSet<u32>, taken: &HashSet<u32>) -> Result<std::result::Result<Vec<u32>, u32>> {
        let (end, hint) = (self.total_clusters(), self.hint());
        let mut last = None;
        let mut longest = 0;
        for (from, to) in [(hint, end), (2, hint.saturating_add(n - 1).min(end))] {
            let (mut start, mut len) = (from, 0);
            for c in from..to {
                if self.is_free(&mut last, c, reusable, taken)? {
                    if len == 0 {
                        start = c;
                    }
                    len += 1;
                    longest = longest.max(len);
                    if len == n {
                        return Ok(Ok((start..start + n).collect()));
                    }
                } else {
                    len = 0;
                }
            }
            if hint == 2 {
                break;
            }
        }
        Ok(Err(longest))
    }

    /// Link `clusters` into a chain ending in end-of-chain.
    fn link_chain(&mut self, clusters: &[u32]) -> Result<()> {
        for (i, &c) in clusters.iter().enumerate() {
            let next = clusters.get(i + 1).copied().unwrap_or(0x0fff_ffff);
            self.set_fat_entry(c, next)?;
        }
        // Move the hint on if we just used the cluster it pointed at.
        let hint = self.hint();
        if clusters.contains(&hint) {
            let after = clusters.last().unwrap() + 1;
            self.next_free_hint = if after < self.total_clusters() { after } else { 2 };
        }
        Ok(())
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

    /// Write a file, replacing any existing one with the same name, with the
    /// allocation [`Allocation::for_name`] picks for it.
    pub fn write_file(&mut self, path: &str, data: &[u8], progress: impl FnMut(u64, u64)) -> Result<()> {
        let alloc = Allocation::for_name(path);
        self.write_file_with(path, data, alloc, progress)
    }

    /// Write a file, replacing any existing one with the same name.
    ///
    /// The clusters and the directory slot are chosen before anything is
    /// written, so if there is no room (or no contiguous run, with
    /// [`Allocation::Contiguous`]) the filesystem, including any file being
    /// replaced, is left exactly as it was. Only a device error part-way
    /// through can leave a partial write.
    pub fn write_file_with(&mut self, path: &str, data: &[u8], alloc: Allocation, mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let full = self.normalise(path);
        let (dir, name) = full.rsplit_once('/').unwrap();
        let dir = if dir.is_empty() { "/" } else { dir };
        if name.is_empty() {
            return Err(Error::Other("empty file name".into()));
        }
        let dir_cluster = self.dir_cluster(dir)?;
        let mut entries = self.read_dir(dir_cluster)?;
        let old = entries.iter().position(|e| e.name.eq_ignore_ascii_case(name)).map(|i| entries.remove(i));
        // The old file's clusters and directory slots are free for the new one.
        let mut reusable = HashSet::new();
        let mut freed = HashSet::new();
        if let Some(old) = &old {
            if old.is_dir() {
                return Err(Error::Other(format!("{full}: is a directory")));
            }
            if old.cluster >= 2 {
                reusable.extend(self.chain(old.cluster)?);
            }
            freed.extend(self.entry_locs(old)?);
        }
        let bytes_per_cluster = self.spc as u64 * SECTOR as u64;
        let nclusters = (data.len() as u64).div_ceil(bytes_per_cluster).max(1);
        if nclusters >= self.total_clusters() as u64 {
            return Err(Error::Other(format!("{full}: too big for the SD card")));
        }
        let clusters = self.plan_clusters(nclusters as u32, alloc, &reusable, &HashSet::new())?;
        let taken: HashSet<u32> = clusters.iter().copied().collect();
        let (short, nlfn) = self.entry_names(name, &entries);
        let plan = self.plan_entry(dir_cluster, nlfn + 1, &freed, &reusable, &taken)?;

        // Everything is decided; only device errors can stop us from here on.
        if let Some(old) = &old {
            self.remove_entry(old)?;
        }
        self.link_chain(&clusters)?;
        self.commit_entry(plan, &short, nlfn, name, 0x20, clusters[0], data.len() as u32)?;
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
        self.cache.flush()?;
        // The FAT must now hold exactly the chain we planned.
        if self.chain(clusters[0])? != clusters {
            return Err(Error::Other(format!("{full}: FAT chain does not match what was written")));
        }
        Ok(())
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
        let none = HashSet::new();
        let c = self.plan_any(1, &none, &none)?[0];
        let (short, nlfn) = self.entry_names(name, &entries);
        let plan = self.plan_entry(parent, nlfn + 1, &HashSet::new(), &none, &HashSet::from([c]))?;
        self.link_chain(&[c])?;
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
        self.commit_entry(plan, &short, nlfn, name, ATTR_DIR, c, 0)?;
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
        let others: Vec<DirEntry> = entries.into_iter().filter(|x| x.loc != e.loc).collect();
        let (short, nlfn) = self.entry_names(name, &others);
        let freed: HashSet<(u32, usize)> = self.entry_locs(&e)?.into_iter().collect();
        let none = HashSet::new();
        let plan = self.plan_entry(dir_cluster, nlfn + 1, &freed, &none, &none)?;
        self.wipe_entry(&e)?;
        self.commit_entry(plan, &short, nlfn, name, e.attr, e.cluster, e.size)?;
        self.update_fsinfo()?;
        self.cache.flush()
    }

    // ---------------------------------------------------- entry plumbing

    /// The directory slots an entry occupies: its LFN entries and the 8.3 entry.
    fn entry_locs(&mut self, e: &DirEntry) -> Result<Vec<(u32, usize)>> {
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
        Ok(locs)
    }

    fn wipe_entry(&mut self, e: &DirEntry) -> Result<()> {
        for (s, i) in self.entry_locs(e)? {
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

    /// The 8.3 name for `name` and how many LFN entries it needs.
    fn entry_names(&self, name: &str, existing: &[DirEntry]) -> ([u8; 11], usize) {
        let (short, needs_lfn) = make_short_name(name, existing);
        (short, if needs_lfn { name.chars().count().div_ceil(13) } else { 0 })
    }

    /// Find `needed` consecutive free slots in a directory, counting `freed`
    /// slots (an entry about to be removed) as free. If there are none, plan
    /// a new cluster for the directory. Writes nothing.
    fn plan_entry(
        &mut self,
        dir_cluster: u32,
        needed: usize,
        freed: &HashSet<(u32, usize)>,
        reusable: &HashSet<u32>,
        taken: &HashSet<u32>,
    ) -> Result<EntryPlan> {
        let mut slots: Vec<(u32, usize)> = Vec::new();
        let mut zeros: Vec<(u32, usize)> = Vec::new();
        let chain = self.chain(dir_cluster)?;
        for &c in &chain {
            let base = self.cluster_sector(c);
            for k in 0..self.spc {
                let s = base + k;
                let sec = self.cache.read(s)?;
                for i in 0..16 {
                    let b = sec[i * 32];
                    if b == 0 {
                        zeros.push((s, i));
                    }
                    if b == 0 || b == 0xe5 || freed.contains(&(s, i)) {
                        slots.push((s, i));
                        if slots.len() == needed {
                            zeros.retain(|z| !slots.contains(z));
                            return Ok(EntryPlan { slots, grow: None, unterminate: zeros });
                        }
                    } else {
                        slots.clear();
                    }
                }
            }
        }
        // Grow the directory by one cluster and put the whole entry in it.
        if needed > 16 * self.spc as usize {
            return Err(Error::Other("name too long for one directory cluster".into()));
        }
        let n = self.plan_any(1, reusable, taken)?[0];
        let base = self.cluster_sector(n);
        let slots = (0..needed).map(|i| (base + i as u32 / 16, i % 16)).collect();
        Ok(EntryPlan { slots, grow: Some((*chain.last().unwrap(), n)), unterminate: zeros })
    }

    /// Write a directory entry (with LFN entries if needed) where `plan` says.
    #[allow(clippy::too_many_arguments)]
    fn commit_entry(&mut self, plan: EntryPlan, short: &[u8; 11], nlfn: usize, name: &str, attr: u8, cluster: u32, size: u32) -> Result<()> {
        if let Some((last, n)) = plan.grow {
            let base = self.cluster_sector(n);
            for k in 0..self.spc {
                self.cache.write(base + k, &[0u8; SECTOR])?;
            }
            self.set_fat_entry(n, 0x0fff_ffff)?;
            self.set_fat_entry(last, n)?;
        }
        for (s, i) in plan.unterminate {
            let mut sec = self.cache.read(s)?;
            sec[i * 32] = 0xe5;
            self.cache.write(s, &sec)?;
        }
        let slots = plan.slots;
        let csum = lfn_checksum(short);
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
        e[..11].copy_from_slice(short);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const SPC: u32 = 1;
    const RESERVED: u32 = 32;
    const SPF: u32 = 17;
    /// Clusters 2..END exist; the FAT has room for 17 * 128 = 2176 entries.
    const END: u32 = 2000;
    const BPC: usize = SPC as usize * SECTOR;

    /// A disk image in memory, shared so a test can reopen or snapshot it.
    #[derive(Clone)]
    struct MemDisk(Rc<RefCell<Vec<u8>>>);

    impl BlockDevice for MemDisk {
        fn read_sectors(&mut self, first: u32, count: usize) -> Result<Vec<u8>> {
            let img = self.0.borrow();
            let a = (first as usize * SECTOR).min(img.len());
            let b = ((first as usize + count) * SECTOR).min(img.len());
            let mut v = img[a..b].to_vec();
            v.resize(count * SECTOR, 0);
            Ok(v)
        }
        fn write_sector(&mut self, sector: u32, data: &[u8]) -> Result<()> {
            let mut img = self.0.borrow_mut();
            let a = sector as usize * SECTOR;
            assert!(a + SECTOR <= img.len(), "write past the end of the image: sector {sector}");
            img[a..a + SECTOR].copy_from_slice(data);
            Ok(())
        }
        fn preferred_batch(&self) -> usize {
            16
        }
    }

    /// A blank FAT32 volume without an MBR: cluster 2 is the root directory.
    fn blank() -> MemDisk {
        let first_data = RESERVED + 2 * SPF;
        let total = first_data + (END - 2) * SPC;
        let mut img = vec![0u8; total as usize * SECTOR];
        let v = &mut img[..SECTOR];
        v[11..13].copy_from_slice(&512u16.to_le_bytes());
        v[13] = SPC as u8;
        v[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
        v[16] = 2;
        v[0x20..0x24].copy_from_slice(&total.to_le_bytes());
        v[0x24..0x28].copy_from_slice(&SPF.to_le_bytes());
        v[0x2c..0x30].copy_from_slice(&2u32.to_le_bytes());
        v[0x30..0x32].copy_from_slice(&1u16.to_le_bytes());
        v[0x52..0x5a].copy_from_slice(b"FAT32   ");
        v[510] = 0x55;
        v[511] = 0xaa;
        let fsi = &mut img[SECTOR..2 * SECTOR];
        fsi[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        fsi[0x1e4..0x1e8].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        fsi[0x1e8..0x1ec].copy_from_slice(&(END - 3).to_le_bytes());
        fsi[0x1ec..0x1f0].copy_from_slice(&3u32.to_le_bytes());
        for fat in [RESERVED, RESERVED + SPF] {
            let o = fat as usize * SECTOR;
            img[o..o + 4].copy_from_slice(&0x0fff_fff8u32.to_le_bytes());
            img[o + 4..o + 8].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
            img[o + 8..o + 12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
        }
        MemDisk(Rc::new(RefCell::new(img)))
    }

    fn data(clusters: usize, seed: u8) -> Vec<u8> {
        (0..clusters * BPC - 7).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    fn put(fs: &mut Fat32<MemDisk>, name: &str, clusters: usize, alloc: Allocation) -> Result<Vec<u8>> {
        let d = data(clusters, name.len() as u8);
        fs.write_file_with(name, &d, alloc, |_, _| {})?;
        Ok(d)
    }

    /// Check a file's chain ends in end-of-chain, has the right length, stays
    /// on the volume, reads back, and that both FAT copies and the FSInfo free
    /// count agree. Reopens the image so nothing comes from the cache.
    fn verify(disk: &MemDisk, path: &str, want: &[u8]) -> Vec<u32> {
        let mut fs = Fat32::open(disk.clone()).unwrap();
        let e = fs.lookup(path).unwrap().expect("file exists");
        assert_eq!(e.size as usize, want.len());
        let chain = fs.chain(e.cluster).unwrap();
        assert_eq!(chain.len(), want.len().div_ceil(BPC).max(1), "chain length of {path}");
        assert!(fs.fat_entry(*chain.last().unwrap()).unwrap() >= EOC, "{path} chain must end in end-of-chain");
        assert!(chain.iter().all(|&c| (2..END).contains(&c)), "{path} uses a cluster off the volume: {chain:?}");
        let unique: HashSet<u32> = chain.iter().copied().collect();
        assert_eq!(unique.len(), chain.len(), "{path} chain repeats a cluster");
        assert_eq!(fs.read_file(path, |_, _| {}).unwrap(), want, "{path} contents");
        let img = disk.0.borrow();
        let fat = |n: u32| &img[(RESERVED + n * SPF) as usize * SECTOR..(RESERVED + (n + 1) * SPF) as usize * SECTOR];
        assert!(fat(0) == fat(1), "FAT copies differ");
        drop(img);
        let (counted, _, _) = fs.df(false).unwrap();
        let (scanned, _, _) = fs.df(true).unwrap();
        assert_eq!(counted, scanned, "FSInfo free count");
        chain
    }

    fn is_run(chain: &[u32]) -> bool {
        chain.windows(2).all(|w| w[1] == w[0] + 1)
    }

    /// Mark the clusters `used` picks as allocated (one-cluster orphan chains),
    /// which is all the allocator sees of other files.
    fn occupy(disk: &MemDisk, used: impl Fn(u32) -> bool) -> Fat32<MemDisk> {
        let mut fs = Fat32::open(disk.clone()).unwrap();
        for c in (3..END).filter(|&c| used(c)) {
            if fs.fat_entry(c).unwrap() == 0 {
                fs.set_fat_entry(c, 0x0fff_ffff).unwrap();
            }
        }
        fs.update_fsinfo().unwrap();
        fs.cache.flush().unwrap();
        fs
    }

    /// Free space in single-cluster holes at 4, 6, 8, ... from `from` on.
    fn checkerboard(disk: &MemDisk, from: u32) -> Fat32<MemDisk> {
        occupy(disk, |c| c < from || c % 2 == 1)
    }

    fn snapshot(disk: &MemDisk) -> Vec<u8> {
        disk.0.borrow().clone()
    }

    #[test]
    fn default_is_contiguous_when_a_run_exists() {
        let disk = blank();
        // Holes at 4, 6, ..., 400, then free space from 400 on.
        let mut fs = occupy(&disk, |c| c < 400 && c % 2 == 1);
        // Point the hint at the holes: the old allocator would weave through them.
        let first_hole = 4;
        fs.next_free_hint = first_hole;
        let d = data(10, 1);
        fs.write_file("/BIG.PRG", &d, |_, _| {}).unwrap();
        fs.close().unwrap();
        let chain = verify(&disk, "/BIG.PRG", &d);
        assert!(is_run(&chain), "{chain:?}");
        assert_eq!(chain[0], 400, "should skip the holes: {chain:?}");
    }

    #[test]
    fn any_keeps_the_old_hint_order() {
        let disk = blank();
        let mut fs = occupy(&disk, |c| c < 400 && c % 2 == 1);
        let first_hole = 4;
        fs.next_free_hint = first_hole;
        let d = put(&mut fs, "/BIG.PRG", 10, Allocation::Any).unwrap();
        fs.close().unwrap();
        let chain = verify(&disk, "/BIG.PRG", &d);
        let holes: Vec<u32> = (0..10).map(|i| first_hole + 2 * i).collect();
        assert_eq!(chain, holes);
    }

    /// The whole volume as a checkerboard, so no run of 2 exists.
    fn full_checkerboard(disk: &MemDisk) -> Fat32<MemDisk> {
        let mut fs = checkerboard(disk, 3);
        let longest = fs.plan_run(2, &HashSet::new(), &HashSet::new()).unwrap();
        assert_eq!(longest, Err(1), "test setup should leave no run of 2");
        fs
    }

    #[test]
    fn contiguous_fails_cleanly_on_fragmented_space() {
        let disk = blank();
        let mut fs = full_checkerboard(&disk);
        let old = put(&mut fs, "/KEEP.TXT", 1, Allocation::Any).unwrap();
        fs.close().unwrap();
        let before = snapshot(&disk);
        let mut fs = Fat32::open(disk.clone()).unwrap();
        let err = put(&mut fs, "/NEW.PRG", 3, Allocation::Contiguous).unwrap_err();
        assert!(err.to_string().contains("no run of 3 contiguous"), "{err}");
        // Replacing an existing file must not lose it either.
        assert!(put(&mut fs, "/KEEP.TXT", 3, Allocation::Contiguous).is_err());
        fs.close().unwrap();
        assert!(snapshot(&disk) == before, "a failed allocation changed the image");
        verify(&disk, "/KEEP.TXT", &old);
    }

    #[test]
    fn disk_images_default_to_contiguous() {
        assert_eq!(Allocation::for_name("/GAMES/Elite.D81"), Allocation::Contiguous);
        assert_eq!(Allocation::for_name("x.d64"), Allocation::Contiguous);
        assert_eq!(Allocation::for_name("PROG.PRG"), Allocation::PreferContiguous);
        assert_eq!(Allocation::for_name("README"), Allocation::PreferContiguous);
        let disk = blank();
        let mut fs = full_checkerboard(&disk);
        fs.close().unwrap();
        let before = snapshot(&disk);
        let mut fs = Fat32::open(disk.clone()).unwrap();
        assert!(fs.write_file("/DISK.D81", &data(4, 0), |_, _| {}).is_err());
        fs.close().unwrap();
        assert!(snapshot(&disk) == before);
    }

    #[test]
    fn prefer_falls_back_to_a_fragmented_chain() {
        let disk = blank();
        let mut fs = full_checkerboard(&disk);
        let d = put(&mut fs, "/NEW.PRG", 5, Allocation::PreferContiguous).unwrap();
        fs.close().unwrap();
        let chain = verify(&disk, "/NEW.PRG", &d);
        assert!(!is_run(&chain));
    }

    #[test]
    fn full_card_fails_cleanly_and_keeps_the_old_file() {
        let disk = blank();
        let mut fs = full_checkerboard(&disk);
        let old = put(&mut fs, "/KEEP.TXT", 2, Allocation::Any).unwrap();
        let free = fs.df(true).unwrap().0 as usize;
        fs.close().unwrap();
        let before = snapshot(&disk);
        let mut fs = Fat32::open(disk.clone()).unwrap();
        // The old file's 2 clusters count as free, but that is still too few.
        let err = put(&mut fs, "/KEEP.TXT", free + 3, Allocation::Any).unwrap_err();
        assert!(err.to_string().contains("full"), "{err}");
        fs.close().unwrap();
        assert!(snapshot(&disk) == before);
        verify(&disk, "/KEEP.TXT", &old);
    }

    #[test]
    fn replacing_a_file_can_reuse_its_own_run() {
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        put(&mut fs, "/IMG.D81", 30, Allocation::Contiguous).unwrap();
        fs.close().unwrap();
        let mut fs = full_checkerboard(&disk);
        let d = data(30, 9);
        fs.write_file("/IMG.D81", &d, |_, _| {}).unwrap();
        fs.close().unwrap();
        assert!(is_run(&verify(&disk, "/IMG.D81", &d)));
    }

    #[test]
    fn finds_a_run_that_straddles_the_hint() {
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        put(&mut fs, "/A", 100, Allocation::Any).unwrap(); // 3..103
        put(&mut fs, "/HOLE", 10, Allocation::Any).unwrap(); // 103..113
        let rest = END as usize - 113;
        put(&mut fs, "/B", rest, Allocation::Any).unwrap(); // up to the end
        fs.delete("/HOLE", false).unwrap();
        fs.next_free_hint = 108;
        let d = put(&mut fs, "/RUN", 8, Allocation::Contiguous).unwrap();
        fs.close().unwrap();
        assert_eq!(verify(&disk, "/RUN", &d), (103..111).collect::<Vec<_>>());
    }

    #[test]
    fn never_allocates_past_the_end_of_the_volume() {
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        fs.next_free_hint = END - 5;
        let d = put(&mut fs, "/A", 10, Allocation::Contiguous).unwrap();
        let e = put(&mut fs, "/B", 10, Allocation::Any).unwrap();
        fs.close().unwrap();
        assert_eq!(verify(&disk, "/A", &d), (3..13).collect::<Vec<_>>());
        let b = verify(&disk, "/B", &e);
        assert_eq!(&b[..5], &[END - 5, END - 4, END - 3, END - 2, END - 1]);
    }

    #[test]
    fn directory_growth_does_not_collide_with_the_run() {
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        // 16 slots per cluster: fill the root so the next entry needs a new cluster.
        for i in 0..16 {
            put(&mut fs, &format!("/F{i}"), 1, Allocation::Any).unwrap();
        }
        let d = put(&mut fs, "/A long name.prg", 20, Allocation::Contiguous).unwrap();
        fs.close().unwrap();
        let chain = verify(&disk, "/A long name.prg", &d);
        assert!(is_run(&chain));
        let mut fs = Fat32::open(disk.clone()).unwrap();
        let root = fs.chain(2).unwrap();
        assert_eq!(root.len(), 2);
        assert!(!chain.contains(&root[1]));
        for i in 0..16 {
            assert!(fs.lookup(&format!("/F{i}")).unwrap().is_some());
        }
    }

    #[test]
    fn no_room_for_the_directory_entry_leaves_the_image_unchanged() {
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        for i in 0..16 {
            put(&mut fs, &format!("/F{i}"), 1, Allocation::Any).unwrap();
        }
        // Leave exactly enough clusters for the file but none for the directory.
        let free = fs.df(true).unwrap().0 as usize;
        put(&mut fs, "/F0", 1 + free - 10, Allocation::Any).unwrap();
        fs.close().unwrap();
        let before = snapshot(&disk);
        let mut fs = Fat32::open(disk.clone()).unwrap();
        assert_eq!(fs.df(false).unwrap().0, 10);
        assert!(put(&mut fs, "/NEW", 10, Allocation::Contiguous).is_err());
        assert!(put(&mut fs, "/NEW", 10, Allocation::Any).is_err());
        fs.close().unwrap();
        assert!(snapshot(&disk) == before);
        // mkdir needs a cluster for itself and one for the root: give it one.
        let mut fs = Fat32::open(disk.clone()).unwrap();
        put(&mut fs, "/F0", 1 + free - 1, Allocation::Any).unwrap();
        fs.close().unwrap();
        let before = snapshot(&disk);
        let mut fs = Fat32::open(disk.clone()).unwrap();
        assert!(fs.mkdir("/NEWDIR").is_err());
        fs.close().unwrap();
        assert!(snapshot(&disk) == before);
    }

    #[test]
    fn mkdir_rename_and_replace_still_work() {
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        fs.mkdir("/Sub dir").unwrap();
        let d = put(&mut fs, "/Sub dir/Some File.txt", 3, Allocation::PreferContiguous).unwrap();
        fs.rename("/Sub dir/Some File.txt", "/Sub dir/Renamed with a long name.txt").unwrap();
        let d2 = put(&mut fs, "/Sub dir/Renamed with a long name.txt", 5, Allocation::PreferContiguous).unwrap();
        fs.close().unwrap();
        assert_ne!(d, d2);
        verify(&disk, "/Sub dir/Renamed with a long name.txt", &d2);
        let mut fs = Fat32::open(disk.clone()).unwrap();
        assert!(fs.lookup("/Sub dir/Some File.txt").is_err());
        assert_eq!(fs.list(Some("/Sub dir"), false).unwrap().len(), 1);
    }

    #[test]
    fn growing_a_directory_keeps_every_entry_visible() {
        // Long names take 3 slots, so the root's 16 slots end in a free gap
        // that the next entry does not fit in; that gap must not end the directory.
        let disk = blank();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        fs.mkdir("/Small files").unwrap();
        for i in 0..40 {
            put(&mut fs, &format!("/Small files/file number {i}.bin"), 1, Allocation::Any).unwrap();
        }
        for i in (0..40).step_by(3) {
            fs.delete(&format!("/Small files/file number {i}.bin"), false).unwrap();
        }
        fs.close().unwrap();
        let mut fs = Fat32::open(disk.clone()).unwrap();
        assert_eq!(fs.list(Some("/Small files"), false).unwrap().len(), 40 - 14);
        drop(fs);
        verify(&disk, "/Small files/file number 38.bin", &data(1, "/Small files/file number 38.bin".len() as u8));
    }
}
