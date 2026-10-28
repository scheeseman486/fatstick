//! Writing playlists to a USB stick by erasing it and laying down a new FAT32
//! file system whose every byte of metadata is chosen here.
//!
//! The stick gets one MBR partition (type 0x0C, FAT32 with LBA) starting at
//! 1 MiB, holding:
//!
//! * at the top level, the volume label, then one folder per playlist in
//!   stick order (or, for a one-playlist stick, the tracks themselves);
//! * in each folder, `.` and `..`, then one entry per track in playlist order
//!   (FAT order);
//! * the top-level table, then every folder's table, then every file's data
//!   in one unbroken run of clusters, back to back in stick order, from
//!   cluster 2;
//! * optionally, hidden files at the end of the top level, their data after
//!   everything else: Fatstick's note of what the stick holds, so it can
//!   recognize the stick later. Players skip them: they aren't audio.
//!
//! A file's bytes are assembled from parts, each a range of a file on disk:
//! typically an ID3 tag, the prepared audio, and an optional ID3v1 tag. The
//! parts are streamed onto the stick, so nothing has to be assembled first.
//!
//! Nothing that was on the stick before is kept or adapted to, so there are
//! no existing layouts to get wrong.
//!
//! Write order, so a pull (or a cancel) at any point leaves either a blank
//! stick or the complete one:
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
//! compared with what was planned, and every file with its parts.

use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::dir::{parse_directory, ENTRY};
use super::entries::{
    entry_group, normalize_name, sfn_entry, validate_name, volume_label, written_name, FatTimestamp, ATTR_ARCHIVE,
    ATTR_DIR, ATTR_HIDDEN, ATTR_VOLUME_ID,
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

/// A run of bytes from a file on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub path: PathBuf,
    #[serde(default)]
    pub offset: u64,
    /// Bytes to take; `None` takes the rest of the file.
    #[serde(default)]
    pub len: Option<u64>,
}

impl Part {
    pub fn whole(path: impl Into<PathBuf>) -> Self {
        Part { path: path.into(), offset: 0, len: None }
    }
    pub fn range(path: impl Into<PathBuf>, offset: u64, len: u64) -> Self {
        Part { path: path.into(), offset, len: Some(len) }
    }
}

/// One file on the stick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteItem {
    /// Name to give the file on the stick.
    pub name: String,
    /// Its bytes, in order.
    pub parts: Vec<Part>,
}

impl WriteItem {
    /// A file on the stick that is a copy of one whole file.
    pub fn file(source: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        WriteItem { name: name.into(), parts: vec![Part::whole(source)] }
    }
}

/// One playlist's folder. An empty name puts the tracks at the top level, and
/// then it must be the only folder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StickFolder {
    #[serde(default)]
    pub name: String,
    pub items: Vec<WriteItem>,
}

/// Everything that goes on a stick, in stick order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StickContent {
    pub folders: Vec<StickFolder>,
    /// Volume label; empty derives it from the first folder's name (or "PLAYLIST").
    #[serde(default)]
    pub label: String,
    /// Hidden files at the end of the top level, written after everything
    /// else (entries and data): the tracks keep their order and stay one
    /// unbroken run.
    #[serde(default)]
    pub hidden: Vec<WriteItem>,
}

/// Options for the one-playlist case (the CLI's `write`).
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

impl StickContent {
    /// One playlist, in a folder or at the top level.
    pub fn single(items: Vec<WriteItem>, opts: &StickOptions) -> Self {
        StickContent { folders: vec![StickFolder { name: opts.folder.clone(), items }], label: opts.label.clone(), hidden: Vec::new() }
    }
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

/// A part with its size settled when the plan was made.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartPlace {
    pub path: PathBuf,
    pub offset: u64,
    pub len: u64,
    /// The whole source file's size when planned, to notice it changing.
    pub file_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePlace {
    pub name: String,
    pub short_name: String,
    pub size: u64,
    /// 0 for an empty file.
    pub first_cluster: u32,
    pub clusters: u32,
    pub parts: Vec<PartPlace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderPlace {
    /// Empty: the files are at the top level.
    pub name: String,
    /// The folder's own table (0 and 0 at the top level).
    pub first_cluster: u32,
    pub clusters: u32,
    pub files: Vec<FilePlace>,
    #[serde(skip)]
    table: Vec<u8>,
}

/// Everything that will be written, decided before the first write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickPlan {
    pub format: Format,
    pub root_clusters: u32,
    pub folders: Vec<FolderPlace>,
    /// The hidden files at the end of the top level.
    #[serde(default)]
    pub hidden: Vec<FilePlace>,
    /// Clusters used in total, all of them from cluster 2 up, without gaps.
    pub used_clusters: u32,
    pub total_bytes: u64,
    pub created_unix: i64,
    #[serde(skip)]
    root_table: Vec<u8>,
}

impl StickPlan {
    /// The tracks are at the top level rather than in folders.
    pub fn top_level(&self) -> bool {
        self.folders.len() == 1 && self.folders[0].name.is_empty()
    }
    /// Every file, in stick order.
    pub fn files(&self) -> impl Iterator<Item = &FilePlace> {
        self.folders.iter().flat_map(|f| f.files.iter())
    }
    pub fn file_count(&self) -> usize {
        self.folders.iter().map(|f| f.files.len()).sum()
    }
    /// The folders' paths on the stick, e.g. "/01 Road Trip" (or "/").
    pub fn folder_paths(&self) -> Vec<String> {
        self.folders.iter().map(|f| format!("/{}", f.name)).collect()
    }
    /// First cluster after the directory tables, where the file data starts.
    pub fn first_data_cluster(&self) -> u32 {
        2 + self.root_clusters + self.folders.iter().map(|f| f.clusters).sum::<u32>()
    }
    pub fn free_clusters(&self) -> u32 {
        self.format.cluster_count - self.used_clusters
    }
    /// Cluster runs in allocation order: root, folder tables, files, hidden files.
    fn runs(&self) -> Vec<(u32, u32)> {
        let mut r = vec![(2, self.root_clusters)];
        r.extend(self.folders.iter().filter(|f| f.clusters > 0).map(|f| (f.first_cluster, f.clusters)));
        r.extend(self.files().chain(&self.hidden).filter(|f| f.clusters > 0).map(|f| (f.first_cluster, f.clusters)));
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

/// Settle a file's parts: every source must exist and hold the range asked for.
fn place_parts(item: &WriteItem) -> Result<(Vec<PartPlace>, u64)> {
    let mut out = Vec::with_capacity(item.parts.len());
    let mut size = 0u64;
    for p in &item.parts {
        let m = std::fs::metadata(&p.path).map_err(|e| FatError::NotFound(format!("{}: {e}", p.path.display())))?;
        if !m.is_file() {
            return Err(FatError::NotFound(p.path.display().to_string()));
        }
        let file_size = m.len();
        let len = match p.len {
            Some(l) => l,
            None => file_size.saturating_sub(p.offset),
        };
        if p.offset.checked_add(len).map_or(true, |end| end > file_size) {
            return Err(FatError::Invalid(format!("{}: the range asked for is past the end of the file", p.path.display())));
        }
        size += len;
        out.push(PartPlace { path: p.path.clone(), offset: p.offset, len, file_size });
    }
    if size > u32::MAX as u64 {
        return Err(FatError::Invalid(format!("{} is larger than FAT32's 4 GiB file limit", item.name)));
    }
    Ok((out, size))
}

/// Decide everything for the one-playlist case.
pub fn plan_stick(disk_bytes: u64, sector_size: u32, items: &[WriteItem], opts: &StickOptions) -> Result<StickPlan> {
    plan_content(disk_bytes, sector_size, &StickContent::single(items.to_vec(), opts))
}

/// Decide everything: geometry, names, the directory tables and where every
/// file goes. Reads only the sizes of the source files.
pub fn plan_content(disk_bytes: u64, sector_size: u32, content: &StickContent) -> Result<StickPlan> {
    let (now, nanos) = timestamp_now();
    plan_content_at(disk_bytes, sector_size, content, now, nanos)
}

pub(crate) fn plan_content_at(disk_bytes: u64, sector_size: u32, content: &StickContent, now: i64, nanos: u32) -> Result<StickPlan> {
    // Folder names: an empty one means the top level, and then it's alone.
    let folder_names: Vec<String> = content.folders.iter().map(|f| written_name(f.name.trim())).collect();
    let top = folder_names.len() == 1 && folder_names[0].is_empty();
    if !top && folder_names.iter().any(|n| n.is_empty()) {
        return Err(FatError::Invalid("tracks at the top level can't share the stick with folders".into()));
    }
    let mut seen_folders = HashSet::new();
    for n in folder_names.iter().filter(|n| !n.is_empty()) {
        validate_name(n)?;
        if !seen_folders.insert(normalize_name(n)) {
            return Err(FatError::Invalid(format!("two folders are called {n:?}")));
        }
    }
    let label_src = if !content.label.trim().is_empty() {
        content.label.clone()
    } else {
        folder_names.first().cloned().unwrap_or_default()
    };
    let label = volume_label(&label_src);
    let (serial, sig) = ids(now, nanos);
    let format = choose_format(disk_bytes, sector_size, label, serial, sig)?;
    let cb = format.cluster_bytes();

    // Names, parts and sizes.
    let mut folders = Vec::with_capacity(content.folders.len());
    for (folder, fname) in content.folders.iter().zip(&folder_names) {
        let mut seen = HashSet::new();
        let mut files = Vec::with_capacity(folder.items.len());
        for it in &folder.items {
            let name = written_name(&it.name);
            validate_name(&name)?;
            if !seen.insert(normalize_name(&name)) {
                return Err(FatError::Invalid(format!("{name:?} appears twice in {}", if fname.is_empty() { "the stick" } else { fname })));
            }
            let (parts, size) = place_parts(it)?;
            files.push(FilePlace { name, short_name: String::new(), size, first_cluster: 0, clusters: size.div_ceil(cb) as u32, parts });
        }
        folders.push(FolderPlace { name: fname.clone(), first_cluster: 0, clusters: 0, files, table: Vec::new() });
    }
    let mut hidden = Vec::with_capacity(content.hidden.len());
    {
        let mut seen: HashSet<String> = if top {
            folders[0].files.iter().map(|f| normalize_name(&f.name)).collect()
        } else {
            folders.iter().map(|f| normalize_name(&f.name)).collect()
        };
        for it in &content.hidden {
            let name = written_name(&it.name);
            validate_name(&name)?;
            if !seen.insert(normalize_name(&name)) {
                return Err(FatError::Invalid(format!("{name:?} appears twice at the top level")));
            }
            let (parts, size) = place_parts(it)?;
            hidden.push(FilePlace { name, short_name: String::new(), size, first_cluster: 0, clusters: size.div_ceil(cb) as u32, parts });
        }
    }

    // Table sizes: build the entries once with placeholder clusters to count slots.
    let ts0 = FatTimestamp::from_unix(now);
    let count_slots = |names: &[&str], attr: u8| -> usize {
        let mut taken = HashSet::new();
        names.iter().map(|n| entry_group(n, &mut taken, attr, 0, 0, ts0).0.len()).sum()
    };
    let clusters_for = |slots: usize| ((slots * ENTRY) as u64).div_ceil(cb).max(1) as u32;
    let hidden_names: Vec<&str> = hidden.iter().map(|f| f.name.as_str()).collect();
    let root_slots = count_slots(&hidden_names, ATTR_ARCHIVE | ATTR_HIDDEN)
        + if top {
            let names: Vec<&str> = folders[0].files.iter().map(|f| f.name.as_str()).collect();
            1 + count_slots(&names, ATTR_ARCHIVE)
        } else {
            let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
            1 + count_slots(&names, ATTR_DIR)
        };
    if root_slots > MAX_DIR_SLOTS {
        return Err(FatError::Invalid("too many entries for the top level of the stick".into()));
    }
    let root_clusters = clusters_for(root_slots);
    let mut next = 2 + root_clusters;
    if !top {
        for f in &mut folders {
            let names: Vec<&str> = f.files.iter().map(|x| x.name.as_str()).collect();
            let slots = 2 + count_slots(&names, ATTR_ARCHIVE);
            if slots > MAX_DIR_SLOTS {
                return Err(FatError::Invalid(format!("too many tracks for one FAT folder ({})", f.name)));
            }
            f.clusters = clusters_for(slots);
            f.first_cluster = next;
            next += f.clusters;
        }
    }

    // Files back to back after the tables, in stick order; the hidden ones last.
    for f in folders.iter_mut().flat_map(|f| f.files.iter_mut()).chain(hidden.iter_mut()) {
        if f.clusters > 0 {
            f.first_cluster = next;
            next += f.clusters;
        }
    }
    let used = next - 2;
    if used > format.cluster_count {
        let mb = |c: u64| c * cb / 1_000_000;
        return Err(FatError::Invalid(format!(
            "this needs {} MB but the stick holds {} MB",
            mb(used as u64),
            mb(format.cluster_count as u64)
        )));
    }

    // The tables themselves. Modification times step 2 s per file across the
    // whole stick (and per folder), so players that sort by date agree.
    let table = |entries: &[[u8; ENTRY]], clusters: u32| -> Vec<u8> {
        let mut t = vec![0u8; (clusters as u64 * cb) as usize];
        for (i, e) in entries.iter().enumerate() {
            t[i * ENTRY..(i + 1) * ENTRY].copy_from_slice(e);
        }
        t
    };
    let mut k = 0i64;
    let mut file_groups = Vec::with_capacity(folders.len());
    let mut top_taken = HashSet::new();
    for f in &mut folders {
        let mut taken = HashSet::new();
        let mut entries = Vec::new();
        for file in &mut f.files {
            let (g, sfn) = entry_group(&file.name, &mut taken, ATTR_ARCHIVE, file.first_cluster, file.size as u32, FatTimestamp::from_unix(now + 2 * k));
            k += 1;
            file.short_name = String::from_utf8_lossy(&sfn).into_owned();
            entries.extend(g);
        }
        file_groups.push(entries);
        top_taken = taken;
    }
    let mut root: Vec<[u8; ENTRY]> = vec![sfn_entry(&format.label_bytes(), ATTR_VOLUME_ID, 0, 0, ts0)];
    if top {
        root.extend(file_groups.pop().unwrap_or_default());
    } else {
        top_taken = HashSet::new();
        let taken = &mut top_taken;
        for (i, (f, files)) in folders.iter_mut().zip(file_groups).enumerate() {
            let ts = FatTimestamp::from_unix(now + 2 * i as i64);
            root.extend(entry_group(&f.name, taken, ATTR_DIR, f.first_cluster, 0, ts).0);
            let mut entries = vec![sfn_entry(b".          ", ATTR_DIR, f.first_cluster, 0, ts), sfn_entry(b"..         ", ATTR_DIR, 0, 0, ts)];
            entries.extend(files);
            f.table = table(&entries, f.clusters);
        }
    }
    for file in &mut hidden {
        let (g, sfn) = entry_group(&file.name, &mut top_taken, ATTR_ARCHIVE | ATTR_HIDDEN, file.first_cluster, file.size as u32, ts0);
        file.short_name = String::from_utf8_lossy(&sfn).into_owned();
        root.extend(g);
    }
    let total_bytes = folders.iter().flat_map(|f| f.files.iter()).map(|f| f.size).sum();
    Ok(StickPlan {
        root_table: table(&root, root_clusters),
        format,
        root_clusters,
        folders,
        hidden,
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
    /// Position of the file across the whole stick.
    pub index: usize,
    pub total: usize,
    pub name: String,
    /// The file's folder ("" at the top level).
    #[serde(default)]
    pub folder: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Bytes of file data done and to do across the whole stick.
    #[serde(default)]
    pub all_done: u64,
    #[serde(default)]
    pub all_total: u64,
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

/// A file's parts read one after another, as one stream.
struct PartReader<'a> {
    parts: &'a [PartPlace],
    next: usize,
    cur: Option<(std::fs::File, u64)>,
}

impl<'a> PartReader<'a> {
    fn new(parts: &'a [PartPlace]) -> Self {
        PartReader { parts, next: 0, cur: None }
    }

    /// Fill `buf` completely.
    fn fill(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut off = 0;
        while off < buf.len() {
            if self.cur.as_ref().map_or(true, |(_, left)| *left == 0) {
                let Some(p) = self.parts.get(self.next) else {
                    return Err(FatError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "a source is shorter than planned")));
                };
                self.next += 1;
                let mut f = std::fs::File::open(&p.path)?;
                if f.metadata()?.len() != p.file_size {
                    return Err(FatError::Invalid(format!("{} changed since the plan was made", p.path.display())));
                }
                f.seek(SeekFrom::Start(p.offset))?;
                self.cur = Some((f, p.len));
                continue;
            }
            let (f, left) = self.cur.as_mut().unwrap();
            let want = (buf.len() - off).min(*left as usize);
            match f.read(&mut buf[off..off + want]) {
                Ok(0) => return Err(FatError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "a source is shorter than planned"))),
                Ok(n) => {
                    off += n;
                    *left -= n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(FatError::Cancelled);
    }
    Ok(())
}

/// Erase the stick and write the planned file system and files.
/// `dev` is the whole disk. Everything on it is lost.
///
/// Setting `cancel` stops the write at the next MiB. The partition table is
/// written last, so a cancelled stick reads as blank.
pub fn write_stick<D: Device>(
    dev: &mut D,
    plan: &StickPlan,
    progress: &mut dyn FnMut(&WriteProgress),
    cancel: Option<&AtomicBool>,
) -> Result<StickResult> {
    let f = &plan.format;
    if plan.root_table.is_empty() {
        return Err(FatError::Invalid("this plan was read from a file; make it again before writing".into()));
    }
    let disk = dev.disk_size()?;
    if disk != f.disk_bytes {
        return Err(FatError::Invalid(format!("the stick's size changed ({disk} bytes, planned for {})", f.disk_bytes)));
    }
    let cb = f.cluster_bytes();
    let mut w = 0u64;
    let total = plan.file_count();
    let all_total = plan.total_bytes;
    cancelled(cancel)?;
    progress(&WriteProgress {
        stage: "erase".into(),
        index: 0,
        total,
        name: String::new(),
        folder: String::new(),
        bytes_done: 0,
        bytes_total: 0,
        all_done: 0,
        all_total,
    });

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
            cancelled(cancel)?;
            let n = (f.fat_bytes() - off).min(MIB);
            let chunk = plan.fat_chunk(&ends, off, n as usize);
            put(dev, f.fat_offset(k) + off, &chunk, &mut w)?;
            off += n;
        }
    }

    // 2c. Directory tables.
    put(dev, f.cluster_offset(2), &plan.root_table, &mut w)?;
    for folder in plan.folders.iter().filter(|x| x.clusters > 0) {
        put(dev, f.cluster_offset(folder.first_cluster), &folder.table, &mut w)?;
    }

    // 2d. File data, each file padded with zeros to its last cluster.
    let mut buf = vec![0u8; MIB as usize];
    let mut i = 0;
    let mut all_done = 0u64;
    for folder in &plan.folders {
        for fp in &folder.files {
            let mut p = WriteProgress {
                stage: "write".into(),
                index: i,
                total,
                name: fp.name.clone(),
                folder: folder.name.clone(),
                bytes_done: 0,
                bytes_total: fp.size,
                all_done,
                all_total,
            };
            progress(&p);
            let mut src = PartReader::new(&fp.parts);
            let span = fp.clusters as u64 * cb;
            let mut done = 0u64;
            while done < span {
                cancelled(cancel)?;
                let n = (span - done).min(MIB) as usize;
                let data_n = fp.size.saturating_sub(done).min(n as u64) as usize;
                src.fill(&mut buf[..data_n])?;
                buf[data_n..n].fill(0);
                put(dev, f.cluster_offset(fp.first_cluster) + done, &buf[..n], &mut w)?;
                done += n as u64;
                p.bytes_done = done.min(fp.size);
                p.all_done = all_done + p.bytes_done;
                progress(&p);
            }
            all_done += fp.size;
            i += 1;
        }
    }
    for fp in &plan.hidden {
        let mut src = PartReader::new(&fp.parts);
        let span = fp.clusters as u64 * cb;
        let mut done = 0u64;
        while done < span {
            cancelled(cancel)?;
            let n = (span - done).min(MIB) as usize;
            let data_n = fp.size.saturating_sub(done).min(n as u64) as usize;
            src.fill(&mut buf[..data_n])?;
            buf[data_n..n].fill(0);
            put(dev, f.cluster_offset(fp.first_cluster) + done, &buf[..n], &mut w)?;
            done += n as u64;
        }
    }
    dev.sync_device()?;
    cancelled(cancel)?;

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
    /// Parsed back as a file system: the folders and tracks are listed in
    /// stick order, with the planned sizes and cluster runs.
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
/// `check_data` is set, every file with its parts.
pub fn verify_stick<D: Device>(
    dev: &mut D,
    plan: &StickPlan,
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
    for folder in plan.folders.iter().filter(|x| x.clusters > 0) {
        let mut t = vec![0u8; folder.table.len()];
        read_at(dev, f.cluster_offset(folder.first_cluster), &mut t)?;
        tables_ok &= t == folder.table;
    }
    v.tables_ok = tables_ok;
    if !tables_ok {
        v.problems.push("a folder table differs from what was written".into());
    }

    // Read it back the way a player would: through the partition table and
    // boot sector, then each folder's entries in FAT order.
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
    let mut order_ok = true;
    let mut layout_ok = true;
    let mut listings = Vec::new();
    if plan.top_level() {
        listings.push(root.clone());
    } else {
        let top: Vec<&str> = root.groups.iter().filter(|g| g.kind != EntryKind::VolumeLabel && !g.is_hidden()).map(|g| g.name()).collect();
        let want: Vec<&str> = plan.folders.iter().map(|f| f.name.as_str()).collect();
        if top != want || root.orphan_lfn_slots != 0 || root.deleted_slots != 0 {
            order_ok = false;
            v.problems.push(format!("the top level holds {top:?}, expected {want:?}"));
        }
        for folder in &plan.folders {
            let d = vol.find_dir(&folder.name)?;
            listings.push(vol.list_dir(d)?);
        }
    }
    for (listing, folder) in listings.iter().zip(&plan.folders) {
        let tracks: Vec<_> = listing.groups.iter().filter(|g| g.kind == EntryKind::File && !g.is_hidden()).collect();
        let names: Vec<&str> = tracks.iter().map(|g| g.name()).collect();
        let want: Vec<&str> = folder.files.iter().map(|f| f.name.as_str()).collect();
        if names != want || listing.orphan_lfn_slots != 0 || listing.deleted_slots != 0 {
            order_ok = false;
            let at = names.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(names.len().min(want.len()));
            let place = if folder.name.is_empty() { "at the top level".to_string() } else { format!("in {}", folder.name) };
            v.problems.push(format!("the tracks {place} are not listed in playlist order (first difference at position {})", at + 1));
        }
        layout_ok &= tracks.len() == folder.files.len();
        for (g, fp) in tracks.iter().zip(&folder.files) {
            let chain = if g.first_cluster >= 2 { vol.cluster_chain(g.first_cluster)? } else { vec![] };
            let planned: Vec<u32> = (fp.first_cluster..fp.first_cluster + fp.clusters).collect();
            if g.size as u64 != fp.size || chain != planned {
                layout_ok = false;
                v.problems.push(format!("{}: not where it was planned", fp.name));
            }
        }
    }
    // The hidden files: last at the top level, where they were planned.
    let hid: Vec<_> = root.groups.iter().filter(|g| g.kind == EntryKind::File && g.is_hidden()).collect();
    let hid_names: Vec<&str> = hid.iter().map(|g| g.name()).collect();
    let want_hid: Vec<&str> = plan.hidden.iter().map(|f| f.name.as_str()).collect();
    let last_hid = root.groups.iter().rposition(|g| !g.is_hidden() && g.kind != EntryKind::VolumeLabel);
    let first_hid = root.groups.iter().position(|g| g.kind == EntryKind::File && g.is_hidden());
    if hid_names != want_hid || matches!((last_hid, first_hid), (Some(a), Some(b)) if b < a) {
        order_ok = false;
        v.problems.push(format!("the hidden files at the top level are {hid_names:?}, expected {want_hid:?} after everything else"));
    }
    for (g, fp) in hid.iter().zip(&plan.hidden) {
        let chain = if g.first_cluster >= 2 { vol.cluster_chain(g.first_cluster)? } else { vec![] };
        let planned: Vec<u32> = (fp.first_cluster..fp.first_cluster + fp.clusters).collect();
        if g.size as u64 != fp.size || chain != planned {
            layout_ok = false;
            v.problems.push(format!("{}: not where it was planned", fp.name));
        }
    }
    v.order_ok = order_ok;
    v.layout_ok = layout_ok;
    drop(vol);

    if check_data {
        let cb = f.cluster_bytes();
        let mut src_buf = vec![0u8; MIB as usize];
        let total = plan.file_count();
        let mut i = 0;
        let mut all_done = 0u64;
        for folder in &plan.folders {
            for fp in &folder.files {
                let mut p = WriteProgress {
                    stage: "verify".into(),
                    index: i,
                    total,
                    name: fp.name.clone(),
                    folder: folder.name.clone(),
                    bytes_done: 0,
                    bytes_total: fp.size,
                    all_done,
                    all_total: plan.total_bytes,
                };
                progress(&p);
                let mut src = PartReader::new(&fp.parts);
                let span = fp.clusters as u64 * cb;
                let mut done = 0u64;
                let mut same = true;
                while done < span {
                    let n = (span - done).min(MIB) as usize;
                    let data_n = fp.size.saturating_sub(done).min(n as u64) as usize;
                    read_at(dev, f.cluster_offset(fp.first_cluster) + done, &mut buf[..n])?;
                    if src.fill(&mut src_buf[..data_n]).is_err() {
                        same = false;
                        break;
                    }
                    if buf[..data_n] != src_buf[..data_n] || buf[data_n..n].iter().any(|&b| b != 0) {
                        same = false;
                        break;
                    }
                    done += n as u64;
                    p.bytes_done = done.min(fp.size);
                    p.all_done = all_done + p.bytes_done;
                    progress(&p);
                }
                if !same {
                    v.data_mismatches.push(if folder.name.is_empty() { fp.name.clone() } else { format!("{}/{}", folder.name, fp.name) });
                }
                all_done += fp.size;
                i += 1;
            }
        }
        for fp in &plan.hidden {
            let mut src = PartReader::new(&fp.parts);
            let span = fp.clusters as u64 * cb;
            let mut done = 0u64;
            let mut same = true;
            while done < span {
                let n = (span - done).min(MIB) as usize;
                let data_n = fp.size.saturating_sub(done).min(n as u64) as usize;
                read_at(dev, f.cluster_offset(fp.first_cluster) + done, &mut buf[..n])?;
                if src.fill(&mut src_buf[..data_n]).is_err() || buf[..data_n] != src_buf[..data_n] || buf[data_n..n].iter().any(|&b| b != 0) {
                    same = false;
                    break;
                }
                done += n as u64;
            }
            if !same {
                v.data_mismatches.push(fp.name.clone());
            }
        }
    }
    Ok(v)
}

/// A file at the top level of a FAT32 stick or image, by name (case
/// insensitive), if it's there and no larger than `max` bytes.
pub fn read_top_level_file<D: Device>(dev: D, name: &str, max: u64) -> Result<Option<Vec<u8>>> {
    let mut vol = Volume::open(dev)?;
    let root = vol.list_dir(DirRef::Root)?;
    let want = normalize_name(name);
    let Some(g) = root.groups.iter().find(|g| g.kind == EntryKind::File && normalize_name(g.name()) == want) else {
        return Ok(None);
    };
    if g.size as u64 > max {
        return Ok(None);
    }
    if g.size == 0 {
        return Ok(Some(Vec::new()));
    }
    let geo = vol.geometry().clone();
    let cb = geo.cluster_bytes();
    let extents: Vec<super::Extent> =
        vol.cluster_chain(g.first_cluster)?.into_iter().map(|c| super::Extent { offset: geo.cluster_offset(c), len: cb }).collect();
    let mut bytes = vol.read_extents(&extents)?;
    bytes.truncate(g.size as usize);
    Ok(Some(bytes))
}

/// The file names of each planned folder table, parsed back (tests and the dry run).
pub fn planned_listing(plan: &StickPlan) -> Vec<Vec<String>> {
    let parse = |t: &[u8]| -> Vec<String> {
        parse_directory(t, DirRef::Root).groups.iter().filter(|g| g.kind == EntryKind::File && !g.is_hidden()).map(|g| g.name().to_string()).collect()
    };
    if plan.top_level() {
        vec![parse(&plan.root_table)]
    } else {
        plan.folders.iter().map(|f| parse(&f.table)).collect()
    }
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

    fn sources(dir: &std::path::Path, tag: &str, sizes: &[usize]) -> Vec<WriteItem> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| {
                let p = dir.join(format!("{tag}{i}.bin"));
                std::fs::write(&p, (0..n).map(|k| (k * 7 + i) as u8).collect::<Vec<u8>>()).unwrap();
                WriteItem::file(p, format!("{:02} - Track number {i} with a long name.mp3", i + 1))
            })
            .collect()
    }

    fn check_back_to_back(plan: &StickPlan) {
        let mut next = plan.first_data_cluster();
        for fp in plan.files().chain(&plan.hidden).filter(|f| f.clusters > 0) {
            assert_eq!(fp.first_cluster, next);
            next += fp.clusters;
        }
        assert_eq!(next - 2, plan.used_clusters);
    }

    #[test]
    fn writes_and_verifies_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let items = sources(dir.path(), "a", &[5000, 0, 70_000, 512, 1]);
        for folder in ["Road Trip", ""] {
            let size = 64 * MIB;
            let mut dev = Cursor::new(vec![0xA5u8; size as usize]);
            let opts = StickOptions { folder: folder.into(), label: String::new() };
            let plan = plan_stick(size, 512, &items, &opts).unwrap();
            assert_eq!(planned_listing(&plan), vec![items.iter().map(|i| i.name.clone()).collect::<Vec<_>>()]);
            write_stick(&mut dev, &plan, &mut |_| {}, None).unwrap();
            let v = verify_stick(&mut dev, &plan, true, &mut |_| {}).unwrap();
            assert!(v.ok(), "{v:?}");
            check_back_to_back(&plan);
        }
    }

    #[test]
    fn several_folders_in_stick_order() {
        let dir = tempfile::tempdir().unwrap();
        let content = StickContent {
            folders: vec![
                StickFolder { name: "01 Road Trip".into(), items: sources(dir.path(), "a", &[40_000, 3, 9000]) },
                StickFolder { name: "02 Kids' car".into(), items: sources(dir.path(), "b", &[700, 120_000]) },
                StickFolder { name: "03 Sleep".into(), items: sources(dir.path(), "c", &[1]) },
            ],
            label: "Car".into(),
            hidden: Vec::new(),
        };
        let size = 64 * MIB;
        let mut dev = Cursor::new(vec![0x5Au8; size as usize]);
        let plan = plan_content(size, 512, &content).unwrap();
        assert_eq!(plan.folders.len(), 3);
        assert_eq!(plan.file_count(), 6);
        assert_eq!(plan.format.label.trim_end(), "CAR");
        // Tables first (root, then each folder's), then the files.
        assert_eq!(plan.folders[0].first_cluster, 2 + plan.root_clusters);
        assert_eq!(plan.folders[1].first_cluster, plan.folders[0].first_cluster + plan.folders[0].clusters);
        check_back_to_back(&plan);
        write_stick(&mut dev, &plan, &mut |_| {}, None).unwrap();
        let v = verify_stick(&mut dev, &plan, true, &mut |_| {}).unwrap();
        assert!(v.ok(), "{v:?}");
        // A player reading it lists the folders in stick order.
        let mut vol = Volume::open(&mut dev).unwrap();
        let root = vol.list_dir(DirRef::Root).unwrap();
        let names: Vec<_> = root.groups.iter().filter(|g| g.kind == EntryKind::Dir).map(|g| g.name().to_string()).collect();
        assert_eq!(names, ["01 Road Trip", "02 Kids' car", "03 Sleep"]);
    }

    #[test]
    fn hidden_files_go_last_and_change_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.json");
        std::fs::write(&note, br#"{"stick":"Car"}"#).unwrap();
        for layout in ["folders", "top"] {
            let folders = if layout == "top" {
                vec![StickFolder { name: String::new(), items: sources(dir.path(), "t", &[40_000, 3, 9000]) }]
            } else {
                vec![
                    StickFolder { name: "01 Road Trip".into(), items: sources(dir.path(), "a", &[40_000, 3, 9000]) },
                    StickFolder { name: "02 Kids' car".into(), items: sources(dir.path(), "b", &[700]) },
                ]
            };
            let plain = StickContent { folders: folders.clone(), label: "Car".into(), hidden: Vec::new() };
            let marked = StickContent { hidden: vec![WriteItem::file(&note, ".fatstick.json")], ..plain.clone() };
            let size = 64 * MIB;
            let a = plan_content_at(size, 512, &plain, 1_700_000_000, 0).unwrap();
            let b = plan_content_at(size, 512, &marked, 1_700_000_000, 0).unwrap();
            // The tracks keep their order, back to back; the note comes after them.
            assert_eq!(planned_listing(&a), planned_listing(&b), "{layout}");
            check_back_to_back(&b);
            let last = b.files().map(|f| f.first_cluster + f.clusters).max().unwrap();
            assert_eq!(b.hidden[0].first_cluster, last);
            assert_eq!(b.hidden[0].short_name.trim(), "FATSTI~1JSO");
            let mut dev = Cursor::new(vec![0x5Au8; size as usize]);
            write_stick(&mut dev, &b, &mut |_| {}, None).unwrap();
            let v = verify_stick(&mut dev, &b, true, &mut |_| {}).unwrap();
            assert!(v.ok(), "{layout}: {v:?}");
            let back = read_top_level_file(&mut dev, ".FatStick.json", 1 << 20).unwrap();
            assert_eq!(back.as_deref(), Some(&br#"{"stick":"Car"}"#[..]));
            assert_eq!(read_top_level_file(&mut dev, "nothing.json", 1 << 20).unwrap(), None);
            // Hidden, and last at the top level.
            let mut vol = Volume::open(&mut dev).unwrap();
            let root = vol.list_dir(DirRef::Root).unwrap();
            let last = root.groups.last().unwrap();
            assert!(last.is_hidden() && last.name() == ".fatstick.json");
        }
    }

    #[test]
    fn files_are_assembled_from_parts() {
        let dir = tempfile::tempdir().unwrap();
        let tag = dir.path().join("tags.bin");
        std::fs::write(&tag, b"HEADER-ONE|HEADER-TWO|").unwrap();
        let audio = dir.path().join("audio.bin");
        let a: Vec<u8> = (0..300_000u32).map(|k| (k % 251) as u8).collect();
        std::fs::write(&audio, &a).unwrap();
        let items = vec![
            WriteItem { name: "one.mp3".into(), parts: vec![Part::range(&tag, 0, 11), Part::range(&audio, 1000, 200_000)] },
            WriteItem { name: "two.mp3".into(), parts: vec![Part::range(&tag, 11, 11), Part::whole(&audio)] },
        ];
        let size = 64 * MIB;
        let mut dev = Cursor::new(vec![0u8; size as usize]);
        let plan = plan_stick(size, 512, &items, &StickOptions::default()).unwrap();
        assert_eq!(plan.folders[0].files[0].size, 11 + 200_000);
        assert_eq!(plan.folders[0].files[1].size, 11 + 300_000);
        write_stick(&mut dev, &plan, &mut |_| {}, None).unwrap();
        assert!(verify_stick(&mut dev, &plan, true, &mut |_| {}).unwrap().ok());
        let raw = dev.get_ref();
        let f1 = &plan.folders[0].files[0];
        let at = plan.format.cluster_offset(f1.first_cluster) as usize;
        assert_eq!(&raw[at..at + 11], b"HEADER-ONE|");
        assert_eq!(&raw[at + 11..at + 11 + 100], &a[1000..1100]);
        // A range past the end of its file is refused before anything is written.
        let bad = vec![WriteItem { name: "x.mp3".into(), parts: vec![Part::range(&tag, 20, 10)] }];
        assert!(plan_stick(size, 512, &bad, &StickOptions::default()).is_err());
    }

    #[test]
    fn a_source_that_changes_fails_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let items = sources(dir.path(), "a", &[10_000, 20_000]);
        let size = 64 * MIB;
        let plan = plan_stick(size, 512, &items, &StickOptions::default()).unwrap();
        std::fs::write(&items[1].parts[0].path, b"shorter").unwrap();
        let mut dev = Cursor::new(vec![0u8; size as usize]);
        let e = write_stick(&mut dev, &plan, &mut |_| {}, None).unwrap_err();
        assert!(e.to_string().contains("changed"), "{e}");
    }

    #[test]
    fn cancelling_leaves_a_blank_stick() {
        let dir = tempfile::tempdir().unwrap();
        let items = sources(dir.path(), "a", &[3_000_000, 3_000_000]);
        let size = 64 * MIB;
        let plan = plan_stick(size, 512, &items, &StickOptions::default()).unwrap();
        let mut dev = Cursor::new(vec![0x77u8; size as usize]);
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let e = write_stick(
            &mut dev,
            &plan,
            &mut |p| {
                calls += 1;
                if p.stage == "write" && p.bytes_done > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            Some(&cancel),
        )
        .unwrap_err();
        assert!(matches!(e, FatError::Cancelled), "{e}");
        assert!(calls > 1);
        // No partition table, no boot sector: every OS sees a blank stick.
        let raw = dev.get_ref();
        assert!(raw[..PARTITION_START as usize * 512 + 512].iter().all(|&b| b == 0));
    }

    #[test]
    fn refuses_bad_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = sources(dir.path(), "a", &[10]);
        let size = 64 * MIB;
        let mixed = StickContent {
            folders: vec![StickFolder { name: String::new(), items: a.clone() }, StickFolder { name: "B".into(), items: a.clone() }],
            label: String::new(),
            hidden: Vec::new(),
        };
        assert!(plan_content(size, 512, &mixed).is_err());
        let twice = StickContent {
            folders: vec![StickFolder { name: "Same".into(), items: a.clone() }, StickFolder { name: "same".into(), items: a.clone() }],
            label: String::new(),
            hidden: Vec::new(),
        };
        assert!(plan_content(size, 512, &twice).unwrap_err().to_string().contains("two folders"));
        // A hidden file can't take a folder's name.
        let clash = StickContent {
            folders: vec![StickFolder { name: "Notes".into(), items: a.clone() }],
            label: String::new(),
            hidden: vec![WriteItem::file(&a[0].parts[0].path, "notes")],
        };
        assert!(plan_content(size, 512, &clash).unwrap_err().to_string().contains("twice at the top level"));
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(80 * MIB).unwrap();
        let items = vec![WriteItem::file(p, "big.mp3")];
        let e = plan_stick(64 * MIB, 512, &items, &StickOptions::default()).unwrap_err();
        assert!(e.to_string().contains("need"), "{e}");
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
