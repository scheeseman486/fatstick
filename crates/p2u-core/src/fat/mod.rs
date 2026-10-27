//! FAT32 on a USB stick: building a fresh stick (see [`stick`]) and reading
//! one back to check it.
//!
//! Only FAT32 is supported. The players this is for implement FAT32 and
//! little else, and because every write erases the stick and lays down a new
//! file system, there is never an existing layout to adapt to.
//!
//! Every read and write is whole sectors at sector-aligned offsets, as raw
//! devices on macOS (`/dev/rdiskN`) and Windows (`\\.\PhysicalDriveN`) require.

mod dir;
pub mod entries;
pub mod stick;

pub use dir::{DirEntryGroup, DirListing, EntryKind};
pub use entries::{check_order, normalize_name, written_name, OrderCheck};

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};

use serde::Serialize;

/// A whole disk (or disk image) the stick's file system lives on.
pub trait Device: Read + Write + Seek {
    /// Push written data to the medium.
    fn sync_device(&mut self) -> io::Result<()>;
    /// Drop any OS cache of this device so the next read comes from the medium.
    fn invalidate_cache(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Size of the whole device in bytes.
    fn disk_size(&mut self) -> io::Result<u64> {
        let end = self.seek(SeekFrom::End(0))?;
        self.seek(SeekFrom::Start(0))?;
        Ok(end)
    }
    /// Logical sector size the device expects I/O in.
    fn logical_sector_size(&mut self) -> io::Result<u32> {
        Ok(512)
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

/// macOS raw disk nodes and Windows physical drive handles are not page-cached.
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
    fn disk_size(&mut self) -> io::Result<u64> {
        (**self).disk_size()
    }
    fn logical_sector_size(&mut self) -> io::Result<u32> {
        (**self).logical_sector_size()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FatError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("no FAT32 file system found: {0}")]
    NotFat(String),
    #[error("path not found on the stick: {0}")]
    NotFound(String),
    #[error("not a folder: {0}")]
    NotADirectory(String),
    #[error("{0}")]
    Invalid(String),
    #[error("corrupt file system: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, FatError>;

/// The parsed FAT32 boot sector plus derived geometry.
#[derive(Debug, Clone, Serialize)]
pub struct Geometry {
    /// Byte offset of the volume on the device (non-zero when a partition table was followed).
    pub partition_offset: u64,
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub reserved_sectors: u32,
    pub num_fats: u32,
    pub fat_size_sectors: u32,
    pub total_sectors: u32,
    pub first_data_sector: u32,
    pub cluster_count: u32,
    pub root_cluster: u32,
    /// Which FAT copy is authoritative (FAT32 may disable mirroring).
    pub active_fat: u32,
    pub fat_mirroring: bool,
    /// Sector of the FSInfo structure (0 = none).
    pub fs_info_sector: u32,
    pub backup_boot_sector: u32,
    pub volume_serial: u32,
    pub volume_label: String,
    pub oem_name: String,
}

impl Geometry {
    pub fn cluster_bytes(&self) -> u64 {
        self.bytes_per_sector as u64 * self.sectors_per_cluster as u64
    }

    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        let sector = self.first_data_sector as u64 + (cluster as u64 - 2) * self.sectors_per_cluster as u64;
        self.partition_offset + sector * self.bytes_per_sector as u64
    }

    pub(crate) fn max_cluster(&self) -> u32 {
        self.cluster_count + 1
    }

    /// Byte offset of FAT copy `k`.
    pub fn fat_offset(&self, k: u32) -> u64 {
        self.partition_offset
            + (self.reserved_sectors as u64 + k as u64 * self.fat_size_sectors as u64) * self.bytes_per_sector as u64
    }
}

/// Mount-state and consistency flags read from the boot sector and FAT[1].
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
    /// The root directory's cluster chain.
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

/// MBR partition types that hold FAT32 (CHS and LBA addressing).
const FAT32_PARTITION_TYPES: [u8; 2] = [0x0B, 0x0C];

pub(crate) fn le16(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}
pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
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

/// Read 4 KiB at `offset`. 4096 is a multiple of every legal sector size, so
/// this is an aligned read on 512-byte and 4Kn devices alike.
fn read_probe<D: Device>(dev: &mut D, offset: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    read_exact_at(dev, offset, &mut buf)?;
    Ok(buf)
}

/// Find the byte offset of the FAT32 volume on a device: either the device
/// itself is the volume (a partition node or a "superfloppy" image), or the
/// first FAT32 partition of an MBR partition table.
pub fn locate_volume<D: Device>(dev: &mut D) -> Result<u64> {
    let s0 = read_probe(dev, 0)?;
    if looks_like_bpb(&s0) {
        return Ok(0);
    }
    if s0[510] != 0x55 || s0[511] != 0xAA {
        return Err(FatError::NotFat("no partition table or boot sector (the stick looks blank)".into()));
    }
    for i in 0..4 {
        let e = &s0[446 + 16 * i..446 + 16 * (i + 1)];
        if FAT32_PARTITION_TYPES.contains(&e[4]) {
            let off = le32(e, 8) as u64 * 512;
            if looks_like_bpb(&read_probe(dev, off)?) {
                return Ok(off);
            }
        }
    }
    Err(FatError::NotFat("no FAT32 partition in the partition table".into()))
}

fn parse_geometry(s: &[u8], partition_offset: u64) -> Result<Geometry> {
    if !looks_like_bpb(s) {
        return Err(FatError::NotFat("boot sector is not a FAT boot sector".into()));
    }
    let bps = le16(s, 11);
    let spc = s[13] as u32;
    let rsvd = le16(s, 14);
    let nfats = s[16] as u32;
    let root_ent = le16(s, 17);
    let tot = if le16(s, 19) != 0 { le16(s, 19) } else { le32(s, 32) };
    let fatsz = if le16(s, 22) != 0 { le16(s, 22) } else { le32(s, 36) };
    if le16(s, 22) != 0 || root_ent != 0 || fatsz == 0 {
        return Err(FatError::NotFat("this is FAT12 or FAT16, not FAT32".into()));
    }
    let meta = rsvd + nfats * fatsz;
    if meta >= tot {
        return Err(FatError::NotFat("metadata larger than volume".into()));
    }
    let cluster_count = (tot - meta) / spc;
    if cluster_count < 65525 {
        return Err(FatError::NotFat(format!("only {cluster_count} clusters; FAT32 needs at least 65525")));
    }
    let ext = le16(s, 40);
    let mirrored = ext & 0x80 == 0;
    let active = if mirrored { 0 } else { ext & 0x0F };
    let fsi = le16(s, 48);
    let bk = le16(s, 50);
    let label = String::from_utf8_lossy(&s[71..82]).trim_end().to_string();
    Ok(Geometry {
        partition_offset,
        bytes_per_sector: bps,
        sectors_per_cluster: spc,
        reserved_sectors: rsvd,
        num_fats: nfats,
        fat_size_sectors: fatsz,
        total_sectors: tot,
        first_data_sector: meta,
        cluster_count,
        root_cluster: le32(s, 44),
        active_fat: active,
        fat_mirroring: mirrored,
        fs_info_sector: if fsi == 0 || fsi == 0xFFFF { 0 } else { fsi },
        backup_boot_sector: if bk == 0xFFFF { 0 } else { bk },
        volume_serial: le32(s, 67),
        volume_label: label,
        oem_name: String::from_utf8_lossy(&s[3..11]).trim_end().to_string(),
    })
}

impl<D: Device> Volume<D> {
    /// Open the FAT32 volume found on `dev` (see [`locate_volume`]).
    pub fn open(mut dev: D) -> Result<Self> {
        let off = locate_volume(&mut dev)?;
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

    /// Read a sector-aligned range.
    pub fn read_extent(&mut self, e: Extent, buf: &mut [u8]) -> io::Result<()> {
        let bps = self.sector_bytes();
        if !e.offset.is_multiple_of(bps) || !e.len.is_multiple_of(bps) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unaligned raw I/O"));
        }
        read_exact_at(&mut self.dev, e.offset, buf)
    }

    /// Forget cached FAT sectors and drop the OS cache of the device, so the
    /// next reads come from the medium.
    pub fn drop_caches(&mut self) -> Result<()> {
        self.fat_cache.clear();
        Ok(self.dev.invalidate_cache()?)
    }

    /// Read one sector of a FAT copy (cached).
    fn fat_sector(&mut self, fat_index: u32, sector: u64) -> io::Result<&[u8]> {
        let key = ((fat_index as u64) << 40) | sector;
        if !self.fat_cache.contains_key(&key) {
            let bps = self.sector_bytes();
            let mut buf = vec![0u8; bps as usize];
            read_exact_at(&mut self.dev, self.geo.fat_offset(fat_index) + sector * bps, &mut buf)?;
            self.fat_cache.insert(key, buf);
        }
        Ok(self.fat_cache.get(&key).unwrap())
    }

    /// Value of FAT entry `n` in the active FAT.
    pub fn fat_entry(&mut self, n: u32) -> io::Result<u32> {
        let bps = self.sector_bytes();
        let b = n as u64 * 4;
        let s = self.fat_sector(self.geo.active_fat, b / bps)?;
        let o = (b % bps) as usize;
        Ok(u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]) & 0x0FFF_FFFF)
    }

    /// Follow a cluster chain, detecting loops and out-of-range links.
    pub fn cluster_chain(&mut self, start: u32) -> Result<Vec<u32>> {
        let max = self.geo.max_cluster();
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
            if next >= 0x0FFF_FFF8 {
                break;
            }
            if next == 0x0FFF_FFF7 {
                return Err(FatError::Corrupt(format!("bad-cluster mark in chain from {start}")));
            }
            c = next;
        }
        Ok(chain)
    }

    /// Byte ranges holding a directory's entries, in order.
    pub fn dir_extents(&mut self, d: DirRef) -> Result<Vec<Extent>> {
        let start = match d {
            DirRef::Root => self.geo.root_cluster,
            DirRef::Cluster(c) => c,
        };
        let cb = self.geo.cluster_bytes();
        Ok(self
            .cluster_chain(start)?
            .into_iter()
            .map(|c| Extent { offset: self.geo.cluster_offset(c), len: cb })
            .collect())
    }

    pub fn read_extents(&mut self, extents: &[Extent]) -> Result<Vec<u8>> {
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

    /// Read `len` bytes of FAT copy `k` starting at byte `off` within the FAT.
    pub fn read_fat_bytes(&mut self, k: u32, off: u64, buf: &mut [u8]) -> io::Result<()> {
        let base = self.geo.fat_offset(k);
        read_exact_at(&mut self.dev, base + off, buf)
    }

    /// Read whole sectors at an absolute device offset.
    pub fn read_raw(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(&mut self.dev, offset, buf)
    }

    /// Mount-state and consistency flags. `compare_fats` reads every FAT copy in full.
    pub fn health(&mut self, compare_fats: bool) -> Result<Health> {
        let s = read_probe(&mut self.dev, self.geo.partition_offset)?;
        let boot_sector_dirty = s[0x41] & 0x01 != 0;
        let fat1 = self.fat_entry(1)?;
        let (clean, hard) = (fat1 & 0x0800_0000 != 0, fat1 & 0x0400_0000 == 0);
        let fats_identical = if compare_fats && self.geo.num_fats > 1 {
            let len = self.geo.fat_size_sectors as u64 * self.sector_bytes();
            let chunk = 1u64 << 20;
            let mut same = true;
            'outer: for i in 1..self.geo.num_fats {
                let mut off = 0;
                while off < len {
                    let n = chunk.min(len - off);
                    let mut a = vec![0u8; n as usize];
                    let mut b = vec![0u8; n as usize];
                    self.read_fat_bytes(0, off, &mut a)?;
                    self.read_fat_bytes(i, off, &mut b)?;
                    if a != b {
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

/// Test tool: a device that fails every write after the first `n`, to check
/// what an interruption at any point leaves behind.
pub struct FaultInjector<D: Device> {
    inner: D,
    remaining: Option<u64>,
}

impl<D: Device> FaultInjector<D> {
    pub fn new(inner: D, fail_after_writes: Option<u64>) -> Self {
        FaultInjector { inner, remaining: fail_after_writes }
    }
}

impl<D: Device> Read for FaultInjector<D> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<D: Device> Write for FaultInjector<D> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.remaining {
            Some(0) => Err(io::Error::other("injected fault: the stick was pulled")),
            Some(ref mut n) => {
                *n -= 1;
                self.inner.write(buf)
            }
            None => self.inner.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<D: Device> Seek for FaultInjector<D> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<D: Device> Device for FaultInjector<D> {
    fn sync_device(&mut self) -> io::Result<()> {
        self.inner.sync_device()
    }
    fn invalidate_cache(&mut self) -> io::Result<()> {
        self.inner.invalidate_cache()
    }
    fn disk_size(&mut self) -> io::Result<u64> {
        self.inner.disk_size()
    }
    fn logical_sector_size(&mut self) -> io::Result<u32> {
        self.inner.logical_sector_size()
    }
}
