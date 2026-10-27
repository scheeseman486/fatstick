//! p2u: command-line front end for Playlist2USB.
//!
//! Typical session:
//!   p2u volumes                                   # find the stick
//!   p2u copy trip.xspf --dest /media/me/STICK     # ordered copy, no elevation
//!   p2u lock /org/freedesktop/UDisks2/block_devices/sdb1 --playlist trip.xspf
//!                                                 # raw reorder + verify (OS prompt)
//! Image files work everywhere a device is accepted, for testing:
//!   p2u ls stick.img --dir "/Road Trip"

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use p2u_core::copy::{execute_copy, readdir_order, CopyOptions};
use p2u_core::fat::{check_order, Backup, EntryKind, ReorderOptions, UnmatchedPlacement, Volume};
use p2u_core::job::{run_lock, LockJob};
use p2u_core::plan::{make_plan, Plan, PlanOptions};

#[derive(Parser)]
#[command(name = "p2u", version, about = "Copy playlists to FAT32 USB sticks in the order cheap players actually use")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct NamingArgs {
    /// Folder name to create (default: playlist name)
    #[arg(long)]
    folder: Option<String>,
    /// Do not prefix file names with track numbers
    #[arg(long)]
    no_number: bool,
    /// Transliterate names to ASCII
    #[arg(long)]
    ascii: bool,
}

impl NamingArgs {
    fn opts(&self) -> PlanOptions {
        PlanOptions { folder_name: self.folder.clone(), number_prefix: !self.no_number, ascii_names: self.ascii }
    }
}

#[derive(Args)]
struct OrderSource {
    /// Text file with one entry name per line
    #[arg(long, conflicts_with = "playlist")]
    order_file: Option<PathBuf>,
    /// Derive names from a playlist (same naming options as `copy`)
    #[arg(long)]
    playlist: Option<PathBuf>,
    #[command(flatten)]
    naming: NamingArgs,
}

impl OrderSource {
    /// Returns (names, folder path inside the volume from the plan).
    fn resolve(&self) -> Result<(Vec<String>, Option<String>), String> {
        if let Some(f) = &self.order_file {
            return p2u_core::read_order_file(f).map(|n| (n, None)).map_err(|e| e.to_string());
        }
        if let Some(pl) = &self.playlist {
            let p = load_plan(pl, Path::new("/"), &self.naming)?;
            return Ok((p.target_names(), Some(p.volume_path)));
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
    /// Copy a playlist into a fresh folder, one file at a time (no elevation)
    Copy {
        playlist: PathBuf,
        #[arg(long)]
        dest: PathBuf,
        #[command(flatten)]
        naming: NamingArgs,
        /// Replace the folder if it exists
        #[arg(long)]
        replace: bool,
        /// Space modification times 2 s apart in playlist order
        #[arg(long)]
        stamp_times: bool,
        /// Also write an .m3u into the folder (last entry)
        #[arg(long)]
        write_m3u: bool,
        /// fsync after every file
        #[arg(long)]
        sync: bool,
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
    /// Check a directory's raw order against an order list
    Verify {
        device: PathBuf,
        #[arg(long)]
        dir: Option<String>,
        #[command(flatten)]
        order: OrderSource,
        #[arg(long)]
        partition: Option<usize>,
    },
    /// Best-effort check of a mounted folder's order via the OS (no elevation)
    VerifyMounted {
        /// The copied folder on the mounted volume
        path: PathBuf,
        #[command(flatten)]
        order: OrderSource,
    },
    /// Undo a sort using its backup file
    Restore {
        device: PathBuf,
        backup: PathBuf,
    },
    /// Unmount a removable volume, reorder via the OS privilege broker, verify, remount
    Lock {
        /// Volume id from `p2u volumes`
        volume: String,
        #[arg(long)]
        dir: Option<String>,
        #[command(flatten)]
        order: OrderSource,
        #[arg(long, default_value = ".")]
        backup_dir: PathBuf,
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
        Cmd::Copy { playlist, dest, naming, replace, stamp_times, write_m3u, sync } => {
            let plan = load_plan(&playlist, &dest, &naming)?;
            for m in &plan.missing {
                eprintln!("warning: missing, skipped: {m}");
            }
            let opts = CopyOptions { replace_existing: replace, stamp_times, write_m3u, sync_each_file: sync };
            let report = execute_copy(&plan, &opts, None, |p| {
                if p.bytes_done == 0 {
                    eprintln!("[{}/{}] {}", p.index + 1, p.total, p.name);
                }
                true
            })
            .map_err(|e| e.to_string())?;
            println!("Copied {} files ({}).", report.files_copied, human(report.bytes_copied));
            if !report.junk_removed.is_empty() {
                println!("Removed {} macOS/Windows metadata files.", report.junk_removed.len());
            }
            println!(
                "Order as reported by the OS: {}",
                if report.check.in_order { "matches the playlist" } else { "DOES NOT match; run `p2u lock`" }
            );
            println!("Folder inside the volume: {}", plan.volume_path);
            Ok(true)
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
            let (names, plan_dir) = order.resolve()?;
            let dir = dir.or(plan_dir).ok_or("give --dir")?;
            let job = LockJob {
                dir_path: dir,
                order: names,
                options: ReorderOptions {
                    unmatched: if unmatched_first { UnmatchedPlacement::Before } else { UnmatchedPlacement::After },
                    dry_run,
                    force,
                },
                partition,
            };
            let out = run_lock(open_rw(&device)?, &job, Some(&backup_dir));
            print_json(&out);
            Ok(out.ok)
        }
        Cmd::Verify { device, dir, order, partition } => {
            let (names, plan_dir) = order.resolve()?;
            let dir = dir.or(plan_dir).ok_or("give --dir")?;
            let mut vol = Volume::open(open_rw(&device)?, partition).map_err(|e| e.to_string())?;
            let d = vol.find_dir(&dir).map_err(|e| e.to_string())?;
            let actual = vol.raw_order(d).map_err(|e| e.to_string())?;
            let c = check_order(&actual, &names);
            print_json(&c);
            Ok(c.in_order)
        }
        Cmd::VerifyMounted { path, order } => {
            let (names, _) = order.resolve()?;
            let c = check_order(&readdir_order(&path).map_err(|e| e.to_string())?, &names);
            print_json(&c);
            Ok(c.in_order)
        }
        Cmd::Restore { device, backup } => {
            let b = Backup::load(&backup).map_err(|e| e.to_string())?;
            let mut vol = Volume::open(open_rw(&device)?, None).map_err(|e| e.to_string())?;
            vol.restore(&b).map_err(|e| e.to_string())?;
            println!("Restored {} from {}", b.dir_path, backup.display());
            Ok(true)
        }
        Cmd::Lock { volume, dir, order, backup_dir } => {
            let (names, plan_dir) = order.resolve()?;
            let dir = dir.or(plan_dir).ok_or("give --dir")?;
            let vols = p2u_device::list_volumes().map_err(|e| e.to_string())?;
            let vol = vols
                .into_iter()
                .find(|v| v.id == volume || v.device == volume)
                .ok_or_else(|| format!("no removable FAT volume {volume:?}; see `p2u volumes`"))?;
            let job = LockJob { dir_path: dir, order: names, options: ReorderOptions::default(), partition: None };
            match p2u_device::acquire(&vol) {
                Ok(raw) => {
                    let out = run_lock(raw, &job, Some(&backup_dir));
                    // `run_lock` consumed the handle, so it is closed here.
                    let remounted = p2u_device::remount_after(&vol).map_err(|e| e.to_string())?;
                    print_json(&out);
                    if let Some(m) = remounted {
                        eprintln!("Mounted again at {}", m.display());
                    }
                    Ok(out.ok)
                }
                Err(p2u_device::DeviceError::NeedsElevation) | Err(p2u_device::DeviceError::NoBroker(_)) => {
                    let out = p2u_device::lock_via_helper(&vol, &job).map_err(|e| e.to_string())?;
                    print_json(&out);
                    Ok(out.ok)
                }
                Err(e) => Err(e.to_string()),
            }
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
