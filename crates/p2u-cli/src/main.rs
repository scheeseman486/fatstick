//! p2u: command-line front end for Playlist2USB.
//!
//! The order on the stick is always set and checked at FAT level. Copying
//! through the OS only gets the bytes there; overwrites, deleted slots and
//! top-level copies leave the OS's own order wrong (tests/kernel_vfat.py).
//!
//! Typical session:
//!   p2u volumes                                        # find the stick
//!   p2u copy trip.xspf --dest /media/me/STICK --lock   # copy, then FAT-level order + verify
//! Or in two steps:
//!   p2u copy trip.xspf --dest /media/me/STICK
//!   p2u lock /org/freedesktop/UDisks2/block_devices/sdb1 --playlist trip.xspf
//! Image files and unmounted devices you can open work with sort/verify/ls:
//!   p2u sort stick.img --playlist trip.xspf

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use p2u_core::copy::{execute_copy, readdir_order, CopyOptions};
use p2u_core::fat::write::{WriteItem, WriteOptions, WriteProgress};
use p2u_core::fat::{check_order, Backup, EntryKind, FaultInjector, ReorderOptions, UnmatchedPlacement, Volume};
use p2u_core::job::{run_lock, run_write, verify_on_medium, LockJob, LockOutcome, WriteJob, WriteOutcome};
use p2u_core::plan::{make_plan, Plan, PlanOptions};
use p2u_device::VolumeInfo;

#[derive(Parser)]
#[command(name = "p2u", version, about = "Copy playlists to FAT32 USB sticks in the order cheap players actually use")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct NamingArgs {
    /// Folder name to create (default: playlist name)
    #[arg(long, conflicts_with = "top_level")]
    folder: Option<String>,
    /// Put the files at the top level of the stick instead of in a folder
    #[arg(long)]
    top_level: bool,
    /// Do not prefix file names with track numbers
    #[arg(long)]
    no_number: bool,
    /// Transliterate names to ASCII
    #[arg(long)]
    ascii: bool,
}

impl NamingArgs {
    fn opts(&self) -> PlanOptions {
        let folder_name = if self.top_level { Some(String::new()) } else { self.folder.clone() };
        PlanOptions { folder_name, number_prefix: !self.no_number, ascii_names: self.ascii }
    }
}

#[derive(Args)]
struct OrderSource {
    /// Text file with one entry name per line
    #[arg(long, conflicts_with = "playlist")]
    order_file: Option<PathBuf>,
    /// Derive names (and expected sizes) from a playlist, with the same naming options as `copy`
    #[arg(long)]
    playlist: Option<PathBuf>,
    #[command(flatten)]
    naming: NamingArgs,
}

struct ResolvedOrder {
    names: Vec<String>,
    sizes: Option<Vec<u64>>,
    dir: Option<String>,
}

impl OrderSource {
    fn resolve(&self) -> Result<ResolvedOrder, String> {
        if let Some(f) = &self.order_file {
            let names = p2u_core::read_order_file(f).map_err(|e| e.to_string())?;
            return Ok(ResolvedOrder { names, sizes: None, dir: None });
        }
        if let Some(pl) = &self.playlist {
            let p = load_plan(pl, Path::new("/"), &self.naming)?;
            return Ok(ResolvedOrder { names: p.target_names(), sizes: Some(p.expected_sizes()), dir: Some(p.volume_path) });
        }
        Err("give --order-file or --playlist".into())
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// List removable FAT volumes
    Volumes,
    /// Show what `copy` would do
    Plan {
        playlist: PathBuf,
        /// Volume mount point (drive root)
        #[arg(long)]
        dest: PathBuf,
        #[command(flatten)]
        naming: NamingArgs,
        /// Also write the target names to this order file
        #[arg(long)]
        order_out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Write a playlist straight into the FAT volume: FAT order and file placement are
    /// chosen here, then the order, layout and every byte are verified from the stick
    Write {
        /// A volume id/device from `p2u volumes` (uses the OS prompt), or an
        /// unmounted device or disk image you can already open
        target: String,
        #[arg(long)]
        playlist: PathBuf,
        #[command(flatten)]
        naming: NamingArgs,
        /// Replace an existing folder of that name (or same-named top-level files)
        #[arg(long)]
        replace: bool,
        /// If no single free block is large enough, allow files to be split
        /// (still in ascending cluster order). Default: refuse
        #[arg(long)]
        allow_fragmented: bool,
        /// Skip comparing every byte with the source after writing
        #[arg(long)]
        no_verify_data: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        partition: Option<usize>,
        /// Keep undo backups here (default: current directory)
        #[arg(long, default_value = ".")]
        backup_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Copy a playlist through the OS to a mounted stick; with --lock, then set and verify the order at FAT level
    Copy {
        playlist: PathBuf,
        /// Volume mount point (drive root)
        #[arg(long)]
        dest: PathBuf,
        #[command(flatten)]
        naming: NamingArgs,
        /// Replace the folder (or same-named top-level files) if they exist
        #[arg(long)]
        replace: bool,
        /// Space modification times 2 s apart in playlist order
        #[arg(long)]
        stamp_times: bool,
        /// Also write an .m3u next to the tracks
        #[arg(long)]
        write_m3u: bool,
        /// fsync after every file
        #[arg(long)]
        sync: bool,
        /// After copying: unmount, reorder at FAT level, verify from the stick (one OS prompt)
        #[arg(long)]
        lock: bool,
        /// Keep undo backups here (default: current directory)
        #[arg(long, default_value = ".")]
        backup_dir: PathBuf,
    },
    /// Show a volume's geometry and health flags
    Inspect {
        device: PathBuf,
        #[arg(long)]
        partition: Option<usize>,
        /// Also compare all FAT copies
        #[arg(long)]
        compare_fats: bool,
    },
    /// List a directory in raw on-disk order
    Ls {
        device: PathBuf,
        #[arg(long, default_value = "/")]
        dir: String,
        #[arg(long)]
        partition: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Reorder a directory on an unmounted device or image you can already open
    Sort {
        device: PathBuf,
        /// Folder inside the volume (default: from --playlist)
        #[arg(long)]
        dir: Option<String>,
        #[command(flatten)]
        order: OrderSource,
        #[arg(long)]
        partition: Option<usize>,
        /// Put entries not in the order list first instead of last
        #[arg(long)]
        unmatched_first: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
        /// Where to keep the undo backup (default: current directory)
        #[arg(long, default_value = ".")]
        backup_dir: PathBuf,
    },
    /// Check a directory's raw order (and sizes, with --playlist) on a device or image
    Verify {
        device: PathBuf,
        #[arg(long)]
        dir: Option<String>,
        #[command(flatten)]
        order: OrderSource,
        #[arg(long)]
        partition: Option<usize>,
    },
    /// What the OS lists for a mounted folder. Diagnostics only: not a guarantee of on-disk order
    OsListing {
        /// The copied folder on the mounted volume
        path: PathBuf,
        #[command(flatten)]
        order: OrderSource,
    },
    /// Undo a sort or a write using its backup file(s); several are applied newest first
    Restore {
        device: PathBuf,
        #[arg(required = true)]
        backups: Vec<PathBuf>,
    },
    /// Unmount a removable volume, reorder at FAT level via the OS prompt, verify from the stick
    Lock {
        /// Volume id or device from `p2u volumes`
        volume: String,
        #[arg(long)]
        dir: Option<String>,
        #[command(flatten)]
        order: OrderSource,
        #[arg(long, default_value = ".")]
        backup_dir: PathBuf,
        /// Mount the volume again afterwards (default: leave it unmounted, safe to unplug)
        #[arg(long)]
        remount: bool,
    },
}

fn load_plan(playlist: &Path, dest: &Path, naming: &NamingArgs) -> Result<Plan, String> {
    let pl = p2u_core::playlist::load(playlist).map_err(|e| e.to_string())?;
    for w in &pl.warnings {
        eprintln!("warning: {w}");
    }
    Ok(make_plan(&pl, dest, &naming.opts()))
}

fn open_rw(path: &Path) -> Result<std::fs::File, String> {
    OpenOptions::new().read(true).write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

fn human(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GiB", mb / 1024.0)
    } else {
        format!("{mb:.1} MiB")
    }
}

fn find_volume(key: &str) -> Result<VolumeInfo, String> {
    p2u_device::list_volumes()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|v| v.id == key || v.device == key)
        .ok_or_else(|| format!("no removable FAT volume {key:?}; see `p2u volumes`"))
}

fn volume_mounted_at(dest: &Path) -> Result<VolumeInfo, String> {
    let want = dest.canonicalize().map_err(|e| e.to_string())?;
    p2u_device::list_volumes()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|v| v.mount_point.as_ref().and_then(|m| m.canonicalize().ok()).as_deref() == Some(want.as_path()))
        .ok_or_else(|| format!("{} is not the mount point of a removable FAT volume", dest.display()))
}

/// Unmount, get raw access through the OS broker (or the elevated helper),
/// run the lock job, and leave the volume unmounted unless asked to remount.
fn lock_volume(vol: &VolumeInfo, mut job: LockJob, backup_dir: &Path, remount: bool) -> Result<LockOutcome, String> {
    match p2u_device::acquire(vol) {
        Ok(mut raw) => {
            let out = run_lock(&mut raw, &job, Some(backup_dir));
            let _ = raw.eject();
            drop(raw);
            if remount || !out.ok {
                if let Ok(Some(m)) = p2u_device::remount_after(vol) {
                    eprintln!("Mounted again at {}", m.display());
                }
            } else {
                eprintln!("The stick is left unmounted: safe to unplug.");
            }
            Ok(out)
        }
        Err(p2u_device::DeviceError::NeedsElevation) | Err(p2u_device::DeviceError::NoBroker(_)) => {
            job.eject = !remount;
            p2u_device::lock_via_helper(vol, &job).map_err(|e| e.to_string())
        }
        Err(e) => {
            let _ = p2u_device::remount_after(vol);
            Err(e.to_string())
        }
    }
}

fn summarize(out: &LockOutcome) {
    if out.ok {
        if let Some(c) = &out.check {
            eprintln!("Verified on the stick: {} of {} tracks in playlist order.", c.found, c.expected);
            if !c.extra.is_empty() {
                eprintln!("Note: {} other entries in the folder come after the playlist (players will play them too).", c.extra.len());
            }
        }
    } else if let Some(e) = &out.error {
        eprintln!("NOT locked: {e}");
    } else {
        if let Some(c) = &out.check {
            if !c.in_order {
                eprintln!("NOT in order: {} missing, first wrong position {:?}.", c.missing.len(), c.first_mismatch);
            }
        }
        for m in &out.size_mismatches {
            eprintln!("Size mismatch: {} is {} bytes on the stick, expected {}.", m.name, m.on_stick, m.expected);
        }
    }
}

fn summarize_write(out: &WriteOutcome) {
    if let Some(e) = &out.error {
        eprintln!("NOT written: {e}");
    }
    let Some(r) = &out.result else { return };
    let mb = r.sizes.iter().sum::<u64>() as f64 / 1_000_000.0;
    let l = &r.layout;
    let first = l.files.first().map(|f| f.first_cluster).unwrap_or(0);
    let last = l.files.iter().rev().find(|f| f.clusters > 0).map(|f| f.first_cluster + f.clusters - 1).unwrap_or(0);
    let shape = if l.contiguous {
        format!("one unbroken block, clusters {first}-{last}, files back to back in playlist order")
    } else if l.ascending {
        let split = l.files.iter().filter(|f| f.fragments > 1).count();
        format!("ascending playlist order, {split} file(s) split across free blocks")
    } else {
        "NOT in ascending order".to_string()
    };
    println!(
        "{} {} files ({mb:.1} MB) to {}: {shape}.",
        if r.written { "Wrote" } else { "Would write" },
        r.names.len(),
        r.target_dir
    );
    if !r.deleted.is_empty() {
        println!("Replaced: {}", r.deleted.join(", "));
    }
    if let Some(v) = &out.verification {
        let tick = |b: bool| if b { "ok" } else { "FAILED" };
        println!(
            "Verified from the stick: FAT order {}, layout {}, FAT copies {}, data {}.",
            tick(v.order_ok),
            tick(v.layout_matches_plan),
            tick(v.fat_copies_identical),
            if v.data_mismatches.is_empty() { "ok".to_string() } else { format!("{} file(s) differ", v.data_mismatches.len()) }
        );
        for p in &v.problems {
            println!("  problem: {p}");
        }
    }
    for b in &r.backups {
        println!("Undo backup: {}", b.display());
    }
}

fn run(cli: Cli) -> Result<bool, String> {
    match cli.cmd {
        Cmd::Volumes => {
            let vols = p2u_device::list_volumes().map_err(|e| e.to_string())?;
            if vols.is_empty() {
                println!("No removable FAT volumes found.");
            }
            for v in vols {
                println!(
                    "{}\n    device {}  label {:?}  {}  {}  mounted at {}",
                    v.id,
                    v.device,
                    v.label,
                    v.fs_type,
                    human(v.size_bytes),
                    v.mount_point.map(|m| m.display().to_string()).unwrap_or_else(|| "-".into())
                );
            }
            Ok(true)
        }
        Cmd::Plan { playlist, dest, naming, order_out, json } => {
            let plan = load_plan(&playlist, &dest, &naming)?;
            if let Some(o) = order_out {
                p2u_core::write_order_file(&o, &plan.target_names()).map_err(|e| e.to_string())?;
            }
            if json {
                print_json(&plan);
            } else {
                println!("Folder: {} ({} files, {})", plan.dest_dir.display(), plan.items.len(), human(plan.total_bytes));
                for i in &plan.items {
                    println!("  {}  <-  {}", i.target_name, i.source.display());
                }
                for m in &plan.missing {
                    println!("  MISSING  {m}");
                }
                for s in &plan.skipped {
                    println!("  SKIPPED  {s}");
                }
            }
            Ok(plan.missing.is_empty())
        }
        Cmd::Copy { playlist, dest, naming, replace, stamp_times, write_m3u, sync, lock, backup_dir } => {
            let plan = load_plan(&playlist, &dest, &naming)?;
            for m in &plan.missing {
                eprintln!("warning: missing, skipped: {m}");
            }
            // Resolve the volume first so --lock fails before copying, not after.
            let vol = if lock { Some(volume_mounted_at(&dest)?) } else { None };
            let opts = CopyOptions { replace_existing: replace, stamp_times, write_m3u, sync_each_file: sync };
            let report = execute_copy(&plan, &opts, None, |p| {
                if p.bytes_done == 0 {
                    eprintln!("[{}/{}] {}", p.index + 1, p.total, p.name);
                }
                true
            })
            .map_err(|e| e.to_string())?;
            println!("Copied {} files ({}) to {}.", report.files_copied, human(report.bytes_copied), plan.volume_path);
            let Some(vol) = vol else {
                println!("The order on the stick is not set yet. Run:");
                println!("  p2u lock <volume> --playlist {:?}{}", playlist, if plan.is_top_level() { " --top-level" } else { "" });
                return Ok(true);
            };
            let job = LockJob {
                dir_path: plan.volume_path.clone(),
                order: plan.target_names(),
                expected_sizes: Some(plan.expected_sizes()),
                options: ReorderOptions::default(),
                partition: None,
                eject: true,
            };
            let out = lock_volume(&vol, job, &backup_dir, false)?;
            summarize(&out);
            Ok(out.ok)
        }
        Cmd::Inspect { device, partition, compare_fats } => {
            let mut vol = Volume::open(open_rw(&device)?, partition).map_err(|e| e.to_string())?;
            let h = vol.health(compare_fats).map_err(|e| e.to_string())?;
            print_json(&serde_json::json!({ "geometry": vol.geometry(), "health": h }));
            Ok(true)
        }
        Cmd::Ls { device, dir, partition, json } => {
            let mut vol = Volume::open(open_rw(&device)?, partition).map_err(|e| e.to_string())?;
            let d = vol.find_dir(&dir).map_err(|e| e.to_string())?;
            let l = vol.list_dir(d).map_err(|e| e.to_string())?;
            if json {
                print_json(&l);
            } else {
                println!(
                    "{}: {} entries, {} slots used of {}, {} deleted, {} orphaned LFN slots",
                    dir,
                    l.groups.len(),
                    l.end_slot,
                    l.capacity_slots,
                    l.deleted_slots,
                    l.orphan_lfn_slots
                );
                for (i, g) in l.groups.iter().enumerate() {
                    let kind = match g.kind {
                        EntryKind::VolumeLabel => "label",
                        EntryKind::Dot | EntryKind::DotDot => "dot",
                        EntryKind::Dir => "dir",
                        EntryKind::File => "file",
                    };
                    println!("{:>4}  slot {:>4}  {:<5}  {:<12}  {}", i, g.first_slot, kind, g.short_name, g.name());
                }
            }
            Ok(true)
        }
        Cmd::Sort { device, dir, order, partition, unmatched_first, dry_run, force, backup_dir } => {
            let r = order.resolve()?;
            let dir = dir.or(r.dir).ok_or("give --dir")?;
            let job = LockJob {
                dir_path: dir,
                order: r.names,
                expected_sizes: r.sizes,
                options: ReorderOptions {
                    unmatched: if unmatched_first { UnmatchedPlacement::Before } else { UnmatchedPlacement::After },
                    dry_run,
                    force,
                },
                partition,
                eject: false,
            };
            // Test hook: simulate the stick being pulled after N writes.
            let fault = std::env::var("P2U_FAIL_AFTER_WRITES").ok().and_then(|v| v.parse().ok());
            let out = run_lock(FaultInjector::new(open_rw(&device)?, fault), &job, Some(&backup_dir));
            print_json(&out);
            Ok(out.ok)
        }
        Cmd::Verify { device, dir, order, partition } => {
            let r = order.resolve()?;
            let dir = dir.or(r.dir).ok_or("give --dir")?;
            let mut vol = Volume::open(open_rw(&device)?, partition).map_err(|e| e.to_string())?;
            let d = vol.find_dir(&dir).map_err(|e| e.to_string())?;
            let (check, sizes) = verify_on_medium(&mut vol, d, &r.names, r.sizes.as_deref()).map_err(|e| e.to_string())?;
            let ok = check.in_order && sizes.is_empty();
            print_json(&serde_json::json!({ "ok": ok, "check": check, "size_mismatches": sizes }));
            Ok(ok)
        }
        Cmd::OsListing { path, order } => {
            let r = order.resolve()?;
            let c = check_order(&readdir_order(&path).map_err(|e| e.to_string())?, &r.names);
            eprintln!("Diagnostics only: this is the OS's listing, not a guarantee of the order players see.");
            print_json(&c);
            Ok(c.in_order)
        }
        Cmd::Restore { device, mut backups } => {
            // File names carry a timestamp and phase tag, so name order is creation order.
            backups.sort();
            backups.reverse();
            let mut vol = Volume::open(open_rw(&device)?, None).map_err(|e| e.to_string())?;
            for path in &backups {
                let b = Backup::load(path).map_err(|e| e.to_string())?;
                vol.restore(&b).map_err(|e| e.to_string())?;
                println!("Restored from {}", path.display());
            }
            Ok(true)
        }
        Cmd::Write { target, playlist, naming, replace, allow_fragmented, no_verify_data, dry_run, force, partition, backup_dir, json } => {
            let plan = load_plan(&playlist, Path::new("/"), &naming)?;
            for m in &plan.missing {
                eprintln!("warning: missing, skipped: {m}");
            }
            let job = WriteJob {
                target_dir: plan.volume_path.clone(),
                items: plan.items.iter().map(|i| WriteItem { source: i.source.clone(), name: i.target_name.clone() }).collect(),
                options: WriteOptions { replace, allow_fragmented, force, dry_run },
                verify_data: !no_verify_data,
                partition,
                eject: true,
            };
            let mut prog = |p: &WriteProgress| {
                if p.bytes_done == 0 && !json {
                    eprintln!("[{} {}/{}] {}", p.stage, p.index + 1, p.total, p.name);
                }
            };
            let listed = p2u_device::list_volumes().ok().and_then(|v| v.into_iter().find(|v| v.id == target || v.device == target));
            let out = match listed {
                Some(vol) => match p2u_device::acquire(&vol) {
                    Ok(mut raw) => {
                        let o = run_write(&mut raw, &job, Some(&backup_dir), &mut prog);
                        let _ = if o.ok { raw.eject() } else { Ok(()) };
                        drop(raw);
                        if !o.ok {
                            let _ = p2u_device::remount_after(&vol);
                        }
                        o
                    }
                    Err(p2u_device::DeviceError::NeedsElevation) | Err(p2u_device::DeviceError::NoBroker(_)) => {
                        p2u_device::write_via_helper(&vol, &job).map_err(|e| e.to_string())?
                    }
                    Err(e) => {
                        let _ = p2u_device::remount_after(&vol);
                        return Err(e.to_string());
                    }
                },
                None => {
                    let f = open_rw(Path::new(&target))?;
                    // Test hook: simulate the stick being pulled after N writes.
                    let fault = std::env::var("P2U_FAIL_AFTER_WRITES").ok().and_then(|v| v.parse().ok());
                    run_write(FaultInjector::new(f, fault), &job, Some(&backup_dir), &mut prog)
                }
            };
            if json {
                print_json(&out);
            } else {
                summarize_write(&out);
            }
            Ok(out.ok)
        }
        Cmd::Lock { volume, dir, order, backup_dir, remount } => {
            let r = order.resolve()?;
            let dir = dir.or(r.dir).ok_or("give --dir")?;
            let vol = find_volume(&volume)?;
            let job = LockJob {
                dir_path: dir,
                order: r.names,
                expected_sizes: r.sizes,
                options: ReorderOptions::default(),
                partition: None,
                eject: !remount,
            };
            let out = lock_volume(&vol, job, &backup_dir, remount)?;
            print_json(&out);
            summarize(&out);
            Ok(out.ok)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
