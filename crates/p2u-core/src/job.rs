//! The one privileged operation, packaged so the GUI, the CLI and the elevated
//! helper all run exactly the same code:
//!
//! 1. read the folder's directory at FAT level and compute the new layout;
//! 2. save an undo backup of the current bytes (before anything is written);
//! 3. write the new directory sectors, flush, drop the OS cache, read them back
//!    from the medium and compare;
//! 4. re-parse the folder from the medium and check the playlist order and
//!    every file's size against what was copied.
//!
//! "In order" is only ever reported from step 4, never from the OS's own
//! directory listing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fat::{check_order, normalize_name, Device, DirRef, EntryKind, OrderCheck, ReorderOptions, ReorderReport, Volume};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockJob {
    /// Folder inside the volume, e.g. "/Road Trip", or "/" for the top level.
    pub dir_path: String,
    /// Entry names in the wanted order.
    pub order: Vec<String>,
    /// Expected file sizes, parallel to `order` (catches truncated copies).
    #[serde(default)]
    pub expected_sizes: Option<Vec<u64>>,
    #[serde(default)]
    pub options: ReorderOptions,
    /// Which FAT partition when the device is a whole disk (1-based).
    #[serde(default)]
    pub partition: Option<usize>,
    /// Ask the device layer to eject the media afterwards (used by the Windows
    /// helper, whose volume would otherwise be remounted as soon as it exits).
    #[serde(default)]
    pub eject: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeMismatch {
    pub name: String,
    pub expected: u64,
    pub on_stick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub report: Option<ReorderReport>,
    /// Order as re-read from the medium after the write.
    pub check: Option<OrderCheck>,
    #[serde(default)]
    pub size_mismatches: Vec<SizeMismatch>,
    pub backup_file: Option<PathBuf>,
}

impl LockOutcome {
    fn fail(e: impl ToString) -> Self {
        LockOutcome {
            ok: false,
            error: Some(e.to_string()),
            report: None,
            check: None,
            size_mismatches: vec![],
            backup_file: None,
        }
    }
}

/// Run a lock job against an open raw device. `backup_dir` receives a backup
/// of the directory's previous bytes, written (exclusively, never overwriting)
/// before the directory is touched; if it cannot be saved nothing is written.
pub fn run_lock<D: Device>(dev: D, job: &LockJob, backup_dir: Option<&Path>) -> LockOutcome {
    let mut vol = match Volume::open(dev, job.partition) {
        Ok(v) => v,
        Err(e) => return LockOutcome::fail(e),
    };
    let dir = match vol.find_dir(&job.dir_path) {
        Ok(d) => d,
        Err(e) => return LockOutcome::fail(e),
    };
    let prepared = match vol.prepare_reorder(dir, &job.dir_path, &job.order, &job.options) {
        Ok(p) => p,
        Err(e) => return LockOutcome::fail(e),
    };
    let mut backup_file = None;
    if prepared.report.changed && !job.options.dry_run {
        if let Some(d) = backup_dir {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|t| t.as_nanos())
                .unwrap_or(0);
            let p = d.join(format!("p2u-backup-{:08x}-{stamp}.bin", vol.geometry().volume_serial));
            if let Err(e) = create_exclusive(&p).and_then(|_| prepared.backup.save(&p)) {
                return LockOutcome::fail(format!("nothing was written: the undo backup could not be saved ({e})"));
            }
            backup_file = Some(p);
        }
        if let Err(e) = vol.apply_reorder(&prepared) {
            let mut o = LockOutcome::fail(e);
            o.backup_file = backup_file;
            return o;
        }
    }

    // Verify from the medium: order, and each file's size.
    let (check, size_mismatches) = match verify_on_medium(&mut vol, dir, &job.order, job.expected_sizes.as_deref()) {
        Ok(v) => v,
        Err(e) => return LockOutcome::fail(e),
    };
    let ok = job.options.dry_run || (check.in_order && size_mismatches.is_empty());
    LockOutcome { ok, error: None, report: Some(prepared.report), check: Some(check), size_mismatches, backup_file }
}

/// Re-read a folder from the medium (OS cache dropped first) and compare its
/// raw entry order and file sizes with what is expected.
pub fn verify_on_medium<D: Device>(
    vol: &mut Volume<D>,
    dir: DirRef,
    order: &[String],
    expected_sizes: Option<&[u64]>,
) -> crate::fat::Result<(OrderCheck, Vec<SizeMismatch>)> {
    vol.drop_caches()?;
    let listing = vol.list_dir(dir)?;
    let actual: Vec<String> = listing
        .groups
        .iter()
        .filter(|g| matches!(g.kind, EntryKind::File | EntryKind::Dir))
        .map(|g| g.name().to_string())
        .collect();
    let check = check_order(&actual, order);
    let mut size_mismatches = Vec::new();
    if let Some(sizes) = expected_sizes {
        for (name, &expected) in order.iter().zip(sizes) {
            let key = normalize_name(name);
            let found = listing.groups.iter().find(|g| {
                g.kind == EntryKind::File && (normalize_name(g.name()) == key || normalize_name(&g.short_name) == key)
            });
            if let Some(g) = found {
                if g.size as u64 != expected {
                    size_mismatches.push(SizeMismatch { name: name.clone(), expected, on_stick: g.size as u64 });
                }
            }
        }
    }
    Ok((check, size_mismatches))
}

/// Create a file that must not already exist (no symlink following on Unix).
pub fn create_exclusive(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(p)
}

// ---------------------------------------------------------------------------
// Raw write: the files themselves, placed and ordered at FAT level.

use crate::fat::write::{WriteItem, WriteOptions, WriteProgress, WriteResult, WriteVerification};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteJob {
    /// "/Road Trip" creates that folder; "/" writes to the top level.
    pub target_dir: String,
    pub items: Vec<WriteItem>,
    #[serde(default)]
    pub options: WriteOptions,
    /// Compare every byte on the stick with the source after writing.
    #[serde(default = "default_true")]
    pub verify_data: bool,
    #[serde(default)]
    pub partition: Option<usize>,
    /// Eject the media afterwards (Windows helper; see LockJob::eject).
    #[serde(default)]
    pub eject: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub result: Option<WriteResult>,
    pub verification: Option<WriteVerification>,
}

/// Write the files at FAT level, then verify everything from the medium.
pub fn run_write<D: Device>(
    dev: D,
    job: &WriteJob,
    backup_dir: Option<&Path>,
    progress: &mut dyn FnMut(&WriteProgress),
) -> WriteOutcome {
    let fail = |e: String| WriteOutcome { ok: false, error: Some(e), result: None, verification: None };
    let mut vol = match Volume::open(dev, job.partition) {
        Ok(v) => v,
        Err(e) => return fail(e.to_string()),
    };
    let result = match vol.write_playlist(&job.target_dir, &job.items, &job.options, backup_dir, progress) {
        Ok(r) => r,
        Err(e) => return fail(e.to_string()),
    };
    if job.options.dry_run {
        return WriteOutcome { ok: true, error: None, result: Some(result), verification: None };
    }
    match vol.verify_written(&result, &job.items, job.verify_data, progress) {
        Ok(v) => WriteOutcome { ok: v.ok(), error: None, result: Some(result), verification: Some(v) },
        Err(e) => WriteOutcome { ok: false, error: Some(format!("written, but verification failed to run: {e}")), result: Some(result), verification: None },
    }
}

/// What the elevated helper is asked to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HelperJob {
    Lock(LockJob),
    Write(WriteJob),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HelperOutcome {
    Lock(LockOutcome),
    Write(WriteOutcome),
}
