//! The one privileged operation, packaged so the GUI, the CLI and the elevated
//! helper all run exactly the same code: find a folder on a raw FAT volume,
//! reorder its entries, keep a backup, and verify the result by re-reading.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fat::{check_order, Device, OrderCheck, ReorderOptions, ReorderReport, Volume};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockJob {
    /// Folder inside the volume, e.g. "/Road Trip".
    pub dir_path: String,
    /// Entry names in the wanted order.
    pub order: Vec<String>,
    #[serde(default)]
    pub options: ReorderOptions,
    /// Which FAT partition when the device is a whole disk (1-based).
    #[serde(default)]
    pub partition: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub report: Option<ReorderReport>,
    pub check: Option<OrderCheck>,
    pub backup_file: Option<PathBuf>,
}

impl LockOutcome {
    fn fail(e: impl ToString) -> Self {
        LockOutcome { ok: false, error: Some(e.to_string()), report: None, check: None, backup_file: None }
    }
}

/// Run a lock job against an open raw device. `backup_dir` receives a backup
/// of the directory's previous bytes (created exclusively, never overwritten).
pub fn run_lock<D: Device>(dev: D, job: &LockJob, backup_dir: Option<&Path>) -> LockOutcome {
    let mut vol = match Volume::open(dev, job.partition) {
        Ok(v) => v,
        Err(e) => return LockOutcome::fail(e),
    };
    let dir = match vol.find_dir(&job.dir_path) {
        Ok(d) => d,
        Err(e) => return LockOutcome::fail(e),
    };
    let (report, backup) = match vol.reorder_dir(dir, &job.dir_path, &job.order, &job.options) {
        Ok(r) => r,
        Err(e) => return LockOutcome::fail(e),
    };
    let mut backup_file = None;
    if let (Some(b), Some(d)) = (backup, backup_dir) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_secs())
            .unwrap_or(0);
        let p = d.join(format!("p2u-backup-{:08x}-{stamp}.bin", vol.geometry().volume_serial));
        // Written after the reorder succeeded; the data is the pre-write state.
        match create_exclusive(&p).and_then(|_| b.save(&p)) {
            Ok(()) => backup_file = Some(p),
            Err(e) => {
                let mut o = LockOutcome::fail(format!("reordered, but the backup could not be saved: {e}"));
                o.report = Some(report);
                return o;
            }
        }
    }
    let check = match vol.raw_order(dir) {
        Ok(actual) => check_order(&actual, &job.order),
        Err(e) => return LockOutcome::fail(e),
    };
    LockOutcome { ok: check.in_order || job.options.dry_run, error: None, report: Some(report), check: Some(check), backup_file }
}

/// Create a file that must not already exist (no symlink following on Unix).
pub fn create_exclusive(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(p)
}
