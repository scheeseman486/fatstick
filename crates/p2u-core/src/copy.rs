//! Ordered copy through the OS's own FAT driver. No elevation needed.
//!
//! Every driver we checked (Linux vfat, Windows fastfat, macOS msdosfs) puts a
//! new entry in the first free slot, and a newly created directory has nothing
//! but free slots after "." and "..". So creating the files one at a time, in
//! playlist order, inside a brand-new folder lays the entries out in that order.
//! What breaks it: temp-file-then-rename, parallel copies, deleting files
//! afterwards, and macOS writing `._` AppleDouble files for extended attributes.
//! This module avoids all four, and the raw reorder pass fixes anything left.

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
    /// Order as the OS reports it after the copy (see [`readdir_order`]).
    pub check: OrderCheck,
}

#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("destination folder {0} already exists and is not empty")]
    DestinationNotEmpty(String),
    #[error("not enough free space: need {need} bytes, {have} available")]
    NoSpace { need: u64, have: u64 },
    #[error("copy cancelled")]
    Cancelled,
}

/// Names in a folder in the order the OS returns them. FAT drivers return
/// entries in on-disk order, so this is an unprivileged (best-effort) view of
/// the raw order; only the raw read in [`crate::fat`] is authoritative.
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
    if dest.exists() {
        let empty = fs::read_dir(dest)?.next().is_none();
        if !empty {
            if opts.replace_existing {
                fs::remove_dir_all(dest)?;
            } else {
                return Err(CopyError::DestinationNotEmpty(dest.display().to_string()));
            }
        } else {
            // An empty folder may still hold deleted slots; recreate it so the
            // driver starts from a zeroed cluster.
            fs::remove_dir(dest)?;
        }
    }
    fs::create_dir(dest)?;

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
        let name = format!("{}.m3u", plan.folder_name);
        let mut f = OpenOptions::new().write(true).create_new(true).open(dest.join(name))?;
        f.write_all(m3u.as_bytes())?;
    }
    let junk_removed = remove_junk(dest)?;
    let check = check_order(&readdir_order(dest)?, &plan.target_names());
    Ok(CopyReport { files_copied: total, bytes_copied: bytes, junk_removed, check })
}
