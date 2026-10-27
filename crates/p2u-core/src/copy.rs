//! Copying the files through the OS's own FAT driver. No elevation needed.
//!
//! This step only gets the bytes onto the stick. The order the driver leaves
//! the entries in is NOT trusted: overwriting an earlier copy keeps each file's
//! old slot, copying into an existing folder or the top level fills deleted
//! slots, and Linux, Windows and macOS pick slots differently (tests/kernel_vfat.py
//! shows all of these with the real Linux driver). The FAT-level step in
//! `job.rs` always runs afterwards and is what decides and verifies the order.
//!
//! The copy still avoids what would make that step harder or the stick messier:
//! temp-file-then-rename, parallel copies, and macOS `._` AppleDouble files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::fat::{check_order, is_junk_name, OrderCheck};
use crate::plan::Plan;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CopyOptions {
    /// Delete the destination folder first if it already exists.
    pub replace_existing: bool,
    /// Give files modification times 2 s apart in playlist order (FAT time
    /// resolution), for players that sort by date and for `fatsort -t`.
    pub stamp_times: bool,
    /// Also write an .m3u (relative names) as the last file in the folder.
    pub write_m3u: bool,
    /// fsync each file before starting the next.
    pub sync_each_file: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CopyProgress {
    pub index: usize,
    pub total: usize,
    pub name: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CopyReport {
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub junk_removed: Vec<String>,
    /// The order the OS lists right after copying, for diagnostics only.
    /// Not a guarantee of on-disk order; the FAT-level step (job.rs) decides.
    pub os_listing: OrderCheck,
}

#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("destination folder {0} already exists and is not empty")]
    DestinationNotEmpty(String),
    #[error("{0} already exists on the stick (allow replacing to overwrite it)")]
    FileExists(String),
    #[error("not enough free space: need {need} bytes, {have} available")]
    NoSpace { need: u64, have: u64 },
    #[error("copy cancelled")]
    Cancelled,
}

/// Names in a folder in the order the OS lists them. Diagnostics only: nothing
/// guarantees a driver lists in on-disk order (macOS's user-space FAT driver
/// is unpublished), so only the raw read in [`crate::fat`] decides "in order".
pub fn readdir_order(dir: &Path) -> io::Result<Vec<String>> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir)? {
        v.push(e?.file_name().to_string_lossy().into_owned());
    }
    Ok(v)
}

/// Copy file contents only. `std::fs::copy` is avoided on purpose: on macOS it
/// calls `fcopyfile` with metadata flags, which copies extended attributes and
/// makes the FAT driver write `._name` AppleDouble files next to each track.
fn copy_contents(src: &Path, dst: &Path, buf: &mut [u8], mut on_bytes: impl FnMut(u64)) -> io::Result<u64> {
    let mut input = File::open(src)?;
    let mut out = OpenOptions::new().write(true).create_new(true).open(dst)?;
    let mut total = 0u64;
    loop {
        let n = match input.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        out.write_all(&buf[..n])?;
        total += n as u64;
        on_bytes(total);
    }
    out.flush()?;
    Ok(total)
}

fn remove_junk(dir: &Path) -> io::Result<Vec<String>> {
    let mut removed = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let n = e.file_name().to_string_lossy().into_owned();
        if is_junk_name(&n) && e.file_type()?.is_file() {
            fs::remove_file(e.path())?;
            removed.push(n);
        }
    }
    Ok(removed)
}

/// Copy the plan's files into a fresh folder, strictly one after another.
/// `progress` returns false to cancel.
pub fn execute_copy(
    plan: &Plan,
    opts: &CopyOptions,
    available_bytes: Option<u64>,
    mut progress: impl FnMut(&CopyProgress) -> bool,
) -> Result<CopyReport, CopyError> {
    if let Some(have) = available_bytes {
        // Leave room for cluster rounding: worst case one cluster per file (64 KiB max).
        let need = plan.total_bytes + plan.items.len() as u64 * 65536;
        if need > have {
            return Err(CopyError::NoSpace { need, have });
        }
    }
    let dest = &plan.dest_dir;
    if plan.is_top_level() {
        // The top level can't be recreated: copy beside whatever is there.
        // Replacing deletes the old file first so it is written fresh.
        for item in &plan.items {
            let t = dest.join(&item.target_name);
            if t.exists() {
                if opts.replace_existing {
                    fs::remove_file(&t)?;
                } else {
                    return Err(CopyError::FileExists(item.target_name.clone()));
                }
            }
        }
    } else {
        if dest.exists() {
            let empty = fs::read_dir(dest)?.next().is_none();
            if !empty && !opts.replace_existing {
                return Err(CopyError::DestinationNotEmpty(dest.display().to_string()));
            }
            fs::remove_dir_all(dest)?;
        }
        fs::create_dir(dest)?;
    }

    let total = plan.items.len();
    let base_time = SystemTime::now() - Duration::from_secs(2 * total as u64 + 2);
    let mut buf = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    for (i, item) in plan.items.iter().enumerate() {
        let target = dest.join(&item.target_name);
        let mut p = CopyProgress { index: i, total, name: item.target_name.clone(), bytes_done: 0, bytes_total: item.size };
        if !progress(&p) {
            return Err(CopyError::Cancelled);
        }
        let n = copy_contents(&item.source, &target, &mut buf, |d| p.bytes_done = d)?;
        if opts.stamp_times {
            let t = base_time + Duration::from_secs(2 * i as u64);
            filetime::set_file_mtime(&target, filetime::FileTime::from_system_time(t))?;
        }
        if opts.sync_each_file {
            File::open(&target)?.sync_all()?;
        }
        bytes += n;
        p.bytes_done = n;
        if !progress(&p) {
            return Err(CopyError::Cancelled);
        }
    }
    if opts.write_m3u {
        let mut m3u = String::from("#EXTM3U\r\n");
        for item in &plan.items {
            if let Some(t) = &item.title {
                m3u.push_str(&format!("#EXTINF:-1,{t}\r\n"));
            }
            m3u.push_str(&item.target_name);
            m3u.push_str("\r\n");
        }
        let base = if plan.is_top_level() { &plan.playlist_name } else { &plan.folder_name };
        let name = format!("{base}.m3u");
        let mut f = OpenOptions::new().write(true).create_new(true).open(dest.join(name))?;
        f.write_all(m3u.as_bytes())?;
    }
    let junk_removed = if plan.is_top_level() {
        // Only our own files' AppleDouble twins; leave the user's other files alone.
        let mut v = Vec::new();
        for item in &plan.items {
            let twin = dest.join(format!("._{}", item.target_name));
            if twin.is_file() {
                fs::remove_file(&twin)?;
                v.push(format!("._{}", item.target_name));
            }
        }
        v
    } else {
        remove_junk(dest)?
    };
    let os_listing = check_order(&readdir_order(dest)?, &plan.target_names());
    Ok(CopyReport { files_copied: total, bytes_copied: bytes, junk_removed, os_listing })
}
