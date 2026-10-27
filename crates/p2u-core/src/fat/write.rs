//! Writing a playlist straight into a FAT volume, bypassing the OS driver, so
//! that both the FAT order of the folder's entries AND the placement of every
//! file's data are chosen here:
//!
//! * entries are written in playlist order;
//! * every file's clusters are allocated as one unbroken run, files back to
//!   back in playlist order, from the lowest free block large enough
//!   (or, only if asked, strictly ascending but fragmented);
//! * the result is re-read from the medium: entry order, each file's cluster
//!   chain against the planned layout, and every byte against the source.
//!
//! Crash safety. Writes happen in a fixed order so that pulling the stick at
//! any point leaves, at worst, clusters marked used that nothing references
//! (which fsck reclaims). Nothing is ever cross-linked, and no file is ever
//! visible before its data and chain are complete:
//!
//! 1. Deletions (only when replacing): mark the old entries deleted, THEN free
//!    their clusters in the allocation table.
//! 2. File data and the new folder's table go into free clusters (nothing
//!    references them yet).
//! 3. The allocation table chains for those clusters (every FAT copy).
//! 4. Commit: the folder's entry is added to its parent table, or, for the top
//!    level, the table is rewritten in two passes (entries that move are first
//!    marked deleted, then written in their new places), so an interruption
//!    can lose an entry but never duplicate one.
//! 5. FSInfo free-cluster count.
//!
//! Before each phase writes anything, the bytes of every metadata sector it
//! will touch are saved to a backup file; if that fails, nothing is written.

use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::dir::{parse_directory, sfn_checksum, DirEntryGroup, DirListing, EntryKind, ENTRY};
use super::reorder::normalize_name;
use super::{Backup, Device, DirRef, Extent, FatError, FatType, Result, Volume};

/// One sector to write: where, its bytes before, its bytes after.
type SectorWrite = (Extent, Vec<u8>, Vec<u8>);

// ---------------------------------------------------------------------------
// The allocation table, held in memory while a write is planned.

/// One copy of the File Allocation Table, loaded whole.
#[derive(Clone)]
pub struct FatTable {
    data: Vec<u8>,
    orig: Vec<u8>,
    fat_type: FatType,
    max_cluster: u32,
}

impl FatTable {
    pub fn get(&self, n: u32) -> u32 {
        let d = &self.data;
        match self.fat_type {
            FatType::Fat32 => {
                let o = n as usize * 4;
                u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) & 0x0FFF_FFFF
            }
            FatType::Fat16 => {
                let o = n as usize * 2;
                u16::from_le_bytes([d[o], d[o + 1]]) as u32
            }
            FatType::Fat12 => {
                let o = n as usize + n as usize / 2;
                let v = u16::from_le_bytes([d[o], d[o + 1]]) as u32;
                if n & 1 == 1 {
                    v >> 4
                } else {
                    v & 0x0FFF
                }
            }
        }
    }

    pub fn set(&mut self, n: u32, v: u32) {
        let d = &mut self.data;
        match self.fat_type {
            FatType::Fat32 => {
                let o = n as usize * 4;
                // The top 4 bits are reserved and must be preserved.
                let old = u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
                let new = (old & 0xF000_0000) | (v & 0x0FFF_FFFF);
                d[o..o + 4].copy_from_slice(&new.to_le_bytes());
            }
            FatType::Fat16 => {
                let o = n as usize * 2;
                d[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
            }
            FatType::Fat12 => {
                let o = n as usize + n as usize / 2;
                if n & 1 == 1 {
                    d[o] = (d[o] & 0x0F) | (((v & 0x0F) as u8) << 4);
                    d[o + 1] = ((v >> 4) & 0xFF) as u8;
                } else {
                    d[o] = (v & 0xFF) as u8;
                    d[o + 1] = (d[o + 1] & 0xF0) | (((v >> 8) & 0x0F) as u8);
                }
            }
        }
    }

    /// End-of-chain marker written for new chains.
    pub fn eoc(&self) -> u32 {
        match self.fat_type {
            FatType::Fat12 => 0x0FFF,
            FatType::Fat16 => 0xFFFF,
            FatType::Fat32 => 0x0FFF_FFFF,
        }
    }

    fn eoc_min(&self) -> u32 {
        match self.fat_type {
            FatType::Fat12 => 0x0FF8,
            FatType::Fat16 => 0xFFF8,
            FatType::Fat32 => 0x0FFF_FFF8,
        }
    }

    pub fn is_free(&self, n: u32) -> bool {
        self.get(n) == 0
    }

    pub fn free_count(&self) -> u32 {
        (2..=self.max_cluster).filter(|&c| self.is_free(c)).count() as u32
    }

    /// Maximal runs of free clusters as (first cluster, length), ascending.
    pub fn free_runs(&self) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut start = None;
        for c in 2..=self.max_cluster {
            match (self.is_free(c), start) {
                (true, None) => start = Some(c),
                (false, Some(s)) => {
                    runs.push((s, c - s));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            runs.push((s, self.max_cluster + 1 - s));
        }
        runs
    }

    /// Free runs that avoid the given clusters.
    pub fn free_runs_excluding(&self, avoid: &HashSet<u32>) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut start = None;
        for c in 2..=self.max_cluster {
            match (self.is_free(c) && !avoid.contains(&c), start) {
                (true, None) => start = Some(c),
                (false, Some(s)) => {
                    runs.push((s, c - s));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            runs.push((s, self.max_cluster + 1 - s));
        }
        runs
    }

    /// Follow a chain in this (in-memory) table.
    pub fn chain(&self, start: u32) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut c = start;
        loop {
            if c < 2 || c > self.max_cluster || !seen.insert(c) {
                return Err(FatError::Corrupt(format!("bad cluster chain from {start}")));
            }
            out.push(c);
            let n = self.get(c);
            if n >= self.eoc_min() {
                return Ok(out);
            }
            c = n;
        }
    }

    /// Indexes of sectors that differ from the table as loaded.
    fn changed_sectors(&self, bps: usize) -> Vec<usize> {
        (0..self.data.len() / bps)
            .filter(|&s| self.data[s * bps..(s + 1) * bps] != self.orig[s * bps..(s + 1) * bps])
            .collect()
    }

    /// Accept the current contents as the on-disk state (after writing them).
    fn commit(&mut self) {
        self.orig = self.data.clone();
    }
}

// ---------------------------------------------------------------------------
// Names and directory entries.

const SFN_ALLOWED: &[u8] = b"!#$%&'()-@^_`{}~";

fn sfn_char(c: char) -> Option<u8> {
    if c.is_ascii_uppercase() || c.is_ascii_digit() || (c.is_ascii() && SFN_ALLOWED.contains(&(c as u8))) {
        Some(c as u8)
    } else {
        None
    }
}

/// The 8.3 name for `long`, unique among `taken`, and whether a long-name
/// entry is needed (Microsoft's basis-name algorithm with a numeric tail).
pub fn short_name(long: &str, taken: &HashSet<[u8; 11]>) -> ([u8; 11], bool) {
    let upper = long.to_uppercase();
    // A name that already is a plain upper-case 8.3 name needs no long entry.
    let (b, e) = match upper.rfind('.') {
        Some(i) if i > 0 => (&upper[..i], &upper[i + 1..]),
        _ => (upper.as_str(), ""),
    };
    let exact = upper == long
        && !b.is_empty()
        && b.len() <= 8
        && e.len() <= 3
        && !b.contains('.')
        && b.chars().all(|c| sfn_char(c).is_some())
        && e.chars().all(|c| sfn_char(c).is_some());
    if exact {
        let mut n = [b' '; 11];
        n[..b.len()].copy_from_slice(b.as_bytes());
        n[8..8 + e.len()].copy_from_slice(e.as_bytes());
        if !taken.contains(&n) {
            return (n, false);
        }
    }
    // Basis name: drop spaces and periods (except the last one), map the rest.
    let trimmed = upper.trim_start_matches('.');
    let (base_src, ext_src) = match trimmed.rfind('.') {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => (trimmed, ""),
    };
    let map = |s: &str| -> Vec<u8> {
        s.chars().filter(|&c| c != ' ' && c != '.').map(|c| sfn_char(c).unwrap_or(b'_')).collect()
    };
    let mut base = map(base_src);
    let mut ext = map(ext_src);
    ext.truncate(3);
    if base.is_empty() {
        base = b"_".to_vec();
    }
    for n in 1u32..1_000_000 {
        let tail = format!("~{n}");
        let keep = base.len().min(8 - tail.len());
        let mut name = [b' '; 11];
        name[..keep].copy_from_slice(&base[..keep]);
        name[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        name[8..8 + ext.len()].copy_from_slice(&ext);
        if !taken.contains(&name) {
            return (name, true);
        }
    }
    unreachable!("a million short-name collisions")
}

/// FAT date and time (2-second resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FatTimestamp {
    pub date: u16,
    pub time: u16,
}

impl FatTimestamp {
    /// From seconds since the Unix epoch (written as UTC; FAT has no zone).
    pub fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Howard Hinnant's civil-from-days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        let y = y.clamp(1980, 2107);
        let (h, mi, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
        FatTimestamp {
            date: (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16,
            time: ((h as u16) << 11) | ((mi as u16) << 5) | (s / 2) as u16,
        }
    }
}

fn lfn_entries(long: &str, sfn: &[u8; 11]) -> Vec<[u8; ENTRY]> {
    const POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let units: Vec<u16> = long.encode_utf16().collect();
    let n = units.len().div_ceil(13);
    let chk = sfn_checksum(sfn);
    let mut out = Vec::with_capacity(n);
    for k in (1..=n).rev() {
        let mut e = [0u8; ENTRY];
        e[0] = k as u8 | if k == n { 0x40 } else { 0 };
        e[11] = 0x0F;
        e[13] = chk;
        for (i, p) in POS.iter().enumerate() {
            let idx = (k - 1) * 13 + i;
            let u = match idx.cmp(&units.len()) {
                std::cmp::Ordering::Less => units[idx],
                std::cmp::Ordering::Equal => 0x0000,
                std::cmp::Ordering::Greater => 0xFFFF,
            };
            e[*p..*p + 2].copy_from_slice(&u.to_le_bytes());
        }
        out.push(e);
    }
    out
}

fn sfn_entry(name: &[u8; 11], attr: u8, cluster: u32, size: u32, ts: FatTimestamp) -> [u8; ENTRY] {
    let mut e = [0u8; ENTRY];
    e[0..11].copy_from_slice(name);
    if e[0] == 0xE5 {
        e[0] = 0x05;
    }
    e[11] = attr;
    e[14..16].copy_from_slice(&ts.time.to_le_bytes());
    e[16..18].copy_from_slice(&ts.date.to_le_bytes());
    e[18..20].copy_from_slice(&ts.date.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&ts.time.to_le_bytes());
    e[24..26].copy_from_slice(&ts.date.to_le_bytes());
    e[26..28].copy_from_slice(&((cluster & 0xFFFF) as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// The long-name entries (if any) followed by the 8.3 entry for one name.
fn entry_group(
    long: &str,
    taken: &mut HashSet<[u8; 11]>,
    attr: u8,
    cluster: u32,
    size: u32,
    ts: FatTimestamp,
) -> Vec<[u8; ENTRY]> {
    let (sfn, needs_lfn) = short_name(long, taken);
    taken.insert(sfn);
    let mut g = if needs_lfn { lfn_entries(long, &sfn) } else { Vec::new() };
    g.push(sfn_entry(&sfn, attr, cluster, size, ts));
    g
}

fn slots_for(long: &str) -> usize {
    // Upper bound used for sizing; exact count comes from entry_group.
    1 + long.encode_utf16().count().div_ceil(13)
}

const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const FORBIDDEN: &[char] = &['"', '*', '/', ':', '<', '>', '?', '\\', '|'];

fn validate_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || name.ends_with(' ')
        || name.chars().any(|c| FORBIDDEN.contains(&c) || (c as u32) < 0x20)
        || name.encode_utf16().count() > 255;
    if bad {
        return Err(FatError::Unsafe(format!("{name:?} is not a valid FAT long name")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Jobs and results.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteItem {
    pub source: PathBuf,
    /// Name to give the file on the stick.
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WriteOptions {
    /// Delete an existing folder of that name (or, at the top level, existing
    /// files with the same names) before writing.
    #[serde(default)]
    pub replace: bool,
    /// If no single free block is large enough, place files in ascending
    /// cluster order but allow them to be split. Off: refuse instead.
    #[serde(default)]
    pub allow_fragmented: bool,
    /// Write even if the volume looks in use or its FAT copies differ.
    #[serde(default)]
    pub force: bool,
    /// Plan only.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLayout {
    pub name: String,
    pub first_cluster: u32,
    pub clusters: u32,
    /// Number of separate runs the file occupies (1 = unbroken).
    pub fragments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layout {
    /// All files in one unbroken run, back to back in playlist order.
    pub contiguous: bool,
    /// Every cluster of every file is higher than the one before it, in playlist order.
    pub ascending: bool,
    pub dir_clusters: Vec<u32>,
    pub files: Vec<FileLayout>,
}

impl Layout {
    fn from_chains(dir_clusters: Vec<u32>, files: &[(String, Vec<u32>)]) -> Self {
        let all: Vec<u32> = files.iter().flat_map(|(_, c)| c.iter().copied()).collect();
        let ascending = all.windows(2).all(|w| w[0] < w[1]);
        let contiguous = all.windows(2).all(|w| w[1] == w[0] + 1);
        let files = files
            .iter()
            .map(|(n, c)| FileLayout {
                name: n.clone(),
                first_cluster: c.first().copied().unwrap_or(0),
                clusters: c.len() as u32,
                fragments: if c.is_empty() { 0 } else { 1 + c.windows(2).filter(|w| w[1] != w[0] + 1).count() as u32 },
            })
            .collect();
        Layout { contiguous, ascending, dir_clusters, files }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteProgress {
    pub stage: String,
    pub index: usize,
    pub total: usize,
    pub name: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResult {
    pub written: bool,
    /// The folder that holds the tracks, e.g. "/Road Trip" or "/".
    pub target_dir: String,
    pub names: Vec<String>,
    pub sizes: Vec<u64>,
    pub layout: Layout,
    pub needed_clusters: u32,
    pub free_clusters_before: u32,
    pub largest_free_run: u32,
    pub deleted: Vec<String>,
    /// Undo files, oldest first (restore newest first).
    pub backups: Vec<PathBuf>,
    pub bytes_written: u64,
    #[serde(default)]
    pub undo_note: Option<String>,
}

/// What the verification read from the medium found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteVerification {
    pub order_ok: bool,
    pub layout_matches_plan: bool,
    pub layout: Option<Layout>,
    pub fat_copies_identical: bool,
    pub data_mismatches: Vec<String>,
    pub problems: Vec<String>,
}

impl WriteVerification {
    pub fn ok(&self) -> bool {
        self.order_ok && self.layout_matches_plan && self.fat_copies_identical && self.data_mismatches.is_empty() && self.problems.is_empty()
    }
}

// ---------------------------------------------------------------------------

enum Target {
    /// A new folder `name` inside `parent`.
    Folder { parent: String, name: String },
    /// The top level of the stick.
    TopLevel,
}

fn parse_target(path: &str) -> Target {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.split_last() {
        None => Target::TopLevel,
        Some((name, rest)) => Target::Folder { parent: format!("/{}", rest.join("/")), name: (*name).to_string() },
    }
}

/// A directory's table as read: where it lives and its bytes.
struct DirState {
    dir: DirRef,
    extents: Vec<Extent>,
    bytes: Vec<u8>,
}

impl DirState {
    fn listing(&self) -> DirListing {
        parse_directory(&self.bytes, self.dir)
    }
}

fn find_group<'a>(l: &'a DirListing, name: &str) -> Option<&'a DirEntryGroup> {
    let key = normalize_name(name);
    l.groups.iter().find(|g| {
        matches!(g.kind, EntryKind::File | EntryKind::Dir) && (normalize_name(g.name()) == key || normalize_name(&g.short_name) == key)
    })
}

fn taken_short_names(l: &DirListing) -> HashSet<[u8; 11]> {
    l.groups.iter().filter_map(|g| g.slots.last()).map(|s| s[0..11].try_into().unwrap()).collect()
}

fn mark_deleted(bytes: &mut [u8], g: &DirEntryGroup) {
    for i in 0..g.slots.len() {
        bytes[(g.first_slot + i) * ENTRY] = 0xE5;
    }
}

impl<D: Device> Volume<D> {
    fn fat_offset(&self, copy: u32) -> u64 {
        let g = self.geometry();
        g.partition_offset + (g.reserved_sectors as u64 + copy as u64 * g.fat_size_sectors as u64) * g.bytes_per_sector as u64
    }

    /// Load the active FAT copy whole.
    pub fn load_fat(&mut self) -> Result<FatTable> {
        let g = self.geometry().clone();
        let len = g.fat_size_sectors as u64 * g.bytes_per_sector as u64;
        let base = self.fat_offset(g.active_fat);
        let mut data = vec![0u8; len as usize];
        let chunk = (1u64 << 20) / g.bytes_per_sector as u64 * g.bytes_per_sector as u64;
        let mut off = 0u64;
        while off < len {
            let n = chunk.min(len - off);
            self.read_extent(Extent { offset: base + off, len: n }, &mut data[off as usize..(off + n) as usize])?;
            off += n;
        }
        // The table must cover every cluster.
        let need = match g.fat_type {
            FatType::Fat32 => (g.max_cluster() as u64 + 1) * 4,
            FatType::Fat16 => (g.max_cluster() as u64 + 1) * 2,
            FatType::Fat12 => (g.max_cluster() as u64 + 1) * 3 / 2 + 1,
        };
        if need > len {
            return Err(FatError::Corrupt("FAT is smaller than the cluster count".into()));
        }
        Ok(FatTable { orig: data.clone(), data, fat_type: g.fat_type, max_cluster: g.max_cluster() })
    }

    /// Extents and old/new bytes of every FAT sector that differs, for every copy written.
    fn fat_sector_writes(&self, t: &FatTable) -> Vec<SectorWrite> {
        let g = self.geometry();
        let bps = g.bytes_per_sector as usize;
        let copies: Vec<u32> = if g.fat_mirroring { (0..g.num_fats).collect() } else { vec![g.active_fat] };
        let mut out = Vec::new();
        // Active copy first.
        let mut order = copies.clone();
        order.sort_by_key(|&c| if c == g.active_fat { 0 } else { 1 });
        for s in t.changed_sectors(bps) {
            for &c in &order {
                out.push((
                    Extent { offset: self.fat_offset(c) + (s * bps) as u64, len: bps as u64 },
                    t.orig[s * bps..(s + 1) * bps].to_vec(),
                    t.data[s * bps..(s + 1) * bps].to_vec(),
                ));
            }
        }
        out
    }

    fn read_dir_state(&mut self, d: DirRef) -> Result<DirState> {
        let extents = self.dir_extents(d)?;
        let bytes = self.read_extents(&extents)?;
        Ok(DirState { dir: d, extents, bytes })
    }

    /// Sector-level changes between two images of one directory.
    fn dir_sector_writes(&self, extents: &[Extent], old: &[u8], new: &[u8]) -> Vec<SectorWrite> {
        let bps = self.sector_size() as usize;
        let mut out = Vec::new();
        let mut pos = 0usize;
        for e in extents {
            for s in 0..(e.len as usize / bps) {
                let a = pos + s * bps;
                if old[a..a + bps] != new[a..a + bps] {
                    out.push((Extent { offset: e.offset + (s * bps) as u64, len: bps as u64 }, old[a..a + bps].to_vec(), new[a..a + bps].to_vec()));
                }
            }
            pos += e.len as usize;
        }
        out
    }

    fn fsinfo_update(&mut self, delta: i64, next_free: Option<u32>) -> Result<Option<SectorWrite>> {
        let g = self.geometry().clone();
        if g.fat_type != FatType::Fat32 || g.fs_info_sector == 0 {
            return Ok(None);
        }
        let bps = g.bytes_per_sector as u64;
        let e = Extent { offset: g.partition_offset + g.fs_info_sector as u64 * bps, len: bps };
        let mut old = vec![0u8; bps as usize];
        self.read_extent(e, &mut old)?;
        let sig = |o: usize| u32::from_le_bytes(old[o..o + 4].try_into().unwrap());
        if sig(0) != 0x4161_5252 || sig(484) != 0x6141_7272 {
            return Ok(None);
        }
        let mut new = old.clone();
        let free = sig(488);
        if free != 0xFFFF_FFFF && free <= g.cluster_count {
            let v = (free as i64 + delta).clamp(0, g.cluster_count as i64) as u32;
            new[488..492].copy_from_slice(&v.to_le_bytes());
        }
        if let Some(n) = next_free {
            if n >= 2 && n <= g.max_cluster() {
                new[492..496].copy_from_slice(&n.to_le_bytes());
            }
        }
        Ok(if new == old { None } else { Some((e, old, new)) })
    }

    fn apply_writes(&mut self, writes: &[SectorWrite]) -> Result<()> {
        for (e, _, new) in writes {
            self.write_extent(*e, new)?;
        }
        self.sync()?;
        Ok(())
    }

    fn save_backup(
        &self,
        dir: Option<&Path>,
        tag: &str,
        writes: &[SectorWrite],
        dir_offsets: &HashSet<u64>,
    ) -> Result<Option<PathBuf>> {
        let Some(dir) = dir else { return Ok(None) };
        let g = self.geometry();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_nanos()).unwrap_or(0);
        let p = dir.join(format!("p2u-backup-{:08x}-{stamp}-{tag}.bin", g.volume_serial));
        let mut data = Vec::new();
        let mut after = Vec::new();
        let mut extents = Vec::new();
        for (e, old, new) in writes {
            extents.push(*e);
            data.extend_from_slice(old);
            after.extend_from_slice(new);
        }
        let b = Backup {
            format: 2,
            volume_serial: g.volume_serial,
            partition_offset: g.partition_offset,
            dir_path: String::new(),
            extents,
            data,
            after,
            after_len: 0,
            dir_sectors: writes.iter().map(|(e, _, _)| dir_offsets.contains(&e.offset)).collect(),
            superseded: false,
            note: String::new(),
        };
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .and_then(|_| b.save(&p))
            .map_err(|e| FatError::Unsafe(format!("nothing was written: the undo backup could not be saved ({e})")))?;
        Ok(Some(p))
    }

    /// Rewrite a directory table in place in two passes: first every entry
    /// that changes is marked deleted as a whole (all its slots), writing the
    /// sectors back to front; then the new contents are written front to back.
    /// So an interruption can hide an entry, never duplicate one, and it can
    /// only ever leave the start of a long name without its end (which fsck
    /// removes automatically), never a stray middle part.
    pub(crate) fn rewrite_dir_two_pass(&mut self, extents: &[Extent], old: &[u8], new: &[u8]) -> Result<()> {
        let mid = two_pass_middle(old, new);
        let mut pass1 = self.dir_sector_writes(extents, old, &mid);
        pass1.reverse();
        self.apply_writes(&pass1)?;
        let pass2 = self.dir_sector_writes(extents, &mid, new);
        self.apply_writes(&pass2)?;
        Ok(())
    }

    /// Collect every cluster owned by a folder tree (its own chain included).
    fn tree_clusters(&mut self, fat: &FatTable, first: u32, out: &mut Vec<u32>, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(FatError::Corrupt("folder nesting too deep".into()));
        }
        out.extend(fat.chain(first)?);
        let l = self.list_dir(DirRef::Cluster(first))?;
        for g in &l.groups {
            match g.kind {
                EntryKind::File if g.first_cluster >= 2 => out.extend(fat.chain(g.first_cluster)?),
                EntryKind::Dir if g.first_cluster >= 2 => self.tree_clusters(fat, g.first_cluster, out, depth + 1)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Write `items`, in this order, into `target_dir` ("/Road Trip" creates
    /// that folder; "/" is the top level). See the module docs for the
    /// placement and crash-safety rules.
    pub fn write_playlist(
        &mut self,
        target_dir: &str,
        items: &[WriteItem],
        opts: &WriteOptions,
        backup_dir: Option<&Path>,
        progress: &mut dyn FnMut(&WriteProgress),
    ) -> Result<WriteResult> {
        let g = self.geometry().clone();
        let cb = g.cluster_bytes();

        // ---- Preconditions -------------------------------------------------
        let health = self.health(true)?;
        if !opts.force && (health.boot_sector_dirty || !health.fat_clean_shutdown) {
            return Err(FatError::Unsafe("the volume is marked as in use (still mounted, or not cleanly ejected)".into()));
        }
        if !opts.force && health.fats_identical == Some(false) {
            return Err(FatError::Unsafe("the FAT copies differ; run fsck.fat / chkdsk first".into()));
        }
        let mut seen = HashSet::new();
        let mut sizes = Vec::with_capacity(items.len());
        for it in items {
            validate_name(&it.name)?;
            if !seen.insert(normalize_name(&it.name)) {
                return Err(FatError::Unsafe(format!("{:?} appears twice", it.name)));
            }
            let m = std::fs::metadata(&it.source)?;
            if !m.is_file() {
                return Err(FatError::NotFound(it.source.display().to_string()));
            }
            if m.len() > u32::MAX as u64 {
                return Err(FatError::Unsafe(format!("{} is larger than FAT32's 4 GiB file limit", it.name)));
            }
            sizes.push(m.len());
        }
        let target = parse_target(target_dir);
        if let Target::Folder { name, .. } = &target {
            validate_name(name)?;
        }

        let mut fat = self.load_fat()?;
        let free_before = fat.free_count();
        let mut backups = Vec::new();
        let mut deleted = Vec::new();

        // ---- Phase 1: deletions (replace) ---------------------------------
        let (parent_ref, mut parent) = match &target {
            Target::Folder { parent, .. } => {
                let d = self.find_dir(parent)?;
                (d, self.read_dir_state(d)?)
            }
            Target::TopLevel => (DirRef::Root, self.read_dir_state(DirRef::Root)?),
        };
        let parent_old = parent.bytes.clone();
        let mut freed: Vec<u32> = Vec::new();
        {
            let listing = parent.listing();
            if !listing.is_consistent() && !opts.force {
                return Err(FatError::Unsafe("the parent folder has orphaned long-name entries; run fsck.fat / chkdsk first".into()));
            }
            let doomed: Vec<DirEntryGroup> = match &target {
                Target::Folder { name, .. } => match find_group(&listing, name) {
                    Some(gr) if gr.kind == EntryKind::File => {
                        return Err(FatError::Unsafe(format!("a file named {name:?} is in the way")))
                    }
                    Some(gr) if !opts.replace => {
                        let _ = gr;
                        return Err(FatError::Unsafe(format!("the folder {name:?} already exists (allow replacing to overwrite it)")));
                    }
                    Some(gr) => vec![gr.clone()],
                    None => vec![],
                },
                Target::TopLevel => {
                    let mut v = Vec::new();
                    for it in items {
                        if let Some(gr) = find_group(&listing, &it.name) {
                            if gr.kind == EntryKind::Dir {
                                return Err(FatError::Unsafe(format!("a folder named {:?} is in the way", it.name)));
                            }
                            if !opts.replace {
                                return Err(FatError::Unsafe(format!("{:?} is already on the stick (allow replacing to overwrite it)", it.name)));
                            }
                            v.push(gr.clone());
                        }
                    }
                    v
                }
            };
            for gr in &doomed {
                mark_deleted(&mut parent.bytes, gr);
                deleted.push(gr.name().to_string());
                if gr.first_cluster >= 2 {
                    if gr.kind == EntryKind::Dir {
                        self.tree_clusters(&fat, gr.first_cluster, &mut freed, 0)?;
                    } else {
                        freed.extend(fat.chain(gr.first_cluster)?);
                    }
                }
            }
        }
        freed.sort_unstable();
        freed.dedup();
        if !deleted.is_empty() {
            for &c in &freed {
                fat.set(c, 0);
            }
            let dir_writes = self.dir_sector_writes(&parent.extents, &parent_old, &parent.bytes);
            let fat_writes = self.fat_sector_writes(&fat);
            let fsi = self.fsinfo_update(freed.len() as i64, None)?;
            if !opts.dry_run {
                let mut all = dir_writes.clone();
                all.extend(fat_writes.iter().cloned());
                all.extend(fsi.iter().cloned());
                let dir_offsets: HashSet<u64> = dir_writes.iter().map(|(e, _, _)| e.offset).collect();
                if let Some(p) = self.save_backup(backup_dir, "1-delete", &all, &dir_offsets)? {
                    backups.push(p);
                }
                // Entries first (back to front, see rewrite_dir_two_pass), then free the clusters.
                let mut rev = dir_writes.clone();
                rev.reverse();
                self.apply_writes(&rev)?;
                self.apply_writes(&fat_writes)?;
                if let Some(w) = fsi {
                    self.apply_writes(&[w])?;
                }
                self.invalidate_cache()?;
            }
            fat.commit();
        }
        let parent_after_delete = parent.bytes.clone();

        // ---- Phase 2: plan the new layout ---------------------------------
        let ts_base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            - 2 * items.len() as i64
            - 2;
        let ts_base = ts_base - ts_base.rem_euclid(2);
        let file_clusters: Vec<u32> = sizes.iter().map(|&s| s.div_ceil(cb) as u32).collect();
        let files_total: u32 = file_clusters.iter().sum();

        // Size the new folder's own table (folder mode).
        let dir_slots: usize = match &target {
            Target::Folder { .. } => 2 + items.iter().map(|i| slots_for(&i.name)).sum::<usize>(),
            Target::TopLevel => 0,
        };
        let dir_clusters_n = if dir_slots == 0 { 0 } else { ((dir_slots * ENTRY) as u64).div_ceil(cb) as u32 };
        let needed = dir_clusters_n + files_total;

        let runs = fat.free_runs();
        let largest = runs.iter().map(|r| r.1).max().unwrap_or(0);
        let free_now = fat.free_count();
        // Prefer space that was free before this operation, so a replaced
        // folder's data stays intact (and its undo backup stays valid).
        let freed_set: HashSet<u32> = freed.iter().copied().collect();
        let untouched_runs = fat.free_runs_excluding(&freed_set);
        if needed > free_now {
            return Err(FatError::Unsafe(format!(
                "not enough free space: {} MB needed, {} MB free",
                needed as u64 * cb / 1_000_000,
                free_now as u64 * cb / 1_000_000
            )));
        }
        let untouched_free = (2..=fat.max_cluster).filter(|&c| fat.is_free(c) && !freed_set.contains(&c)).count() as u32;
        let assigned: Vec<u32> = match (untouched_runs.iter().find(|r| r.1 >= needed), runs.iter().find(|r| r.1 >= needed)) {
            (Some(&(start, _)), _) | (None, Some(&(start, _))) => (start..start + needed).collect(),
            (None, None) if opts.allow_fragmented && untouched_free >= needed => {
                (2..=fat.max_cluster).filter(|&c| fat.is_free(c) && !freed_set.contains(&c)).take(needed as usize).collect()
            }
            (None, None) if opts.allow_fragmented => (2..=fat.max_cluster).filter(|&c| fat.is_free(c)).take(needed as usize).collect(),
            (None, None) => {
                return Err(FatError::Unsafe(format!(
                    "no single free block is large enough: {} MB needed, largest free block {} MB. Free some space, erase the stick, or allow a fragmented layout",
                    needed as u64 * cb / 1_000_000,
                    largest as u64 * cb / 1_000_000
                )))
            }
        };
        let dir_clusters: Vec<u32> = assigned[..dir_clusters_n as usize].to_vec();
        let mut file_chains: Vec<Vec<u32>> = Vec::with_capacity(items.len());
        let mut pos = dir_clusters_n as usize;
        for &n in &file_clusters {
            file_chains.push(assigned[pos..pos + n as usize].to_vec());
            pos += n as usize;
        }
        let in_run: HashSet<u32> = assigned.iter().copied().collect();

        // Link the chains in the in-memory table.
        let link = |fat: &mut FatTable, chain: &[u32]| {
            for w in chain.windows(2) {
                fat.set(w[0], w[1]);
            }
            if let Some(&last) = chain.last() {
                let eoc = fat.eoc();
                fat.set(last, eoc);
            }
        };
        link(&mut fat, &dir_clusters);
        for ch in &file_chains {
            link(&mut fat, ch);
        }

        // Entries for the tracks, in playlist order.
        let mut taken_tracks: HashSet<[u8; 11]> = match &target {
            Target::Folder { .. } => HashSet::new(),
            Target::TopLevel => taken_short_names(&parse_directory(&parent_after_delete, DirRef::Root)),
        };
        let mut track_groups: Vec<Vec<[u8; ENTRY]>> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            let first = file_chains[i].first().copied().unwrap_or(0);
            let ts = FatTimestamp::from_unix(ts_base + 2 * i as i64);
            track_groups.push(entry_group(&it.name, &mut taken_tracks, ATTR_ARCHIVE, first, sizes[i] as u32, ts));
        }

        // Extra clusters for a table that must grow, taken outside the main run.
        let spare: Vec<u32> = (2..=fat.max_cluster).filter(|&c| fat.is_free(c) && !in_run.contains(&c)).take(4096).collect();
        let mut spare_iter = spare.into_iter();
        let mut zero_clusters: Vec<u32> = Vec::new();
        let mut grow = |fat: &mut FatTable, state: &mut DirState, slots_needed: usize| -> Result<()> {
            let have = state.bytes.len() / ENTRY;
            if slots_needed <= have {
                return Ok(());
            }
            let first = match state.dir {
                DirRef::Root if g.fat_type == FatType::Fat32 => g.root_cluster,
                DirRef::Root => return Err(FatError::Unsafe("the top level of this FAT12/16 stick is full".into())),
                DirRef::Cluster(c) => c,
            };
            let mut chain = fat.chain(first)?;
            let more = ((slots_needed - have) * ENTRY).div_ceil(cb as usize);
            for _ in 0..more {
                let c = spare_iter.next().ok_or_else(|| FatError::Unsafe("no free cluster to grow the folder table".into()))?;
                let last = *chain.last().unwrap();
                fat.set(last, c);
                let eoc = fat.eoc();
                fat.set(c, eoc);
                chain.push(c);
                zero_clusters.push(c);
                state.extents.push(Extent { offset: g.cluster_offset(c), len: cb });
                state.bytes.extend(std::iter::repeat_n(0u8, cb as usize));
            }
            Ok(())
        };

        // The new folder's own table, or the rewritten top-level table.
        let mut new_dir_bytes: Vec<u8> = Vec::new();
        let mut parent_new = parent_after_delete.clone();
        let mut commit_two_pass = false;
        let target_label: String;
        match &target {
            Target::Folder { name, .. } => {
                target_label = format!("{}/{}", if let Target::Folder { parent, .. } = &target { parent.trim_end_matches('/') } else { "" }, name);
                // Folder table: ".", "..", then the tracks.
                let ts = FatTimestamp::from_unix(ts_base);
                let me = dir_clusters[0];
                let up = match parent_ref {
                    DirRef::Root => 0,
                    DirRef::Cluster(c) => c,
                };
                new_dir_bytes = vec![0u8; dir_clusters.len() * cb as usize];
                let dots = [sfn_entry(b".          ", ATTR_DIR, me, 0, ts), sfn_entry(b"..         ", ATTR_DIR, up, 0, ts)];
                for (slot, e) in dots.into_iter().chain(track_groups.iter().flatten().copied()).enumerate() {
                    new_dir_bytes[slot * ENTRY..(slot + 1) * ENTRY].copy_from_slice(&e);
                }
                // The folder's entry in its parent: append after the last entry,
                // else reuse a run of deleted slots, else grow the parent table.
                let mut taken_parent = taken_short_names(&parse_directory(&parent_after_delete, parent_ref));
                let folder_group = entry_group(name, &mut taken_parent, ATTR_DIR, me, 0, ts);
                let k = folder_group.len();
                let listing = parse_directory(&parent.bytes, parent_ref);
                let cap = parent.bytes.len() / ENTRY;
                let at = if listing.end_slot + k <= cap {
                    Some(listing.end_slot)
                } else {
                    (0..listing.end_slot.saturating_sub(k - 1)).find(|&s| (s..s + k).all(|i| parent.bytes[i * ENTRY] == 0xE5))
                };
                let at = match at {
                    Some(a) => a,
                    None => {
                        grow(&mut fat, &mut parent, listing.end_slot + k + 1)?;
                        parent_new = parent.bytes.clone();
                        listing.end_slot
                    }
                };
                // Grown clusters are zero in `parent.bytes`; keep the commit image in step.
                if parent_new.len() < parent.bytes.len() {
                    parent_new.resize(parent.bytes.len(), 0);
                }
                for (i, e) in folder_group.iter().enumerate() {
                    parent_new[(at + i) * ENTRY..(at + i + 1) * ENTRY].copy_from_slice(e);
                }
                // Make sure the table ends right after a newly appended entry.
                let after = at + k;
                if at == listing.end_slot && after < parent_new.len() / ENTRY {
                    parent_new[after * ENTRY..(after + 1) * ENTRY].fill(0);
                }
            }
            Target::TopLevel => {
                target_label = "/".into();
                // Rebuild the top-level table: label, the tracks in playlist
                // order, then everything else in its existing order, compacted.
                let listing = parse_directory(&parent_after_delete, DirRef::Root);
                let mut head: Vec<[u8; ENTRY]> = Vec::new();
                let mut rest: Vec<[u8; ENTRY]> = Vec::new();
                for gr in &listing.groups {
                    match gr.kind {
                        EntryKind::VolumeLabel | EntryKind::Dot | EntryKind::DotDot => head.extend(gr.slots.iter().copied()),
                        _ => rest.extend(gr.slots.iter().copied()),
                    }
                }
                let total = head.len() + track_groups.iter().map(|t| t.len()).sum::<usize>() + rest.len();
                grow(&mut fat, &mut parent, total + 1)?;
                parent_new = vec![0u8; parent.bytes.len()];
                for (i, e) in head.iter().chain(track_groups.iter().flatten()).chain(rest.iter()).enumerate() {
                    parent_new[i * ENTRY..(i + 1) * ENTRY].copy_from_slice(e);
                }
                commit_two_pass = true;
            }
        }

        let layout = Layout::from_chains(
            dir_clusters.clone(),
            &items.iter().zip(&file_chains).map(|(i, c)| (i.name.clone(), c.clone())).collect::<Vec<_>>(),
        );
        let mut result = WriteResult {
            written: false,
            target_dir: target_label.clone(),
            names: items.iter().map(|i| i.name.clone()).collect(),
            sizes: sizes.clone(),
            layout,
            needed_clusters: needed,
            free_clusters_before: free_before,
            largest_free_run: largest,
            deleted,
            backups,
            bytes_written: 0,
            undo_note: None,
        };
        if opts.dry_run {
            return Ok(result);
        }

        // Metadata writes, planned in full before anything is written.
        let fat_writes = self.fat_sector_writes(&fat);
        // The parent's old bytes as on disk now (after phase 1), padded for grown clusters.
        let mut parent_disk = parent_after_delete.clone();
        parent_disk.resize(parent.bytes.len(), 0);
        let dir_writes = self.dir_sector_writes(&parent.extents, &parent_disk, &parent_new);
        let alloc_delta = -(needed as i64) - zero_clusters.len() as i64;
        let next = assigned.last().map(|c| c + 1);
        let fsi = self.fsinfo_update(alloc_delta, next)?;
        {
            // Grown clusters were free: no backup needed for them.
            let grown: HashSet<u64> = zero_clusters.iter().map(|&c| g.cluster_offset(c)).collect();
            let mut all: Vec<SectorWrite> = fat_writes.clone();
            all.extend(dir_writes.iter().filter(|(e, _, _)| !grown.iter().any(|&o| e.offset >= o && e.offset < o + cb)).cloned());
            all.extend(fsi.iter().cloned());
            let dir_offsets: HashSet<u64> = dir_writes.iter().map(|(e, _, _)| e.offset).collect();
            if let Some(p) = self.save_backup(backup_dir, "2-write", &all, &dir_offsets)? {
                result.backups.push(p);
            }
        }
        // If the new data lands where the replaced files were, the deletion's
        // backup could no longer bring them back: mark it before writing.
        let reuses = assigned.iter().chain(zero_clusters.iter()).any(|c| freed_set.contains(c));
        if reuses {
            if let Some(first) = result.backups.iter().find(|p| p.to_string_lossy().ends_with("-1-delete.bin")) {
                Backup::supersede(first, "the replaced files' space was reused by the new copy")
                    .map_err(|e| FatError::Unsafe(format!("nothing new was written: could not update the backup ({e})")))?;
            }
            result.undo_note = Some("The old folder's space was reused, so only the new write can be undone.".into());
        }

        // ---- Stage A: data and new tables into free clusters --------------
        let total_bytes: u64 = sizes.iter().sum();
        let mut buf = vec![0u8; 1 << 20];
        for (i, it) in items.iter().enumerate() {
            let mut p = WriteProgress { stage: "write".into(), index: i, total: items.len(), name: it.name.clone(), bytes_done: 0, bytes_total: sizes[i] };
            progress(&p);
            let mut f = std::fs::File::open(&it.source)?;
            let chain = &file_chains[i];
            let mut ci = 0usize;
            let mut remaining = sizes[i];
            while ci < chain.len() {
                // A run of consecutive clusters, capped to the buffer size.
                let mut n = 1usize;
                while ci + n < chain.len() && chain[ci + n] == chain[ci + n - 1] + 1 && (n + 1) as u64 * cb <= buf.len() as u64 {
                    n += 1;
                }
                let len = n as u64 * cb;
                let want = remaining.min(len) as usize;
                let chunk = &mut buf[..len as usize];
                read_full(&mut f, &mut chunk[..want])?;
                chunk[want..].fill(0);
                self.write_extent(Extent { offset: g.cluster_offset(chain[ci]), len }, chunk)?;
                remaining -= want as u64;
                p.bytes_done += want as u64;
                result.bytes_written += want as u64;
                progress(&p);
                ci += n;
            }
            // The source must not have changed size while we copied it.
            let mut probe = [0u8; 1];
            if remaining != 0 || f.read(&mut probe)? != 0 {
                return Err(FatError::Unsafe(format!("{} changed while it was being copied; nothing was committed", it.name)));
            }
        }
        for (k, &c) in dir_clusters.iter().enumerate() {
            let a = k * cb as usize;
            self.write_extent(Extent { offset: g.cluster_offset(c), len: cb }, &new_dir_bytes[a..a + cb as usize])?;
        }
        let zero = vec![0u8; cb as usize];
        for &c in &zero_clusters {
            self.write_extent(Extent { offset: g.cluster_offset(c), len: cb }, &zero)?;
        }
        self.sync()?;
        let _ = total_bytes;

        // ---- Stage B: allocation chains, every FAT copy ---------------------
        self.apply_writes(&fat_writes)?;

        // ---- Stage C: commit ------------------------------------------------
        if commit_two_pass {
            self.rewrite_dir_two_pass(&parent.extents, &parent_disk, &parent_new)?;
        } else {
            // Appending: if the slot after the new entry held leftover bytes,
            // zero it first, so the table never runs on into junk.
            let mut zeroed = parent_disk.clone();
            let mut differs = false;
            for i in 0..zeroed.len() / ENTRY {
                let new_slot = &parent_new[i * ENTRY..(i + 1) * ENTRY];
                if new_slot.iter().all(|&b| b == 0) && zeroed[i * ENTRY..(i + 1) * ENTRY].iter().any(|&b| b != 0) {
                    zeroed[i * ENTRY..(i + 1) * ENTRY].fill(0);
                    differs = true;
                }
            }
            if differs {
                let pre = self.dir_sector_writes(&parent.extents, &parent_disk, &zeroed);
                self.apply_writes(&pre)?;
                let rest = self.dir_sector_writes(&parent.extents, &zeroed, &parent_new);
                self.apply_writes(&rest)?;
            } else {
                self.apply_writes(&dir_writes)?;
            }
        }

        // ---- Stage D: FSInfo -------------------------------------------------
        if let Some(w) = fsi {
            self.apply_writes(&[w])?;
        }
        self.invalidate_cache()?;
        result.written = true;
        Ok(result)
    }

    /// Re-read everything from the medium and compare it with what was planned
    /// and with the source files.
    pub fn verify_written(
        &mut self,
        result: &WriteResult,
        items: &[WriteItem],
        check_data: bool,
        progress: &mut dyn FnMut(&WriteProgress),
    ) -> Result<WriteVerification> {
        self.drop_caches()?;
        let mut v = WriteVerification {
            order_ok: false,
            layout_matches_plan: false,
            layout: None,
            fat_copies_identical: false,
            data_mismatches: vec![],
            problems: vec![],
        };
        v.fat_copies_identical = self.health(true)?.fats_identical != Some(false);
        let d = self.find_dir(&result.target_dir)?;
        let listing = self.list_dir(d)?;
        let names: Vec<String> = listing
            .groups
            .iter()
            .filter(|g| matches!(g.kind, EntryKind::File | EntryKind::Dir))
            .map(|g| g.name().to_string())
            .collect();
        let check = super::check_order(&names, &result.names);
        // At the top level other entries may follow; the playlist must come first and in order.
        v.order_ok = check.in_order && names.iter().take(result.names.len()).zip(&result.names).all(|(a, b)| normalize_name(a) == normalize_name(b));
        if !v.order_ok {
            v.problems.push(format!("entries are not in playlist order (first wrong position {:?})", check.first_mismatch));
        }
        let fat = self.load_fat()?;
        let cb = self.geometry().cluster_bytes();
        let mut chains = Vec::new();
        for (i, name) in result.names.iter().enumerate() {
            let Some(gr) = find_group(&listing, name) else {
                v.problems.push(format!("{name} is missing"));
                continue;
            };
            if gr.size as u64 != result.sizes[i] {
                v.problems.push(format!("{name}: size {} on the stick, {} expected", gr.size, result.sizes[i]));
            }
            let chain = if gr.first_cluster >= 2 { fat.chain(gr.first_cluster)? } else { vec![] };
            if chain.len() as u64 != result.sizes[i].div_ceil(cb) {
                v.problems.push(format!("{name}: chain of {} clusters for {} bytes", chain.len(), result.sizes[i]));
            }
            chains.push((name.clone(), chain));
        }
        let layout = Layout::from_chains(result.layout.dir_clusters.clone(), &chains);
        v.layout_matches_plan = layout.files.iter().zip(&result.layout.files).all(|(a, b)| {
            a.first_cluster == b.first_cluster && a.clusters == b.clusters && a.fragments == b.fragments
        }) && layout.files.len() == result.layout.files.len()
            && layout.contiguous == result.layout.contiguous;
        v.layout = Some(layout);
        if check_data {
            let g = self.geometry().clone();
            let mut buf = vec![0u8; cb as usize];
            let mut src = vec![0u8; cb as usize];
            for (i, (name, chain)) in chains.iter().enumerate() {
                let mut p = WriteProgress { stage: "verify".into(), index: i, total: chains.len(), name: name.clone(), bytes_done: 0, bytes_total: result.sizes[i] };
                progress(&p);
                let mut f = std::fs::File::open(&items[i].source)?;
                let mut remaining = result.sizes[i];
                let mut same = true;
                for &c in chain {
                    self.read_extent(Extent { offset: g.cluster_offset(c), len: cb }, &mut buf)?;
                    let n = remaining.min(cb) as usize;
                    read_full(&mut f, &mut src[..n])?;
                    if buf[..n] != src[..n] {
                        same = false;
                        break;
                    }
                    remaining -= n as u64;
                    p.bytes_done += n as u64;
                }
                progress(&p);
                if !same {
                    v.data_mismatches.push(name.clone());
                }
            }
        }
        Ok(v)
    }
}

/// The in-between image of a two-pass rewrite: every live entry (all of its
/// slots) that is not identical in the new image is marked deleted.
pub(crate) fn two_pass_middle(old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut mid = old.to_vec();
    let listing = parse_directory(old, DirRef::Root);
    let mut covered = vec![false; old.len() / ENTRY];
    for g in &listing.groups {
        let r = g.first_slot..g.first_slot + g.slots.len();
        for i in r.clone() {
            covered[i] = true;
        }
        let changed = r.clone().any(|i| old[i * ENTRY..(i + 1) * ENTRY] != new[i * ENTRY..(i + 1) * ENTRY]);
        if changed {
            for i in r {
                mid[i * ENTRY] = 0xE5;
            }
        }
    }
    // Live slots outside any group (orphaned long-name parts): per slot.
    for (i, cov) in covered.iter().enumerate() {
        let o = &old[i * ENTRY..(i + 1) * ENTRY];
        if !cov && o[0] != 0x00 && o[0] != 0xE5 && o != &new[i * ENTRY..(i + 1) * ENTRY] {
            mid[i * ENTRY] = 0xE5;
        }
    }
    // Nothing after the end marker is live; leave it as is.
    mid
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        match f.read(&mut buf[off..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "source file is shorter than expected")),
            Ok(n) => off += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Unicode-normalize a name the way it will be written (NFC), so the order
/// list and the written entries agree.
pub fn written_name(name: &str) -> String {
    name.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_follow_the_basis_rules() {
        let mut taken = HashSet::new();
        let (a, lfn) = short_name("README.TXT", &taken);
        assert_eq!(&a, b"README  TXT");
        assert!(!lfn);
        let (b, lfn) = short_name("01 - Night Ferry.mp3", &taken);
        assert_eq!(&b, b"01-NIG~1MP3");
        assert!(lfn);
        taken.insert(b);
        let (c, _) = short_name("01 - Night Train.mp3", &taken);
        assert_eq!(&c, b"01-NIG~2MP3");
        let (d, _) = short_name("Café+Bar.flac", &HashSet::new());
        assert_eq!(&d, b"CAF__B~1FLA");
    }

    #[test]
    fn lfn_round_trips_through_the_parser() {
        let mut taken = HashSet::new();
        let name = "12 - Last Exit Before the Bridge (Live).mp3";
        let g = entry_group(name, &mut taken, ATTR_ARCHIVE, 1234, 5678, FatTimestamp::from_unix(1_700_000_000));
        let mut bytes = Vec::new();
        for e in &g {
            bytes.extend_from_slice(e);
        }
        bytes.extend_from_slice(&[0u8; 32]);
        let l = parse_directory(&bytes, DirRef::Root);
        assert_eq!(l.orphan_lfn_slots, 0);
        assert_eq!(l.groups.len(), 1);
        assert_eq!(l.groups[0].name(), name);
        assert_eq!(l.groups[0].first_cluster, 1234);
        assert_eq!(l.groups[0].size, 5678);
    }

    #[test]
    fn fat12_entries_pack_and_unpack() {
        let mut t = FatTable { data: vec![0u8; 64], orig: vec![0u8; 64], fat_type: FatType::Fat12, max_cluster: 40 };
        for n in 2..40u32 {
            t.set(n, (n * 37) & 0xFFF);
        }
        for n in 2..40u32 {
            assert_eq!(t.get(n), (n * 37) & 0xFFF, "entry {n}");
        }
    }

    #[test]
    fn fat_dates() {
        // 2026-10-27 12:34:56 UTC
        let t = FatTimestamp::from_unix(1_793_104_496);
        assert_eq!(t.date >> 9, 2026 - 1980);
        assert_eq!((t.date >> 5) & 0xF, 10);
        assert_eq!(t.date & 0x1F, 27);
        assert_eq!(t.time >> 11, 12);
        assert_eq!((t.time >> 5) & 0x3F, 34);
        assert_eq!((t.time & 0x1F) * 2, 56);
    }
}
