//! Writing a playlist to a USB stick by erasing it and laying down a new
//! FAT32 file system whose every byte of metadata is chosen here.
//!
//! The stick gets one MBR partition (type 0x0C, FAT32 with LBA) starting at
//! 1 MiB, holding:
//!
//! * the volume label, then the playlist folder (or the tracks themselves) as
//!   the only entries at the top level;
//! * in the folder, `.` and `..`, then one entry per track in playlist order
//!   (FAT order);
//! * the root table, the folder table and then every file's data in one
//!   unbroken run of clusters, back to back in playlist order, from cluster 2.
//!
//! Nothing that was on the stick before is kept or adapted to, so there are
//! no existing layouts to get wrong.
//!
//! Write order, so a pull at any point leaves either a blank stick or the
//! complete one:
//!
//! 1. Erase the first and last MiB of the disk (old partition tables,
//!    including a GPT's backup header, and old file-system signatures).
//!    From here on the stick reads as blank.
//! 2. The partition's reserved sectors (boot sector still zero), both FAT
//!    copies, the directory tables and the file data.
//! 3. The FAT32 boot sector (backup, then primary).
//! 4. The MBR partition table, last. Only now does any OS see a file system.
//!
//! Afterwards every byte of metadata is read back from the medium and
//! compared with what was planned, and every file is compared with its source.

use std::collections::HashSet;
use std::io::{self, Read, SeekFrom};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::dir::{parse_directory, ENTRY};
use super::entries::{
    entry_group, normalize_name, sfn_entry, validate_name, volume_label, written_name, FatTimestamp, ATTR_ARCHIVE,
    ATTR_DIR, ATTR_VOLUME_ID,
};
use super::{Device, DirRef, EntryKind, FatError, Result, Volume};

const SECTOR: u64 = 512;
const MIB: u64 = 1 << 20;
/// Partition start in sectors: 1 MiB, as Windows, macOS and Linux tools align.
pub const PARTITION_START: u64 = 2048;
const RESERVED_MIN: u32 = 32;
const BACKUP_BOOT: u64 = 6;
const EOC: u32 = 0x0FFF_FFFF;
/// MBR sector counts are 32-bit.
const MAX_DISK_SECTORS: u64 = 0xFFFF_FFFF;
/// FAT32 directories hold at most 65536 entries.
const MAX_DIR_SLOTS: usize = 65536;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteItem {
    pub source: PathBuf,
    /// Name to give the file on the stick.
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StickOptions {
    /// Folder at the top of the stick that holds the tracks; empty puts the
    /// tracks at the top level.
    #[serde(default)]
    pub folder: String,
    /// Volume label; empty derives it from the folder name (or "PLAYLIST").
    #[serde(default)]
    pub label: String,
}

/// The file system's shape on this particular stick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Format {
    pub disk_bytes: u64,
    /// First sector of the partition.
    pub partition_start: u64,
    pub partition_sectors: u64,
    pub sectors_per_cluster: u32,
    pub reserved_sectors: u32,
    pub fat_sectors: u32,
    pub cluster_count: u32,
    /// The 11-byte label as text (upper case, space padded).
    pub label: String,
    pub serial: u32,
    pub disk_signature: u32,
}

impl Format {
    pub fn cluster_bytes(&self) -> u64 {
        self.sectors_per_cluster as u64 * SECTOR
    }
    fn partition_offset(&self) -> u64 {
        self.partition_start * SECTOR
    }
    pub fn fat_offset(&self, k: u32) -> u64 {
        (self.partition_start + self.reserved_sectors as u64 + k as u64 * self.fat_sectors as u64) * SECTOR
    }
    pub fn data_offset(&self) -> u64 {
        self.fat_offset(2)
    }
    pub fn cluster_offset(&self, c: u32) -> u64 {
        self.data_offset() + (c as u64 - 2) * self.cluster_bytes()
    }
    fn fat_bytes(&self) -> u64 {
        self.fat_sectors as u64 * SECTOR
    }
    fn label_bytes(&self) -> [u8; 11] {
        let mut b = [b' '; 11];
        let s = self.label.as_bytes();
        b[..s.len().min(11)].copy_from_slice(&s[..s.len().min(11)]);
        b
    }
}

/// Sectors per cluster for a FAT32 volume of `sectors` 512-byte sectors, from
/// the table in Microsoft's FAT specification (`None`: too small for FAT32).
pub fn cluster_sectors_for(sectors: u64) -> Option<u32> {
    Some(match sectors {
        0..=66_600 => return None,
        66_601..=532_480 => 1,
        532_481..=16_777_216 => 8,
        16_777_217..=33_554_432 => 16,
        33_554_433..=67_108_864 => 32,
        _ => 64,
    })
}

/// Choose the partition and FAT32 geometry for a disk of `disk_bytes`.
pub fn choose_format(disk_bytes: u64, sector_size: u32, label: [u8; 11], serial: u32, disk_signature: u32) -> Result<Format> {
    if sector_size != SECTOR as u32 {
        return Err(FatError::Invalid(format!(
            "this stick uses {sector_size}-byte sectors; only 512-byte sectors are supported"
        )));
    }
    let disk_sectors = disk_bytes / SECTOR;
    if disk_sectors > MAX_DISK_SECTORS {
        return Err(FatError::Invalid("the stick is larger than 2 TiB, the limit of FAT32 with an MBR partition table".into()));
    }
    let part = disk_sectors.saturating_sub(PARTITION_START);
    let spc = cluster_sectors_for(part)
        .ok_or_else(|| FatError::Invalid("the stick is smaller than FAT32's minimum of about 35 MB".into()))?;
    // FAT size from the specification's formula (slightly generous).
    let per = 128 * spc as u64 + 1;
    let fat = (part - RESERVED_MIN as u64).div_ceil(per);
    // Align the first cluster to the flash erase block where possible: 4 MiB
    // on larger sticks, 1 MiB otherwise, falling back for very small ones.
    let aligns: &[u64] = if disk_bytes >= 2 << 30 { &[8192, 2048, 1] } else { &[2048, 1] };
    for &align in aligns {
        let base = PARTITION_START + RESERVED_MIN as u64 + 2 * fat;
        let pad = (align - base % align) % align;
        let reserved = RESERVED_MIN as u64 + pad;
        if reserved > 0xFFFF {
            continue;
        }
        let clusters = (part - reserved - 2 * fat) / spc as u64;
        if clusters >= 65_525 && clusters + 2 <= fat * 128 && clusters <= 0x0FFF_FFF5 {
            return Ok(Format {
                disk_bytes,
                partition_start: PARTITION_START,
                partition_sectors: part,
                sectors_per_cluster: spc,
                reserved_sectors: reserved as u32,
                fat_sectors: fat as u32,
                cluster_count: clusters as u32,
                label: String::from_utf8_lossy(&label).into_owned(),
                serial,
                disk_signature,
            });
        }
    }
    Err(FatError::Invalid("no valid FAT32 layout fits this stick".into()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePlace {
    pub name: String,
    pub short_name: String,
    pub size: u64,
    /// 0 for an empty file.
    pub first_cluster: u32,
    pub clusters: u32,
}

/// Everything that will be written, decided before the first write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickPlan {
    pub format: Format,
    /// Folder holding the tracks; empty = top level.
    pub folder: String,
    pub root_clusters: u32,
    /// First cluster and length of the folder's table (0 when at the top level).
    pub folder_first: u32,
    pub folder_clusters: u32,
    pub files: Vec<FilePlace>,
    /// Clusters used in total, all of them from cluster 2 up, without gaps.
    pub used_clusters: u32,
    pub total_bytes: u64,
    pub created_unix: i64,
    #[serde(skip)]
    root_table: Vec<u8>,
    #[serde(skip)]
    folder_table: Vec<u8>,
}

impl StickPlan {
    /// Where the tracks are, e.g. "/Road Trip" or "/".
    pub fn target_dir(&self) -> String {
        format!("/{}", self.folder)
    }
    pub fn names(&self) -> Vec<String> {
        self.files.iter().map(|f| f.name.clone()).collect()
    }
    pub fn free_clusters(&self) -> u32 {
        self.format.cluster_count - self.used_clusters
    }
    /// Cluster runs in allocation order: root, folder, files.
    fn runs(&self) -> Vec<(u32, u32)> {
        let mut r = vec![(2, self.root_clusters)];
        if self.folder_clusters > 0 {
            r.push((self.folder_first, self.folder_clusters));
        }
        r.extend(self.files.iter().filter(|f| f.clusters > 0).map(|f| (f.first_cluster, f.clusters)));
        r
    }

    /// The bytes of FAT entries [byte_off, byte_off + len) as this plan defines them.
    fn fat_chunk(&self, ends: &HashSet<u32>, byte_off: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let first = (byte_off / 4) as u32;
        let last_used = 1 + self.used_clusters; // highest cluster in use
        for i in 0..len / 4 {
            let n = first + i as u32;
            let v = match n {
                0 => 0x0FFF_FFF8,
                1 => 0x0FFF_FFFF,
                n if n <= last_used => {
                    if ends.contains(&n) {
                        EOC
                    } else {
                        n + 1
                    }
                }
                _ => 0,
            };
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    fn chain_ends(&self) -> HashSet<u32> {
        self.runs().into_iter().map(|(f, n)| f + n - 1).collect()
    }
}

fn timestamp_now() -> (i64, u32) {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

/// Volume serial and MBR disk signature, both derived from the time like
/// Windows' format does; never zero.
fn ids(secs: i64, nanos: u32) -> (u32, u32) {
    let x = (secs as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (nanos as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    let a = (x >> 32) as u32 | 1;
    let b = (x as u32).rotate_left(13) | 0x100;
    (a, b)
}

/// Decide everything: geometry, names, the directory tables and where every
/// file goes. Reads only the sizes of the source files.
pub fn plan_stick(disk_bytes: u64, sector_size: u32, items: &[WriteItem], opts: &StickOptions) -> Result<StickPlan> {
    let (now, nanos) = timestamp_now();
    plan_stick_at(disk_bytes, sector_size, items, opts, now, nanos)
}

pub(crate) fn plan_stick_at(
    disk_bytes: u64,
    sector_size: u32,
    items: &[WriteItem],
    opts: &StickOptions,
    now: i64,
    nanos: u32,
) -> Result<StickPlan> {
    let folder = written_name(opts.folder.trim());
    if !folder.is_empty() {
        validate_name(&folder)?;
    }
    let label_src = if !opts.label.trim().is_empty() { opts.label.as_str() } else { folder.as_str() };
    let label = volume_label(label_src);
    let (serial, sig) = ids(now, nanos);
    let format = choose_format(disk_bytes, sector_size, label, serial, sig)?;
    let cb = format.cluster_bytes();

    // Names and sizes.
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(items.len());
    for it in items {
        let name = written_name(&it.name);
        validate_name(&name)?;
        if !seen.insert(normalize_name(&name)) {
            return Err(FatError::Invalid(format!("{name:?} appears twice")));
        }
        let m = std::fs::metadata(&it.source)?;
        if !m.is_file() {
            return Err(FatError::NotFound(it.source.display().to_string()));
        }
        if m.len() > u32::MAX as u64 {
            return Err(FatError::Invalid(format!("{name} is larger than FAT32's 4 GiB file limit")));
        }
        files.push(FilePlace {
            name,
            short_name: String::new(),
            size: m.len(),
            first_cluster: 0,
            clusters: m.len().div_ceil(cb) as u32,
        });
    }

    // Table sizes: build the entries once with placeholder clusters to count slots.
    let ts0 = FatTimestamp::from_unix(now);
    let count_slots = |names: &[&str], attr: u8| -> usize {
        let mut taken = HashSet::new();
        names.iter().map(|n| entry_group(n, &mut taken, attr, 0, 0, ts0).0.len()).sum()
    };
    let file_names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    let (root_slots, folder_slots) = if folder.is_empty() {
        (1 + count_slots(&file_names, ATTR_ARCHIVE), 0)
    } else {
        (1 + count_slots(&[folder.as_str()], ATTR_DIR), 2 + count_slots(&file_names, ATTR_ARCHIVE))
    };
    if root_slots.max(folder_slots) > MAX_DIR_SLOTS {
        return Err(FatError::Invalid("too many tracks for one FAT folder".into()));
    }
    let clusters_for = |slots: usize| ((slots * ENTRY) as u64).div_ceil(cb).max(1) as u32;
    let root_clusters = clusters_for(root_slots);
    let folder_clusters = if folder.is_empty() { 0 } else { clusters_for(folder_slots) };
    let folder_first = if folder.is_empty() { 0 } else { 2 + root_clusters };

    // Files back to back after the tables.
    let mut next = 2 + root_clusters + folder_clusters;
    for f in &mut files {
        if f.clusters > 0 {
            f.first_cluster = next;
            next += f.clusters;
        }
    }
    let used = next - 2;
    if used > format.cluster_count {
        let mb = |c: u64| c * cb / 1_000_000;
        return Err(FatError::Invalid(format!(
            "the playlist needs {} MB but the stick holds {} MB",
            mb(used as u64),
            mb(format.cluster_count as u64)
        )));
    }

    // The tables themselves.
    let label_b = format.label_bytes();
    let file_ts = |i: usize| FatTimestamp::from_unix(now + 2 * i as i64);
    let mut taken_files = HashSet::new();
    let mut file_entries = Vec::new();
    for (i, f) in files.iter_mut().enumerate() {
        let (g, sfn) = entry_group(&f.name, &mut taken_files, ATTR_ARCHIVE, f.first_cluster, f.size as u32, file_ts(i));
        f.short_name = String::from_utf8_lossy(&sfn).into_owned();
        file_entries.extend(g);
    }
    let mut root: Vec<[u8; ENTRY]> = vec![sfn_entry(&label_b, ATTR_VOLUME_ID, 0, 0, ts0)];
    let mut folder_entries = Vec::new();
    if folder.is_empty() {
        root.extend(file_entries);
    } else {
        let mut taken = HashSet::new();
        root.extend(entry_group(&folder, &mut taken, ATTR_DIR, folder_first, 0, ts0).0);
        folder_entries.push(sfn_entry(b".          ", ATTR_DIR, folder_first, 0, ts0));
        folder_entries.push(sfn_entry(b"..         ", ATTR_DIR, 0, 0, ts0));
        folder_entries.extend(file_entries);
    }
    let table = |entries: &[[u8; ENTRY]], clusters: u32| -> Vec<u8> {
        let mut t = vec![0u8; (clusters as u64 * cb) as usize];
        for (i, e) in entries.iter().enumerate() {
            t[i * ENTRY..(i + 1) * ENTRY].copy_from_slice(e);
        }
        t
    };
    let total_bytes = files.iter().map(|f| f.size).sum();
    Ok(StickPlan {
        root_table: table(&root, root_clusters),
        folder_table: if folder.is_empty() { Vec::new() } else { table(&folder_entries, folder_clusters) },
        format,
        folder,
        root_clusters,
        folder_first,
        folder_clusters,
        files,
        used_clusters: used,
        total_bytes,
        created_unix: now,
    })
}

// ---------------------------------------------------------------------------
// The fixed sectors.

fn chs(lba: u64) -> [u8; 3] {
    let (heads, spt) = (255u64, 63u64);
    let c = lba / (heads * spt);
    if c > 1023 {
        return [0xFE, 0xFF, 0xFF];
    }
    let h = (lba / spt) % heads;
    let s = lba % spt + 1;
    [h as u8, (s as u8) | (((c >> 2) & 0xC0) as u8), c as u8]
}

/// The MBR: one FAT32 (LBA) partition covering the disk from 1 MiB.
pub fn mbr_sector(f: &Format) -> [u8; 512] {
    let mut s = [0u8; 512];
    s[440..444].copy_from_slice(&f.disk_signature.to_le_bytes());
    let e = &mut s[446..462];
    e[0] = 0x00;
    e[1..4].copy_from_slice(&chs(f.partition_start));
    e[4] = 0x0C;
    e[5..8].copy_from_slice(&chs(f.partition_start + f.partition_sectors - 1));
    e[8..12].copy_from_slice(&(f.partition_start as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(f.partition_sectors as u32).to_le_bytes());
    s[510] = 0x55;
    s[511] = 0xAA;
    s
}

/// The FAT32 boot sector (BPB), per Microsoft's specification.
pub fn boot_sector(f: &Format) -> [u8; 512] {
    let mut s = [0u8; 512];
    s[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    s[3..11].copy_from_slice(b"MSWIN4.1");
    s[11..13].copy_from_slice(&512u16.to_le_bytes());
    s[13] = f.sectors_per_cluster as u8;
    s[14..16].copy_from_slice(&(f.reserved_sectors as u16).to_le_bytes());
    s[16] = 2;
    s[21] = 0xF8;
    s[24..26].copy_from_slice(&63u16.to_le_bytes());
    s[26..28].copy_from_slice(&255u16.to_le_bytes());
    s[28..32].copy_from_slice(&(f.partition_start as u32).to_le_bytes());
    s[32..36].copy_from_slice(&(f.partition_sectors as u32).to_le_bytes());
    s[36..40].copy_from_slice(&f.fat_sectors.to_le_bytes());
    s[44..48].copy_from_slice(&2u32.to_le_bytes());
    s[48..50].copy_from_slice(&1u16.to_le_bytes());
    s[50..52].copy_from_slice(&(BACKUP_BOOT as u16).to_le_bytes());
    s[64] = 0x80;
    s[66] = 0x29;
    s[67..71].copy_from_slice(&f.serial.to_le_bytes());
    s[71..82].copy_from_slice(&f.label_bytes());
    s[82..90].copy_from_slice(b"FAT32   ");
    // Not bootable: "int 18h" (no boot device), then halt in place.
    s[90..94].copy_from_slice(&[0xCD, 0x18, 0xEB, 0xFE]);
    s[510] = 0x55;
    s[511] = 0xAA;
    s
}

pub fn fsinfo_sector(free: u32, next_free: u32) -> [u8; 512] {
    let mut s = [0u8; 512];
    s[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    s[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    s[488..492].copy_from_slice(&free.to_le_bytes());
    s[492..496].copy_from_slice(&next_free.to_le_bytes());
    s[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    s
}

fn fsinfo_for(plan: &StickPlan) -> [u8; 512] {
    let next = 2 + plan.used_clusters;
    let next = if next <= plan.format.cluster_count + 1 { next } else { 0xFFFF_FFFF };
    fsinfo_sector(plan.free_clusters(), next)
}

// ---------------------------------------------------------------------------
// Writing.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteProgress {
    /// "erase", "write" or "verify".
    pub stage: String,
    pub index: usize,
    pub total: usize,
    pub name: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickResult {
    pub plan: StickPlan,
    pub bytes_written: u64,
}

fn put<D: Device>(dev: &mut D, offset: u64, data: &[u8], written: &mut u64) -> io::Result<()> {
    dev.seek(SeekFrom::Start(offset))?;
    dev.write_all(data)?;
    *written += data.len() as u64;
    Ok(())
}

fn zero<D: Device>(dev: &mut D, offset: u64, len: u64, written: &mut u64) -> io::Result<()> {
    let buf = vec![0u8; MIB.min(len) as usize];
    let mut off = 0;
    while off < len {
        let n = (len - off).min(MIB);
        put(dev, offset + off, &buf[..n as usize], written)?;
        off += n;
    }
    Ok(())
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

/// Erase the stick and write the planned file system and files.
/// `dev` is the whole disk. Everything on it is lost.
pub fn write_stick<D: Device>(
    dev: &mut D,
    plan: &StickPlan,
    items: &[WriteItem],
    progress: &mut dyn FnMut(&WriteProgress),
) -> Result<StickResult> {
    let f = &plan.format;
    if items.len() != plan.files.len() {
        return Err(FatError::Invalid("the plan does not match the playlist".into()));
    }
    let disk = dev.disk_size()?;
    if disk != f.disk_bytes {
        return Err(FatError::Invalid(format!("the stick's size changed ({disk} bytes, planned for {})", f.disk_bytes)));
    }
    let cb = f.cluster_bytes();
    let mut w = 0u64;
    let total = plan.files.len();
    progress(&WriteProgress { stage: "erase".into(), index: 0, total, name: String::new(), bytes_done: 0, bytes_total: 0 });

    // 1. Erase the start (partition tables, boot sectors) and the end (GPT
    //    backup header, end-of-disk signatures). The stick now reads as blank.
    zero(dev, 0, f.partition_start * SECTOR, &mut w)?;
    let tail = MIB.min(disk - f.partition_start * SECTOR) / SECTOR * SECTOR;
    zero(dev, (disk / SECTOR) * SECTOR - tail, tail, &mut w)?;
    dev.sync_device()?;

    // 2a. Reserved sectors: everything zero except FSInfo and its backup; the
    //     boot sector and its backup stay zero until the commit.
    let fsinfo = fsinfo_for(plan);
    let mut reserved = vec![0u8; f.reserved_sectors as usize * SECTOR as usize];
    reserved[512..1024].copy_from_slice(&fsinfo);
    reserved[(BACKUP_BOOT as usize + 1) * 512..(BACKUP_BOOT as usize + 2) * 512].copy_from_slice(&fsinfo);
    let mut off = 0usize;
    while off < reserved.len() {
        let n = (reserved.len() - off).min(MIB as usize);
        put(dev, f.partition_offset() + off as u64, &reserved[off..off + n], &mut w)?;
        off += n;
    }

    // 2b. Both FAT copies.
    let ends = plan.chain_ends();
    for k in 0..2 {
        let mut off = 0u64;
        while off < f.fat_bytes() {
            let n = (f.fat_bytes() - off).min(MIB);
            let chunk = plan.fat_chunk(&ends, off, n as usize);
            put(dev, f.fat_offset(k) + off, &chunk, &mut w)?;
            off += n;
        }
    }

    // 2c. Directory tables.
    put(dev, f.cluster_offset(2), &plan.root_table, &mut w)?;
    if plan.folder_clusters > 0 {
        put(dev, f.cluster_offset(plan.folder_first), &plan.folder_table, &mut w)?;
    }

    // 2d. File data, each file padded with zeros to its last cluster.
    let mut buf = vec![0u8; MIB as usize];
    for (i, (fp, it)) in plan.files.iter().zip(items).enumerate() {
        let mut p = WriteProgress {
            stage: "write".into(),
            index: i,
            total,
            name: fp.name.clone(),
            bytes_done: 0,
            bytes_total: fp.size,
        };
        progress(&p);
        let mut src = std::fs::File::open(&it.source)?;
        if src.metadata()?.len() != fp.size {
            return Err(FatError::Invalid(format!("{} changed size since the plan was made", it.source.display())));
        }
        let span = fp.clusters as u64 * cb;
        let mut done = 0u64;
        while done < span {
            let n = (span - done).min(MIB) as usize;
            let data_n = fp.size.saturating_sub(done).min(n as u64) as usize;
            read_full(&mut src, &mut buf[..data_n])?;
            buf[data_n..n].fill(0);
            put(dev, f.cluster_offset(fp.first_cluster) + done, &buf[..n], &mut w)?;
            done += n as u64;
            p.bytes_done = done.min(fp.size);
            progress(&p);
        }
    }
    dev.sync_device()?;

    // 3. Commit the file system: backup boot sector, then the primary.
    let boot = boot_sector(f);
    put(dev, f.partition_offset() + BACKUP_BOOT * SECTOR, &boot, &mut w)?;
    put(dev, f.partition_offset(), &boot, &mut w)?;
    dev.sync_device()?;

    // 4. Commit the partition table, last.
    put(dev, 0, &mbr_sector(f), &mut w)?;
    dev.sync_device()?;
    dev.invalidate_cache()?;

    Ok(StickResult { plan: plan.clone(), bytes_written: w })
}

// ---------------------------------------------------------------------------
// Verification.

/// What reading the stick back from the medium found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StickVerification {
    /// The MBR is exactly as planned: one FAT32 partition from 1 MiB.
    pub partition_table_ok: bool,
    /// Boot sector, its backup and both FSInfo copies are exactly as planned.
    pub boot_sectors_ok: bool,
    /// Both FAT copies are byte-identical to the planned table: every chain
    /// one unbroken run, everything else free.
    pub fats_ok: bool,
    /// The top-level and folder tables are byte-identical to the plan.
    pub tables_ok: bool,
    /// Parsed back as a file system: the tracks are listed in playlist order,
    /// with the planned sizes and cluster runs.
    pub order_ok: bool,
    pub layout_ok: bool,
    pub data_checked: bool,
    pub data_mismatches: Vec<String>,
    pub problems: Vec<String>,
}

impl StickVerification {
    pub fn ok(&self) -> bool {
        self.partition_table_ok
            && self.boot_sectors_ok
            && self.fats_ok
            && self.tables_ok
            && self.order_ok
            && self.layout_ok
            && self.data_mismatches.is_empty()
            && self.problems.is_empty()
    }
}

fn read_at<D: Device>(dev: &mut D, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    dev.seek(SeekFrom::Start(offset))?;
    dev.read_exact(buf)
}

/// Read the stick back from the medium and compare it with the plan and, when
/// `check_data` is set, every file with its source.
pub fn verify_stick<D: Device>(
    dev: &mut D,
    plan: &StickPlan,
    items: &[WriteItem],
    check_data: bool,
    progress: &mut dyn FnMut(&WriteProgress),
) -> Result<StickVerification> {
    dev.invalidate_cache()?;
    let f = &plan.format;
    let mut v = StickVerification { data_checked: check_data, ..Default::default() };
    let mut sector = [0u8; 512];

    read_at(dev, 0, &mut sector)?;
    v.partition_table_ok = sector == mbr_sector(f);
    if !v.partition_table_ok {
        v.problems.push("the partition table differs from what was written".into());
    }
    let boot = boot_sector(f);
    let fsinfo = fsinfo_for(plan);
    let mut boot_ok = true;
    for (sec, want) in [(0, &boot), (1, &fsinfo), (BACKUP_BOOT, &boot), (BACKUP_BOOT + 1, &fsinfo)] {
        read_at(dev, f.partition_offset() + sec * SECTOR, &mut sector)?;
        boot_ok &= &sector == want;
    }
    v.boot_sectors_ok = boot_ok;
    if !boot_ok {
        v.problems.push("a boot or FSInfo sector differs from what was written".into());
    }

    let ends = plan.chain_ends();
    let mut fats_ok = true;
    let mut buf = vec![0u8; MIB as usize];
    for k in 0..2 {
        let mut off = 0u64;
        while off < f.fat_bytes() {
            let n = (f.fat_bytes() - off).min(MIB) as usize;
            read_at(dev, f.fat_offset(k) + off, &mut buf[..n])?;
            if buf[..n] != plan.fat_chunk(&ends, off, n)[..] {
                fats_ok = false;
                v.problems.push(format!("FAT copy {} differs from the plan near byte {off}", k + 1));
                break;
            }
            off += n as u64;
        }
    }
    v.fats_ok = fats_ok;

    let mut t = vec![0u8; plan.root_table.len()];
    read_at(dev, f.cluster_offset(2), &mut t)?;
    let mut tables_ok = t == plan.root_table;
    if plan.folder_clusters > 0 {
        let mut t = vec![0u8; plan.folder_table.len()];
        read_at(dev, f.cluster_offset(plan.folder_first), &mut t)?;
        tables_ok &= t == plan.folder_table;
    }
    v.tables_ok = tables_ok;
    if !tables_ok {
        v.problems.push("a folder table differs from what was written".into());
    }

    // Read it back the way a player would: through the partition table and
    // boot sector, then the folder's entries in FAT order.
    let mut vol = Volume::open(&mut *dev)?;
    let g = vol.geometry().clone();
    if g.partition_offset != f.partition_offset()
        || g.cluster_count != f.cluster_count
        || g.sectors_per_cluster != f.sectors_per_cluster
        || g.root_cluster != 2
    {
        v.problems.push("the file system read back has a different geometry".into());
    }
    let root = vol.list_dir(DirRef::Root)?;
    let top: Vec<&str> = root.groups.iter().filter(|g| g.kind != EntryKind::VolumeLabel).map(|g| g.name()).collect();
    let listing = if plan.folder.is_empty() {
        root.clone()
    } else {
        if top != [plan.folder.as_str()] {
            v.problems.push(format!("the top level holds {top:?}, expected only {:?}", plan.folder));
        }
        let d = vol.find_dir(&plan.folder)?;
        vol.list_dir(d)?
    };
    let tracks: Vec<_> = listing.groups.iter().filter(|g| g.kind == EntryKind::File).collect();
    let names: Vec<&str> = tracks.iter().map(|g| g.name()).collect();
    let want: Vec<&str> = plan.files.iter().map(|f| f.name.as_str()).collect();
    v.order_ok = names == want && listing.orphan_lfn_slots == 0 && listing.deleted_slots == 0;
    if !v.order_ok {
        let at = names.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(names.len().min(want.len()));
        v.problems.push(format!("the tracks are not listed in playlist order (first difference at position {})", at + 1));
    }
    let mut layout_ok = tracks.len() == plan.files.len();
    for (g, fp) in tracks.iter().zip(&plan.files) {
        let chain = if g.first_cluster >= 2 { vol.cluster_chain(g.first_cluster)? } else { vec![] };
        let planned: Vec<u32> = (fp.first_cluster..fp.first_cluster + fp.clusters).collect();
        if g.size as u64 != fp.size || chain != planned {
            layout_ok = false;
            v.problems.push(format!("{}: not where it was planned", fp.name));
        }
    }
    v.layout_ok = layout_ok;
    drop(vol);

    if check_data {
        let cb = f.cluster_bytes();
        let mut src_buf = vec![0u8; MIB as usize];
        for (i, (fp, it)) in plan.files.iter().zip(items).enumerate() {
            let mut p = WriteProgress {
                stage: "verify".into(),
                index: i,
                total: plan.files.len(),
                name: fp.name.clone(),
                bytes_done: 0,
                bytes_total: fp.size,
            };
            progress(&p);
            let mut src = std::fs::File::open(&it.source)?;
            let span = fp.clusters as u64 * cb;
            let mut done = 0u64;
            let mut same = true;
            while done < span {
                let n = (span - done).min(MIB) as usize;
                let data_n = fp.size.saturating_sub(done).min(n as u64) as usize;
                read_at(dev, f.cluster_offset(fp.first_cluster) + done, &mut buf[..n])?;
                read_full(&mut src, &mut src_buf[..data_n])?;
                if buf[..data_n] != src_buf[..data_n] || buf[data_n..n].iter().any(|&b| b != 0) {
                    same = false;
                    break;
                }
                done += n as u64;
                p.bytes_done = done.min(fp.size);
                progress(&p);
            }
            if !same {
                v.data_mismatches.push(fp.name.clone());
            }
        }
    }
    Ok(v)
}

/// Parse the planned top-level table (used by tests and the dry run).
pub fn planned_listing(plan: &StickPlan) -> Vec<String> {
    let t = if plan.folder.is_empty() { &plan.root_table } else { &plan.folder_table };
    parse_directory(t, DirRef::Root)
        .groups
        .iter()
        .filter(|g| g.kind == EntryKind::File)
        .map(|g| g.name().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const GIB: u64 = 1 << 30;

    fn fmt(bytes: u64) -> Format {
        choose_format(bytes, 512, *b"TEST       ", 1, 1).unwrap()
    }

    #[test]
    fn geometry_follows_the_spec_table_and_aligns() {
        for (bytes, spc) in [
            (64 * MIB, 1),
            (256 * MIB, 1),
            (GIB, 8),
            (8 * GIB, 8),
            (16 * GIB, 16),
            (32 * GIB, 32),
            (64 * GIB, 64),
            (256 * GIB, 64),
        ] {
            let f = fmt(bytes);
            assert_eq!(f.sectors_per_cluster, spc, "{bytes}");
            assert!(f.cluster_count >= 65_525);
            assert!(f.cluster_count as u64 + 2 <= f.fat_sectors as u64 * 128, "FAT too small for {bytes}");
            let align = if bytes >= 2 * GIB { 4 * MIB } else { MIB };
            assert_eq!(f.data_offset() % align, 0, "data not aligned for {bytes}");
            let end = f.data_offset() + f.cluster_count as u64 * f.cluster_bytes();
            assert!(end <= bytes);
        }
    }

    #[test]
    fn refuses_impossible_sticks() {
        assert!(choose_format(16 * MIB, 512, *b"X          ", 1, 1).is_err());
        assert!(choose_format(4 * GIB, 4096, *b"X          ", 1, 1).is_err());
        assert!(choose_format(3 << 40, 512, *b"X          ", 1, 1).is_err());
        // Just above the minimum still works.
        let f = choose_format(36 * 1_000_000, 512, *b"X          ", 1, 1).unwrap();
        assert!(f.cluster_count >= 65_525);
    }

    fn sources(dir: &std::path::Path, sizes: &[usize]) -> Vec<WriteItem> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| {
                let p = dir.join(format!("src{i}.bin"));
                std::fs::write(&p, (0..n).map(|k| (k * 7 + i) as u8).collect::<Vec<u8>>()).unwrap();
                WriteItem { source: p, name: format!("{:02} - Track number {i} with a long name.mp3", i + 1) }
            })
            .collect()
    }

    #[test]
    fn writes_and_verifies_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let items = sources(dir.path(), &[5000, 0, 70_000, 512, 1]);
        for folder in ["Road Trip", ""] {
            let size = 64 * MIB;
            let mut dev = Cursor::new(vec![0xA5u8; size as usize]);
            let opts = StickOptions { folder: folder.into(), label: String::new() };
            let plan = plan_stick(size, 512, &items, &opts).unwrap();
            assert_eq!(planned_listing(&plan), items.iter().map(|i| i.name.clone()).collect::<Vec<_>>());
            write_stick(&mut dev, &plan, &items, &mut |_| {}).unwrap();
            let v = verify_stick(&mut dev, &plan, &items, true, &mut |_| {}).unwrap();
            assert!(v.ok(), "{v:?}");
            // Files back to back from right after the tables.
            let mut next = 2 + plan.root_clusters + plan.folder_clusters;
            for fp in plan.files.iter().filter(|f| f.clusters > 0) {
                assert_eq!(fp.first_cluster, next);
                next += fp.clusters;
            }
        }
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(80 * MIB).unwrap();
        let items = vec![WriteItem { source: p, name: "big.mp3".into() }];
        let e = plan_stick(64 * MIB, 512, &items, &StickOptions::default()).unwrap_err();
        assert!(e.to_string().contains("needs"), "{e}");
    }

    #[test]
    fn mbr_and_boot_sector_fields() {
        let f = fmt(8 * GIB);
        let m = mbr_sector(&f);
        assert_eq!(m[446 + 4], 0x0C);
        assert_eq!(u32::from_le_bytes(m[454..458].try_into().unwrap()), 2048);
        assert_eq!(u32::from_le_bytes(m[458..462].try_into().unwrap()) as u64, 8 * GIB / 512 - 2048);
        let b = boot_sector(&f);
        assert_eq!(&b[82..90], b"FAT32   ");
        assert_eq!(super::super::le16(&b, 22), 0, "FAT32 has no 16-bit FAT size");
        assert_eq!(super::super::le32(&b, 28), 2048, "hidden sectors = partition start");
    }
}
