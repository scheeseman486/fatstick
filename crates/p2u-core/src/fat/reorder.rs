//! Rewriting a directory so its entries appear in a requested order.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::dir::{parse_directory, DirEntryGroup, EntryKind, ENTRY};
use super::{Device, DirRef, Extent, FatError, Result, Volume};

/// Key used to compare names the way FAT does: case-insensitive. NFC first,
/// because macOS may hand us decomposed (NFD) names.
pub fn normalize_name(s: &str) -> String {
    s.nfc().collect::<String>().to_uppercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnmatchedPlacement {
    /// Entries not named in the order list go after the ordered ones (default).
    #[default]
    After,
    /// Entries not named in the order list go before the ordered ones.
    Before,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReorderOptions {
    pub unmatched: UnmatchedPlacement,
    /// Compute everything but do not write.
    pub dry_run: bool,
    /// Write even if the volume looks mounted or the directory has orphaned long-name slots.
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorderReport {
    pub changed: bool,
    pub written: bool,
    pub ordered: Vec<String>,
    pub unmatched: Vec<String>,
    pub missing: Vec<String>,
    pub duplicates_in_order: Vec<String>,
    pub deleted_slots_reclaimed: usize,
    pub slots_used: usize,
    pub capacity_slots: usize,
    pub final_order: Vec<String>,
    pub warnings: Vec<String>,
}

/// The directory's bytes before a write, so it can be put back exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub format: u32,
    pub volume_serial: u32,
    pub partition_offset: u64,
    pub dir_path: String,
    pub extents: Vec<Extent>,
    #[serde(skip)]
    pub data: Vec<u8>,
}

impl Backup {
    /// One JSON header line, then the raw directory bytes.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        serde_json::to_writer(&mut f, self)?;
        f.write_all(b"\n")?;
        f.write_all(&self.data)?;
        f.sync_all()
    }

    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut header = String::new();
        r.read_line(&mut header)?;
        let mut b: Backup = serde_json::from_str(&header)?;
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut r, &mut data)?;
        let expected: u64 = b.extents.iter().map(|e| e.len).sum();
        if data.len() as u64 != expected {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "backup size mismatch"));
        }
        b.data = data;
        Ok(b)
    }
}

/// A computed rewrite of one directory, not yet written.
#[derive(Debug, Clone)]
pub struct PreparedReorder {
    pub report: ReorderReport,
    /// The directory's current bytes; save this before applying.
    pub backup: Backup,
    new: Vec<u8>,
    extents: Vec<Extent>,
}

/// How a folder's actual order compares with the order wanted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCheck {
    pub in_order: bool,
    pub expected: usize,
    pub found: usize,
    pub missing: Vec<String>,
    /// Files present in the folder but not in the playlist (a player would play them too).
    pub extra: Vec<String>,
    /// macOS AppleDouble files and similar that players may list as tracks.
    pub junk: Vec<String>,
    /// First playlist position (0-based) whose file is not where it should be.
    pub first_mismatch: Option<usize>,
    pub actual: Vec<String>,
}

pub fn is_junk_name(name: &str) -> bool {
    name.starts_with("._") || name == ".DS_Store" || name.eq_ignore_ascii_case("Thumbs.db") || name == "desktop.ini"
}

/// Compare the order of `actual` names (as stored in the directory) with `expected`.
pub fn check_order(actual: &[String], expected: &[String]) -> OrderCheck {
    let want: HashMap<String, usize> =
        expected.iter().enumerate().map(|(i, n)| (normalize_name(n), i)).collect();
    let mut seen_positions = Vec::new();
    let mut extra = Vec::new();
    let mut junk = Vec::new();
    for n in actual {
        match want.get(&normalize_name(n)) {
            Some(&i) => seen_positions.push(i),
            None if is_junk_name(n) => junk.push(n.clone()),
            None => extra.push(n.clone()),
        }
    }
    let present: std::collections::HashSet<usize> = seen_positions.iter().copied().collect();
    let missing: Vec<String> =
        expected.iter().enumerate().filter(|(i, _)| !present.contains(i)).map(|(_, n)| n.clone()).collect();
    let expected_present: Vec<usize> = (0..expected.len()).filter(|i| present.contains(i)).collect();
    let first_mismatch = seen_positions.iter().zip(expected_present.iter()).position(|(a, b)| a != b).map(|k| expected_present[k]);
    OrderCheck {
        in_order: first_mismatch.is_none() && missing.is_empty() && seen_positions.len() == expected_present.len(),
        expected: expected.len(),
        found: seen_positions.len(),
        missing,
        extra,
        junk,
        first_mismatch,
        actual: actual.to_vec(),
    }
}

impl<D: Device> Volume<D> {
    /// Names of the files and folders in a directory, in raw on-disk order.
    pub fn raw_order(&mut self, d: DirRef) -> Result<Vec<String>> {
        Ok(self
            .list_dir(d)?
            .groups
            .iter()
            .filter(|g| matches!(g.kind, EntryKind::File | EntryKind::Dir))
            .map(|g| g.name().to_string())
            .collect())
    }

    /// Work out how directory `d` must be rewritten so the entries named in
    /// `order` come first (or last), in that order, with deleted slots compacted
    /// away. Reads only; nothing is written until [`Volume::apply_reorder`].
    pub fn prepare_reorder(
        &mut self,
        d: DirRef,
        dir_path: &str,
        order: &[String],
        opts: &ReorderOptions,
    ) -> Result<PreparedReorder> {
        let mut warnings = Vec::new();
        let health = self.health(false)?;
        if health.boot_sector_dirty || !health.fat_clean_shutdown {
            let msg = "the volume is marked as in use (still mounted, or not cleanly ejected)";
            if opts.force || opts.dry_run {
                warnings.push(msg.to_string());
            } else {
                return Err(FatError::Unsafe(msg.into()));
            }
        }

        let extents = self.dir_extents(d)?;
        let old = self.read_extents(&extents)?;
        let listing = parse_directory(&old, d);
        if !listing.is_consistent() {
            let msg = format!(
                "directory has {} orphaned long-name slots; run fsck.fat / chkdsk first",
                listing.orphan_lfn_slots
            );
            if opts.force || opts.dry_run {
                warnings.push(msg);
            } else {
                return Err(FatError::Unsafe(msg));
            }
        }
        if listing.nonzero_after_end > 0 {
            warnings.push(format!(
                "{} non-empty slots after the end marker will be cleared",
                listing.nonzero_after_end
            ));
        }

        // Split into fixed head (label, ".", "..") and movable entries.
        let mut head: Vec<&DirEntryGroup> = Vec::new();
        let mut movable: Vec<&DirEntryGroup> = Vec::new();
        for g in &listing.groups {
            match g.kind {
                EntryKind::VolumeLabel | EntryKind::Dot | EntryKind::DotDot => head.push(g),
                _ => movable.push(g),
            }
        }
        // "." must precede "..".
        head.sort_by_key(|g| match g.kind {
            EntryKind::VolumeLabel => 0,
            EntryKind::Dot => 1,
            _ => 2,
        });

        let mut by_name: HashMap<String, usize> = HashMap::new();
        for (i, g) in movable.iter().enumerate() {
            by_name.entry(normalize_name(g.name())).or_insert(i);
            by_name.entry(normalize_name(&g.short_name)).or_insert(i);
        }
        let mut taken = vec![false; movable.len()];
        let mut ordered_idx = Vec::new();
        let mut missing = Vec::new();
        let mut dups = Vec::new();
        for name in order {
            match by_name.get(&normalize_name(name)) {
                Some(&i) if !taken[i] => {
                    taken[i] = true;
                    ordered_idx.push(i);
                }
                Some(_) => dups.push(name.clone()),
                None => missing.push(name.clone()),
            }
        }
        let rest_idx: Vec<usize> = (0..movable.len()).filter(|&i| !taken[i]).collect();
        let sequence: Vec<usize> = match opts.unmatched {
            UnmatchedPlacement::After => ordered_idx.iter().chain(rest_idx.iter()).copied().collect(),
            UnmatchedPlacement::Before => rest_idx.iter().chain(ordered_idx.iter()).copied().collect(),
        };

        // Build the new directory image.
        let mut new = vec![0u8; old.len()];
        let mut slot = 0usize;
        let mut put = |g: &DirEntryGroup, new: &mut Vec<u8>| {
            for s in &g.slots {
                new[slot * ENTRY..(slot + 1) * ENTRY].copy_from_slice(s);
                slot += 1;
            }
        };
        for g in &head {
            put(g, &mut new);
        }
        for &i in &sequence {
            put(movable[i], &mut new);
        }
        let used = slot;
        if used > listing.capacity_slots {
            return Err(FatError::Corrupt("entries do not fit (should be impossible)".into()));
        }

        let changed = new != old;
        let report = ReorderReport {
            changed,
            written: changed && !opts.dry_run,
            ordered: ordered_idx.iter().map(|&i| movable[i].name().to_string()).collect(),
            unmatched: rest_idx.iter().map(|&i| movable[i].name().to_string()).collect(),
            missing,
            duplicates_in_order: dups,
            deleted_slots_reclaimed: listing.deleted_slots,
            slots_used: used,
            capacity_slots: listing.capacity_slots,
            final_order: sequence.iter().map(|&i| movable[i].name().to_string()).collect(),
            warnings,
        };
        let backup = Backup {
            format: 1,
            volume_serial: self.geometry().volume_serial,
            partition_offset: self.geometry().partition_offset,
            dir_path: dir_path.to_string(),
            extents: extents.clone(),
            data: old,
        };
        Ok(PreparedReorder { report, backup, new, extents })
    }

    /// Write a prepared reorder, push it to the medium, drop any OS cache of the
    /// device and read the sectors back from the medium to confirm them.
    /// Refuses if the directory changed since it was prepared.
    pub fn apply_reorder(&mut self, p: &PreparedReorder) -> Result<()> {
        if !p.report.changed {
            return Ok(());
        }
        if self.read_extents(&p.extents)? != p.backup.data {
            return Err(FatError::Unsafe("the directory changed after it was read".into()));
        }
        self.write_extents(&p.extents, &p.new)?;
        self.invalidate_cache()?;
        if self.read_extents(&p.extents)? != p.new {
            return Err(FatError::VerifyFailed("directory bytes on the medium differ from what was written".into()));
        }
        Ok(())
    }

    /// Prepare and apply in one step. Returns the report and, if a write
    /// happened, the backup of the previous bytes. Callers that must keep the
    /// backup safe before writing use prepare/apply separately (see job.rs).
    pub fn reorder_dir(
        &mut self,
        d: DirRef,
        dir_path: &str,
        order: &[String],
        opts: &ReorderOptions,
    ) -> Result<(ReorderReport, Option<Backup>)> {
        let p = self.prepare_reorder(d, dir_path, order, opts)?;
        if !p.report.changed || opts.dry_run {
            return Ok((p.report, None));
        }
        self.apply_reorder(&p)?;
        Ok((p.report, Some(p.backup)))
    }

    /// Put a directory back exactly as a [`Backup`] recorded it.
    pub fn restore(&mut self, b: &Backup) -> Result<()> {
        if b.volume_serial != self.geometry().volume_serial || b.partition_offset != self.geometry().partition_offset {
            return Err(FatError::Unsafe("backup belongs to a different volume".into()));
        }
        let d = self.find_dir(&b.dir_path)?;
        if self.dir_extents(d)? != b.extents {
            return Err(FatError::Unsafe("directory layout changed since the backup".into()));
        }
        self.write_extents(&b.extents, &b.data)?;
        self.invalidate_cache()?;
        if self.read_extents(&b.extents)? != b.data {
            return Err(FatError::VerifyFailed("restored bytes differ on the medium".into()));
        }
        Ok(())
    }
}
