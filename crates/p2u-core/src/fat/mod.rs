//! Minimal FAT12/16/32 access for one job: permuting the entries of a
//! directory in place so that the raw on-disk order matches a playlist.
//!
//! Design rules (they are what makes this safe on a real USB stick):
//! * The FAT, the FSInfo sector and the data clusters of files are never
//!   written. Only the bytes of directory clusters that the directory already
//!   owns are rewritten, so no allocation can be corrupted.
//! * Every read and write is whole sectors at sector-aligned offsets, as raw
//!   devices on macOS (`/dev/rdiskN`) and Windows (`\\.\E:`) require.
//! * A directory with inconsistencies (orphaned long-name entries, bad
//!   checksums, broken cluster chains) is refused unless `force` is set.
//!
//! The approach follows the one fatsort uses (walk the directory's cluster
//! chain, group LFN entries with their short entry, write the groups back into
//! the same clusters), implemented from the Microsoft FAT specification.

mod dir;
mod reorder;

pub use dir::{DirEntryGroup, DirListing, EntryKind};
pub use reorder::{
    check_order, is_junk_name, normalize_name, Backup, OrderCheck, PreparedReorder, ReorderOptions, ReorderReport,
    UnmatchedPlacement,
};

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};

use serde::Serialize;

/// A block device or image the volume lives on.
pub trait Device: Read + Write + Seek {
    /// Push written data to the medium.
    fn sync_device(&mut self) -> io::Result<()>;
    /// Drop any OS cache of this device so the next read comes from the medium.
    fn invalidate_cache(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Device for std::fs::File {
    fn sync_device(&mut self) -> io::Result<()> {
        self.flush()?;
        self.sync_all()
    }
    fn invalidate_cache(&mut self) -> io::Result<()> {
        drop_page_cache(self)
    }
}

/// Linux block devices are read through the page cache; after an fsync the
/// cached pages are clean, and POSIX_FADV_DONTNEED evicts them (no privileges
/// needed), so a verification read really comes from the stick.
#[cfg(target_os = "linux")]
pub fn drop_page_cache(f: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    f.sync_all()?;
    // SAFETY: plain syscall on a valid descriptor.
    let r = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if r != 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    Ok(())
}

/// macOS raw disk nodes and Windows volume handles are not page-cached.
#[cfg(not(target_os = "linux"))]
pub fn drop_page_cache(_f: &std::fs::File) -> io::Result<()> {
    Ok(())
}

impl Device for std::io::Cursor<Vec<u8>> {
    fn sync_device(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<T: Device + ?Sized> Device for &mut T {
    fn sync_device(&mut self) -> io::Result<()> {
        (**self).sync_device()
    }
    fn invalidate_cache(&mut self) -> io::Result<()> {
        (**self).invalidate_cache()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FatError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("no FAT file system found: {0}")]
    NotFat(String),
    #[error("path not found on the volume: {0}")]
    NotFound(String),
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("refusing to write: {0} (use force to override)")]
    Unsafe(String),
    #[error("corrupt file system: {0}")]
    Corrupt(String),
    #[error("verification after write failed: {0}")]
    VerifyFailed(String),
}

pub type Result<T> = std::result::Result<T, FatError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

/// The parsed BIOS parameter block plus derived geometry.
#[derive(Debug, Clone, Serialize)]
pub struct Geometry {
    pub fat_type: FatType,
    /// Byte offset of the volume on the device (non-zero when a partition table was followed).
    pub partition_offset: u64,
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub reserved_sectors: u32,
    pub num_fats: u32,
    pub fat_size_sectors: u32,
    pub root_entry_count: u32,
    pub root_dir_sectors: u32,
    pub total_sectors: u32,
    pub first_data_sector: u32,
    pub cluster_count: u32,
    /// FAT32 only: first cluster of the root directory.
    pub root_cluster: u32,
    /// Which FAT copy is authoritative (FAT32 may disable mirroring).
    pub active_fat: u32,
    pub volume_serial: u32,
    pub volume_label: String,
    pub oem_name: String,
}

impl Geometry {
    pub fn cluster_bytes(&self) -> u64 {
        self.bytes_per_sector as u64 * self.sectors_per_cluster as u64
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        let sector = self.first_data_sector as u64 + (cluster as u64 - 2) * self.sectors_per_cluster as u64;
        self.partition_offset + sector * self.bytes_per_sector as u64
    }

    fn max_cluster(&self) -> u32 {
        self.cluster_count + 1
    }

    fn eoc_min(&self) -> u32 {
        match self.fat_type {
            FatType::Fat12 => 0xFF8,
            FatType::Fat16 => 0xFFF8,
            FatType::Fat32 => 0x0FFF_FFF8,
        }
    }
}

/// Health flags read from the boot sector and FAT[1].
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    /// Linux sets this bit in the boot sector while the volume is mounted.
    pub boot_sector_dirty: bool,
    /// Windows clears the "clean shutdown" bit in FAT[1] while mounted.
    pub fat_clean_shutdown: bool,
    pub fat_hard_error: bool,
    /// All FAT copies identical (only checked when `compare_fats` was requested).
    pub fats_identical: Option<bool>,
}

/// Where a directory's entries live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DirRef {
    /// Fixed root directory region of FAT12/16, or the root cluster chain of FAT32.
    Root,
    /// A directory stored in a cluster chain starting at this cluster.
    Cluster(u32),
}

/// One sector-aligned byte range on the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Extent {
    pub offset: u64,
    pub len: u64,
}

pub struct Volume<D: Device> {
    dev: D,
    geo: Geometry,
    fat_cache: HashMap<u64, Vec<u8>>,
}

const FAT_PARTITION_TYPES: [u8; 6] = [0x01, 0x04, 0x06, 0x0B, 0x0C, 0x0E];
// Microsoft basic data partition GUID in on-disk (mixed-endian) byte order.
const GPT_BASIC_DATA: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

fn le16(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn looks_like_bpb(s: &[u8]) -> bool {
    if s.len() < 512 {
        return false;
    }
    let jump_ok = s[0] == 0xEB || s[0] == 0xE9;
    let bps = le16(s, 11);
    let spc = s[13] as u32;
    let rsvd = le16(s, 14);
    let nfats = s[16];
    let media = s[21];
    let tot = if le16(s, 19) != 0 { le16(s, 19) } else { le32(s, 32) };
    jump_ok
        && matches!(bps, 512 | 1024 | 2048 | 4096)
        && spc.is_power_of_two()
        && spc <= 128
        && rsvd >= 1
        && (1..=4).contains(&nfats)
        && (media == 0xF0 || media >= 0xF8)
        && tot != 0
}

fn read_exact_at<D: Device>(dev: &mut D, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    dev.seek(SeekFrom::Start(offset))?;
    dev.read_exact(buf)
}

/// Read the first 4 KiB of the device. 4096 is a multiple of every legal sector
/// size, so this is an aligned read on 512-byte and 4Kn devices alike.
fn read_probe<D: Device>(dev: &mut D, offset: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    read_exact_at(dev, offset, &mut buf)?;
    Ok(buf)
}

/// Find the byte offset of the FAT volume on a device: either the device itself
/// is the volume (a partition node, a "superfloppy" stick or an image made with
/// mkfs.fat), or follow an MBR/GPT partition table to the requested FAT partition.
pub fn locate_volume<D: Device>(dev: &mut D, partition: Option<usize>) -> Result<u64> {
    let s0 = read_probe(dev, 0)?;
    if partition.is_none() && looks_like_bpb(&s0) {
        return Ok(0);
    }
    if s0[510] != 0x55 || s0[511] != 0xAA {
        return Err(FatError::NotFat("no boot sector or partition table signature".into()));
    }
    let mut candidates: Vec<u64> = Vec::new();
    let is_gpt = (0..4).any(|i| s0[446 + 16 * i + 4] == 0xEE);
    if is_gpt {
        let hdr = &s0[512..1024];
        if &hdr[0..8] != b"EFI PART" {
            return Err(FatError::NotFat("protective MBR without GPT header (non-512-byte LBA?)".into()));
        }
        let entries_lba = le64(hdr, 72);
        let n = le32(hdr, 80) as u64;
        let esz = le32(hdr, 84) as u64;
        let bytes = n * esz;
        let aligned = bytes.div_ceil(4096) * 4096;
        let mut table = vec![0u8; aligned as usize];
        read_exact_at(dev, entries_lba * 512, &mut table)?;
        for i in 0..n as usize {
            let e = &table[i * esz as usize..(i + 1) * esz as usize];
            if e[0..16] == GPT_BASIC_DATA {
                candidates.push(le64(e, 32) * 512);
            }
        }
    } else {
        for i in 0..4 {
            let e = &s0[446 + 16 * i..446 + 16 * (i + 1)];
            if FAT_PARTITION_TYPES.contains(&e[4]) {
                candidates.push(le32(e, 8) as u64 * 512);
            }
        }
    }
    let mut fat_parts = Vec::new();
    for off in candidates {
        if looks_like_bpb(&read_probe(dev, off)?) {
            fat_parts.push(off);
        }
    }
    match partition {
        Some(p) => fat_parts
            .get(p.saturating_sub(1))
            .copied()
            .ok_or_else(|| FatError::NotFat(format!("FAT partition {p} not found ({} found)", fat_parts.len()))),
        None => fat_parts
            .first()
            .copied()
            .ok_or_else(|| FatError::NotFat("no FAT partition in the partition table".into())),
    }
}

fn parse_geometry(s: &[u8], partition_offset: u64) -> Result<Geometry> {
    if !looks_like_bpb(s) {
        return Err(FatError::NotFat("boot sector is not a FAT BPB".into()));
    }
    let bps = le16(s, 11);
    let spc = s[13] as u32;
    let rsvd = le16(s, 14);
    let nfats = s[16] as u32;
    let root_ent = le16(s, 17);
    let tot = if le16(s, 19) != 0 { le16(s, 19) } else { le32(s, 32) };
    let fatsz = if le16(s, 22) != 0 { le16(s, 22) } else { le32(s, 36) };
    if fatsz == 0 {
        return Err(FatError::NotFat("FAT size is zero".into()));
    }
    let root_dir_sectors = (root_ent * 32).div_ceil(bps);
    let meta = rsvd + nfats * fatsz + root_dir_sectors;
    if meta >= tot {
        return Err(FatError::NotFat("metadata larger than volume".into()));
    }
    let cluster_count = (tot - meta) / spc;
    // The spec decides by cluster count alone, but a BPB with a zero 16-bit FAT
    // size and no fixed root is laid out as FAT32 whatever its size; Linux and
    // fatsort decide by layout too, and some formatters produce such volumes.
    let fat_type = if le16(s, 22) == 0 && root_ent == 0 {
        FatType::Fat32
    } else if cluster_count < 4085 {
        FatType::Fat12
    } else if cluster_count < 65525 {
        FatType::Fat16
    } else {
        FatType::Fat32
    };
    let (root_cluster, active_fat, serial_off, label_off) = if fat_type == FatType::Fat32 {
        let ext = le16(s, 40);
        let active = if ext & 0x80 != 0 { ext & 0x0F } else { 0 };
        (le32(s, 44), active, 67, 71)
    } else {
        (0, 0, 39, 43)
    };
    if fat_type == FatType::Fat32 && root_ent != 0 {
        return Err(FatError::NotFat("FAT32 with a fixed root directory".into()));
    }
    let label = String::from_utf8_lossy(&s[label_off..label_off + 11]).trim_end().to_string();
    Ok(Geometry {
        fat_type,
        partition_offset,
        bytes_per_sector: bps,
        sectors_per_cluster: spc,
        reserved_sectors: rsvd,
        num_fats: nfats,
        fat_size_sectors: fatsz,
        root_entry_count: root_ent,
        root_dir_sectors,
        total_sectors: tot,
        first_data_sector: meta,
        cluster_count,
        root_cluster,
        active_fat,
        volume_serial: le32(s, serial_off),
        volume_label: label,
        oem_name: String::from_utf8_lossy(&s[3..11]).trim_end().to_string(),
    })
}

impl<D: Device> Volume<D> {
    /// Open the FAT volume found on `dev` (see [`locate_volume`]).
    pub fn open(mut dev: D, partition: Option<usize>) -> Result<Self> {
        let off = locate_volume(&mut dev, partition)?;
        let s = read_probe(&mut dev, off)?;
        let geo = parse_geometry(&s, off)?;
        Ok(Volume { dev, geo, fat_cache: HashMap::new() })
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geo
    }

    pub fn into_inner(self) -> D {
        self.dev
    }

    fn sector_bytes(&self) -> u64 {
        self.geo.bytes_per_sector as u64
    }

    fn check_aligned(&self, offset: u64, len: u64) -> io::Result<()> {
        let bps = self.sector_bytes();
        if !offset.is_multiple_of(bps) || !len.is_multiple_of(bps) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unaligned raw I/O"));
        }
        Ok(())
    }

    pub(crate) fn read_extent(&mut self, e: Extent, buf: &mut [u8]) -> io::Result<()> {
        self.check_aligned(e.offset, e.len)?;
        read_exact_at(&mut self.dev, e.offset, buf)
    }

    pub(crate) fn write_extent(&mut self, e: Extent, buf: &[u8]) -> io::Result<()> {
        self.check_aligned(e.offset, e.len)?;
        self.dev.seek(SeekFrom::Start(e.offset))?;
        self.dev.write_all(buf)
    }

    pub(crate) fn sync(&mut self) -> io::Result<()> {
        self.dev.sync_device()
    }

    pub(crate) fn invalidate_cache(&mut self) -> io::Result<()> {
        self.fat_cache.clear();
        self.dev.invalidate_cache()
    }

    /// Forget cached FAT sectors and drop the OS cache of the device, so the
    /// next reads come from the medium.
    pub fn drop_caches(&mut self) -> Result<()> {
        Ok(self.invalidate_cache()?)
    }

    /// Read one sector of the active FAT (cached).
    fn fat_sector(&mut self, fat_index: u32, sector: u64) -> io::Result<&[u8]> {
        let key = ((fat_index as u64) << 40) | sector;
        if !self.fat_cache.contains_key(&key) {
            let bps = self.sector_bytes();
            let base = self.geo.partition_offset
                + (self.geo.reserved_sectors as u64 + fat_index as u64 * self.geo.fat_size_sectors as u64) * bps;
            let mut buf = vec![0u8; bps as usize];
            read_exact_at(&mut self.dev, base + sector * bps, &mut buf)?;
            self.fat_cache.insert(key, buf);
        }
        Ok(self.fat_cache.get(&key).unwrap())
    }

    fn fat_bytes(&mut self, fat_index: u32, byte_off: u64, n: usize) -> io::Result<[u8; 4]> {
        let bps = self.sector_bytes();
        let mut out = [0u8; 4];
        for (i, slot) in out.iter_mut().enumerate().take(n) {
            let b = byte_off + i as u64;
            *slot = self.fat_sector(fat_index, b / bps)?[(b % bps) as usize];
        }
        Ok(out)
    }

    /// Value of FAT entry `n` in the active FAT.
    pub fn fat_entry(&mut self, n: u32) -> io::Result<u32> {
        self.fat_entry_in(self.geo.active_fat, n)
    }

    fn fat_entry_in(&mut self, fat_index: u32, n: u32) -> io::Result<u32> {
        Ok(match self.geo.fat_type {
            FatType::Fat32 => u32::from_le_bytes(self.fat_bytes(fat_index, n as u64 * 4, 4)?) & 0x0FFF_FFFF,
            FatType::Fat16 => {
                let b = self.fat_bytes(fat_index, n as u64 * 2, 2)?;
                u16::from_le_bytes([b[0], b[1]]) as u32
            }
            FatType::Fat12 => {
                let b = self.fat_bytes(fat_index, n as u64 + n as u64 / 2, 2)?;
                let v = u16::from_le_bytes([b[0], b[1]]) as u32;
                if n & 1 == 1 {
                    v >> 4
                } else {
                    v & 0x0FFF
                }
            }
        })
    }

    /// Follow a cluster chain, detecting loops and out-of-range links.
    pub fn cluster_chain(&mut self, start: u32) -> Result<Vec<u32>> {
        let max = self.geo.max_cluster();
        let eoc = self.geo.eoc_min();
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut c = start;
        loop {
            if c < 2 || c > max {
                return Err(FatError::Corrupt(format!("cluster {c} out of range in chain from {start}")));
            }
            if !seen.insert(c) {
                return Err(FatError::Corrupt(format!("loop in cluster chain from {start}")));
            }
            chain.push(c);
            let next = self.fat_entry(c)?;
            if next >= eoc {
                break;
            }
            if next == eoc - 1 {
                return Err(FatError::Corrupt(format!("bad-cluster mark in chain from {start}")));
            }
            c = next;
        }
        Ok(chain)
    }

    /// Byte ranges holding a directory's entries, in order.
    pub fn dir_extents(&mut self, d: DirRef) -> Result<Vec<Extent>> {
        match (d, self.geo.fat_type) {
            (DirRef::Root, FatType::Fat12 | FatType::Fat16) => {
                let bps = self.sector_bytes();
                let start = self.geo.reserved_sectors as u64 + self.geo.num_fats as u64 * self.geo.fat_size_sectors as u64;
                Ok(vec![Extent {
                    offset: self.geo.partition_offset + start * bps,
                    len: self.geo.root_dir_sectors as u64 * bps,
                }])
            }
            (DirRef::Root, FatType::Fat32) => self.chain_extents(self.geo.root_cluster),
            (DirRef::Cluster(c), _) => self.chain_extents(c),
        }
    }

    fn chain_extents(&mut self, start: u32) -> Result<Vec<Extent>> {
        let cb = self.geo.cluster_bytes();
        Ok(self
            .cluster_chain(start)?
            .into_iter()
            .map(|c| Extent { offset: self.geo.cluster_offset(c), len: cb })
            .collect())
    }

    pub(crate) fn read_extents(&mut self, extents: &[Extent]) -> Result<Vec<u8>> {
        let total: u64 = extents.iter().map(|e| e.len).sum();
        let mut buf = vec![0u8; total as usize];
        let mut pos = 0usize;
        for e in extents {
            let end = pos + e.len as usize;
            self.read_extent(*e, &mut buf[pos..end])?;
            pos = end;
        }
        Ok(buf)
    }

    pub(crate) fn write_extents(&mut self, extents: &[Extent], data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        for e in extents {
            let end = pos + e.len as usize;
            self.write_extent(*e, &data[pos..end])?;
            pos = end;
        }
        self.sync()?;
        Ok(())
    }

    /// Parse a directory into entry groups.
    pub fn list_dir(&mut self, d: DirRef) -> Result<DirListing> {
        let extents = self.dir_extents(d)?;
        let bytes = self.read_extents(&extents)?;
        Ok(dir::parse_directory(&bytes, d))
    }

    /// Resolve a path like `/Music/Road Trip` (case-insensitive, long or short
    /// names, `/` or `\` separators) to a directory.
    pub fn find_dir(&mut self, path: &str) -> Result<DirRef> {
        let mut cur = DirRef::Root;
        for comp in path.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".") {
            let listing = self.list_dir(cur)?;
            let want = normalize_name(comp);
            let g = listing
                .groups
                .iter()
                .find(|g| {
                    matches!(g.kind, EntryKind::Dir | EntryKind::File)
                        && (normalize_name(g.name()) == want || normalize_name(&g.short_name) == want)
                })
                .ok_or_else(|| FatError::NotFound(path.to_string()))?;
            if g.kind != EntryKind::Dir {
                return Err(FatError::NotADirectory(path.to_string()));
            }
            cur = if g.first_cluster == 0 { DirRef::Root } else { DirRef::Cluster(g.first_cluster) };
        }
        Ok(cur)
    }

    /// Mount-state and consistency flags. `compare_fats` reads every FAT copy in full.
    pub fn health(&mut self, compare_fats: bool) -> Result<Health> {
        let s = read_probe(&mut self.dev, self.geo.partition_offset)?;
        let state_off = if self.geo.fat_type == FatType::Fat32 { 0x41 } else { 0x25 };
        let boot_sector_dirty = s[state_off] & 0x01 != 0;
        let fat1 = self.fat_entry(1)?;
        let (clean, hard) = match self.geo.fat_type {
            FatType::Fat32 => (fat1 & 0x0800_0000 != 0, fat1 & 0x0400_0000 == 0),
            FatType::Fat16 => (fat1 & 0x8000 != 0, fat1 & 0x4000 == 0),
            FatType::Fat12 => (true, false),
        };
        let fats_identical = if compare_fats && self.geo.num_fats > 1 {
            let bps = self.sector_bytes();
            let len = self.geo.fat_size_sectors as u64 * bps;
            let chunk = 1u64 << 20;
            let mut same = true;
            'outer: for i in 1..self.geo.num_fats {
                let mut off = 0;
                while off < len {
                    let n = chunk.min(len - off);
                    let base = |k: u32| {
                        self.geo.partition_offset
                            + (self.geo.reserved_sectors as u64 + k as u64 * self.geo.fat_size_sectors as u64) * bps
                            + off
                    };
                    let (b0, bi) = (base(0), base(i));
                    let mut a = vec![0u8; n as usize];
                    let mut b = vec![0u8; n as usize];
                    read_exact_at(&mut self.dev, b0, &mut a)?;
                    read_exact_at(&mut self.dev, bi, &mut b)?;
                    // FAT[0] and FAT[1] hold media/state flags that legitimately
                    // differ between copies on some drivers; compare from entry 2.
                    let skip = if off == 0 {
                        match self.geo.fat_type {
                            FatType::Fat32 => 8,
                            FatType::Fat16 => 4,
                            FatType::Fat12 => 3,
                        }
                    } else {
                        0
                    };
                    if a[skip..] != b[skip..] {
                        same = false;
                        break 'outer;
                    }
                    off += n;
                }
            }
            Some(same)
        } else {
            None
        };
        Ok(Health { boot_sector_dirty, fat_clean_shutdown: clean, fat_hard_error: hard, fats_identical })
    }
}
