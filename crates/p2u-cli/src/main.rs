//! p2u: command-line front end for Playlist2USB.
//!
//! Writing erases the whole stick: it gets one FAT32 partition, laid out here,
//! with the playlist's tracks in FAT order and in one unbroken run of clusters
//! each, back to back. Then everything is read back from the stick.
//!
//! Typical session:
//!   p2u sticks                                   # find the stick
//!   p2u write <stick> --playlist trip.xspf       # erase, write, verify (one OS prompt)
//! Disk images (and whole-disk device nodes you can open yourself) work too:
//!   truncate -s 1G stick.img && p2u write stick.img --playlist trip.xspf --yes
//!   p2u ls stick.img --dir "/Road Trip"
//! The app's sync (several folders, tagged and converted files) is a job:
//!   p2u write-job stick.img --job job.json --yes

use std::fs::OpenOptions;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use p2u_core::fat::stick::{plan_content, StickContent, StickOptions, WriteItem, WriteProgress};
use p2u_core::fat::{check_order, normalize_name, Device, EntryKind, FaultInjector, Volume};
use p2u_core::job::{run_stick, StickJob, StickOutcome};
use p2u_core::plan::{make_plan, Plan, PlanOptions};
use p2u_device::{human_size, StickInfo};

#[derive(Parser)]
#[command(name = "p2u", version, about = "Write playlists to USB sticks in the order cheap players actually use")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct NamingArgs {
    /// Folder for the tracks (default: playlist name)
    #[arg(long, conflicts_with = "top_level")]
    folder: Option<String>,
    /// Put the tracks at the top level of the stick instead of in a folder
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
    /// Derive names (and expected sizes) from a playlist, with the same naming options as `write`
    #[arg(long)]
    playlist: Option<PathBuf>,
    #[command(flatten)]
    naming: NamingArgs,
}

#[derive(Subcommand)]
enum Cmd {
    /// List USB sticks and what is on them
    Sticks,
    /// Show which files would be written, under which names
    Plan {
        playlist: PathBuf,
        #[command(flatten)]
        naming: NamingArgs,
        /// Also write the target names to this order file
        #[arg(long)]
        order_out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// ERASE a stick and write the playlist onto a fresh FAT32 file system, then
    /// read everything back: order, placement and every byte
    Write {
        /// A stick from `p2u sticks` (uses the OS prompt), or a disk image or
        /// whole-disk device node you can already open
        target: String,
        #[arg(long)]
        playlist: PathBuf,
        #[command(flatten)]
        naming: NamingArgs,
        /// Volume label (default: the folder name)
        #[arg(long)]
        label: Option<String>,
        /// Don't ask before erasing
        #[arg(long)]
        yes: bool,
        /// Show the layout only; write nothing
        #[arg(long)]
        dry_run: bool,
        /// Skip comparing every byte with the source after writing
        #[arg(long)]
        no_verify_data: bool,
        #[arg(long)]
        json: bool,
    },
    /// ERASE a stick and write a prepared job: several folders, files made of
    /// parts (what the app's sync hands over). Same checks as `write`.
    WriteJob {
        /// A stick from `p2u sticks`, or a disk image or whole-disk device node
        target: String,
        /// A StickJob as JSON
        #[arg(long)]
        job: PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show a stick's FAT32 geometry and health flags
    Inspect {
        device: PathBuf,
        /// Also compare the FAT copies
        #[arg(long)]
        compare_fats: bool,
    },
    /// List a folder in FAT order
    Ls {
        device: PathBuf,
        #[arg(long, default_value = "/")]
        dir: String,
        #[arg(long)]
        json: bool,
    },
    /// Check a folder's FAT order (and sizes, with --playlist) on a stick or image
    Verify {
        device: PathBuf,
        #[arg(long)]
        dir: Option<String>,
        #[command(flatten)]
        order: OrderSource,
    },
}

fn load_plan(playlist: &Path, naming: &NamingArgs) -> Result<Plan, String> {
    let pl = p2u_core::playlist::load(playlist).map_err(|e| e.to_string())?;
    for w in &pl.warnings {
        eprintln!("warning: {w}");
    }
    Ok(make_plan(&pl, &naming.opts()))
}

fn open_ro(path: &Path) -> Result<std::fs::File, String> {
    OpenOptions::new().read(true).open(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

fn describe(stick: &StickInfo) -> String {
    let mut s = format!("{} {} ({})", stick.device, if stick.name.is_empty() { "USB stick" } else { &stick.name }, human_size(stick.size_bytes));
    if stick.volumes.is_empty() {
        s.push_str(", nothing recognisable on it");
    }
    for v in &stick.volumes {
        let label = if v.label.is_empty() { "(no label)".to_string() } else { format!("{:?}", v.label) };
        let mp = v.mount_point.as_ref().map(|m| format!(", mounted at {}", m.display())).unwrap_or_default();
        s.push_str(&format!("\n    {label} {}{mp}", if v.fs_type.is_empty() { "unknown" } else { &v.fs_type }));
    }
    s
}

/// Ask for confirmation on the terminal; without one, require --yes.
fn confirm_erase(what: &str) -> Result<(), String> {
    if !std::io::stdin().is_terminal() {
        return Err("writing erases the whole stick; pass --yes to confirm".into());
    }
    eprintln!("This ERASES EVERYTHING on:\n  {what}");
    eprint!("Type \"erase\" to continue: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    if line.trim() == "erase" {
        Ok(())
    } else {
        Err("cancelled; nothing was written".into())
    }
}

fn summarize(out: &StickOutcome) {
    if let Some(e) = &out.error {
        eprintln!("{}: {e}", if out.ok { "Note" } else { "FAILED" });
    }
    let Some(r) = &out.result else { return };
    let p = &r.plan;
    let f = &p.format;
    let mb = p.total_bytes as f64 / 1e6;
    let last = p.files().filter(|x| x.clusters > 0).last().map(|x| x.first_cluster + x.clusters - 1).unwrap_or(0);
    println!(
        "Wrote {} tracks ({mb:.1} MB) to {} on a fresh FAT32 partition ({} clusters of {} bytes, label {:?}).",
        p.file_count(),
        p.folder_paths().join(", "),
        f.cluster_count,
        f.cluster_bytes(),
        f.label.trim_end()
    );
    println!("Placement: clusters {}-{last}, each track in one unbroken run, back to back in stick order.", p.first_data_cluster());
    if let Some(v) = &out.verification {
        let tick = |b: bool| if b { "ok" } else { "FAILED" };
        println!(
            "Read back from the stick: partition table {}, boot sectors {}, FATs {}, folder tables {}, FAT order {}, placement {}, data {}.",
            tick(v.partition_table_ok),
            tick(v.boot_sectors_ok),
            tick(v.fats_ok),
            tick(v.tables_ok),
            tick(v.order_ok),
            tick(v.layout_ok),
            if !v.data_checked {
                "not checked".to_string()
            } else if v.data_mismatches.is_empty() {
                "every byte matches".to_string()
            } else {
                format!("{} file(s) differ", v.data_mismatches.len())
            }
        );
        for p in &v.problems {
            println!("  problem: {p}");
        }
    }
}

/// Plan, confirm, then erase and write a job to a listed stick or an image.
fn run_job(target: &str, job: &StickJob, yes: bool, dry_run: bool, json: bool) -> Result<bool, String> {
    let mut prog = |p: &WriteProgress| {
        if p.bytes_done == 0 && !json {
            match p.stage.as_str() {
                "erase" => eprintln!("[erase] clearing the stick"),
                s => eprintln!("[{s} {}/{}] {}", p.index + 1, p.total, p.name),
            }
        }
    };
    let listed = p2u_device::list_sticks().ok().and_then(|v| v.into_iter().find(|s| s.id == target || s.device == target));
    let out = match listed {
        Some(stick) => {
            // Catch "doesn't fit" and bad names before the stick is touched.
            let st = plan_content(stick.size_bytes, 512, &job.content).map_err(|e| format!("nothing was written: {e}"))?;
            if dry_run {
                print_json(&st);
                return Ok(true);
            }
            if !yes {
                confirm_erase(&describe(&stick))?;
            }
            match p2u_device::acquire(&stick) {
                Ok(mut raw) => {
                    let mut o = run_stick(&mut raw, job, &mut prog, None);
                    if o.ok {
                        if let Err(e) = raw.finish() {
                            o.error = Some(e.to_string());
                        } else if !json {
                            eprintln!("The stick has been ejected: unplug it.");
                        }
                    }
                    o
                }
                Err(p2u_device::DeviceError::NeedsElevation) | Err(p2u_device::DeviceError::NoBroker(_)) => {
                    p2u_device::write_via_helper(&stick, job).map_err(|e| e.to_string())?
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        None => {
            let path = Path::new(target);
            let mounted = p2u_device::mounted_on(path);
            if !mounted.is_empty() {
                return Err(format!("{target} is mounted at {}; unmount it first", mounted[0].display()));
            }
            let mut f = OpenOptions::new().read(true).write(true).open(path).map_err(|e| format!("{target}: {e}"))?;
            let size = f.disk_size().map_err(|e| e.to_string())?;
            let st = plan_content(size, 512, &job.content).map_err(|e| format!("nothing was written: {e}"))?;
            if dry_run {
                print_json(&st);
                return Ok(true);
            }
            if !yes {
                confirm_erase(&format!("{target} ({})", human_size(size)))?;
            }
            // Test hook: simulate the stick being pulled after N writes.
            let fault = std::env::var("P2U_FAIL_AFTER_WRITES").ok().and_then(|v| v.parse().ok());
            run_stick(&mut FaultInjector::new(f, fault), job, &mut prog, None)
        }
    };
    if json {
        print_json(&out);
    } else {
        summarize(&out);
    }
    Ok(out.ok)
}

fn run(cli: Cli) -> Result<bool, String> {
    match cli.cmd {
        Cmd::Sticks => {
            let sticks = p2u_device::list_sticks().map_err(|e| e.to_string())?;
            if sticks.is_empty() {
                println!("No USB sticks found.");
            }
            for s in sticks {
                println!("{}\n  {}", s.id, describe(&s));
            }
            Ok(true)
        }
        Cmd::Plan { playlist, naming, order_out, json } => {
            let plan = load_plan(&playlist, &naming)?;
            if let Some(o) = order_out {
                p2u_core::write_order_file(&o, &plan.target_names()).map_err(|e| e.to_string())?;
            }
            if json {
                print_json(&plan);
            } else {
                println!("Folder: {} ({} files, {})", plan.volume_path, plan.items.len(), human_size(plan.total_bytes));
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
        Cmd::Write { target, playlist, naming, label, yes, dry_run, no_verify_data, json } => {
            let plan = load_plan(&playlist, &naming)?;
            for m in &plan.missing {
                eprintln!("warning: missing, skipped: {m}");
            }
            let items = plan.items.iter().map(|i| WriteItem::file(i.source.clone(), i.target_name.clone())).collect();
            let opts = StickOptions { folder: plan.folder_name.clone(), label: label.unwrap_or_default() };
            let job = StickJob { content: StickContent::single(items, &opts), verify_data: !no_verify_data, eject: true };
            run_job(&target, &job, yes, dry_run, json)
        }
        Cmd::WriteJob { target, job, yes, dry_run, json } => {
            let bytes = std::fs::read(&job).map_err(|e| format!("{}: {e}", job.display()))?;
            let job: StickJob = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", job.display()))?;
            run_job(&target, &job, yes, dry_run, json)
        }
        Cmd::Inspect { device, compare_fats } => {
            let mut vol = Volume::open(open_ro(&device)?).map_err(|e| e.to_string())?;
            let h = vol.health(compare_fats).map_err(|e| e.to_string())?;
            print_json(&serde_json::json!({ "geometry": vol.geometry(), "health": h }));
            Ok(true)
        }
        Cmd::Ls { device, dir, json } => {
            let mut vol = Volume::open(open_ro(&device)?).map_err(|e| e.to_string())?;
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
        Cmd::Verify { device, dir, order } => {
            let (names, sizes, plan_dir) = if let Some(f) = &order.order_file {
                (p2u_core::read_order_file(f).map_err(|e| e.to_string())?, None, None)
            } else if let Some(pl) = &order.playlist {
                let p = load_plan(pl, &order.naming)?;
                (p.target_names(), Some(p.expected_sizes()), Some(p.volume_path))
            } else {
                return Err("give --order-file or --playlist".into());
            };
            let dir = dir.or(plan_dir).ok_or("give --dir")?;
            let mut vol = Volume::open(open_ro(&device)?).map_err(|e| e.to_string())?;
            let d = vol.find_dir(&dir).map_err(|e| e.to_string())?;
            let listing = vol.list_dir(d).map_err(|e| e.to_string())?;
            let files: Vec<_> = listing.groups.iter().filter(|g| g.kind == EntryKind::File).collect();
            let actual: Vec<String> = files.iter().map(|g| g.name().to_string()).collect();
            let check = check_order(&actual, &names);
            let mut wrong_sizes = Vec::new();
            if let Some(sizes) = sizes {
                for (n, s) in names.iter().zip(sizes) {
                    if let Some(g) = files.iter().find(|g| normalize_name(g.name()) == normalize_name(n)) {
                        if g.size as u64 != s {
                            wrong_sizes.push(serde_json::json!({ "name": n, "expected": s, "on_stick": g.size }));
                        }
                    }
                }
            }
            let ok = check.in_order && check.extra.is_empty() && wrong_sizes.is_empty();
            print_json(&serde_json::json!({ "ok": ok, "check": check, "size_mismatches": wrong_sizes }));
            Ok(ok)
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
