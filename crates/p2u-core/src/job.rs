//! The one privileged operation, packaged so the GUI, the CLI and the elevated
//! helper all run exactly the same code: erase the stick, write a fresh FAT32
//! file system with the playlists in FAT order and every file in one unbroken
//! run of clusters, then read it all back from the medium.
//!
//! "Written OK" is only ever reported from that read-back, never from what the
//! OS says about the stick.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::fat::stick::{plan_content, verify_stick, write_stick, StickContent, StickResult, StickVerification, WriteProgress};
use crate::fat::{Device, FatError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickJob {
    pub content: StickContent,
    /// Compare every byte with its source after writing (default on).
    #[serde(default = "yes")]
    pub verify_data: bool,
    /// Ask the device layer to eject the media afterwards (used by the Windows
    /// helper, whose disk would otherwise be remounted as soon as it exits).
    #[serde(default)]
    pub eject: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StickOutcome {
    pub ok: bool,
    pub error: Option<String>,
    /// Set once the first write happened: the stick's old contents are gone.
    #[serde(default)]
    pub erased: bool,
    /// The write was cancelled; the stick reads as blank.
    #[serde(default)]
    pub cancelled: bool,
    pub result: Option<StickResult>,
    pub verification: Option<StickVerification>,
}

impl StickOutcome {
    pub fn fail(e: impl ToString) -> Self {
        StickOutcome { ok: false, error: Some(e.to_string()), ..Default::default() }
    }
}

/// Plan, erase and write, then verify from the medium. `dev` is the whole disk.
pub fn run_stick<D: Device>(
    dev: &mut D,
    job: &StickJob,
    progress: &mut dyn FnMut(&WriteProgress),
    cancel: Option<&AtomicBool>,
) -> StickOutcome {
    let size = match dev.disk_size() {
        Ok(s) => s,
        Err(e) => return StickOutcome::fail(format!("could not read the stick's size: {e}")),
    };
    let sector = match dev.logical_sector_size() {
        Ok(s) => s,
        Err(e) => return StickOutcome::fail(format!("could not read the stick's sector size: {e}")),
    };
    let plan = match plan_content(size, sector, &job.content) {
        Ok(p) => p,
        Err(e) => return StickOutcome::fail(format!("nothing was written: {e}")),
    };
    let result = match write_stick(dev, &plan, progress, cancel) {
        Ok(r) => r,
        Err(FatError::Cancelled) => {
            return StickOutcome {
                ok: false,
                error: Some("cancelled while writing. The stick is blank now; sync again to fill it.".into()),
                erased: true,
                cancelled: true,
                ..Default::default()
            }
        }
        Err(e) => {
            return StickOutcome {
                ok: false,
                error: Some(format!("writing failed: {e}. The stick is now blank or incomplete; run the write again.")),
                erased: true,
                ..Default::default()
            }
        }
    };
    let mut o = StickOutcome { erased: true, result: Some(result), ..Default::default() };
    match verify_stick(dev, &plan, job.verify_data, progress) {
        Ok(v) => {
            o.ok = v.ok();
            if !o.ok {
                o.error = Some(
                    v.problems
                        .first()
                        .cloned()
                        .or_else(|| v.data_mismatches.first().map(|n| format!("{n} read back differently")))
                        .unwrap_or_else(|| "the check after writing failed".into()),
                );
            }
            o.verification = Some(v);
        }
        Err(e) => o.error = Some(format!("reading the stick back failed: {e}")),
    }
    o
}

/// Create a file that must not already exist (no symlink following on Unix).
pub fn create_exclusive(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(p)
}
