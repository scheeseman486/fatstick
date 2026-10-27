//! p2u-helper: the only part of Playlist2USB that ever runs with administrator
//! or root rights, and only on Windows (UAC) or on Linux without udisks2 (pkexec).
//!
//! Usage: p2u-helper <device> <job.json> <result.json>
//!
//! * `<device>`: `\\.\E:` on Windows, `/dev/sdX[N]` on Linux. Must be a
//!   removable FAT volume; the helper unmounts (Linux) or locks (Windows) it.
//! * `<job.json>`: a serialized `p2u_core::job::HelperJob` (reorder or raw write).
//! * `<result.json>`: created exclusively (fails if it exists); receives a
//!   `HelperOutcome`. Undo backups are written next to it.
//!
//! A lock job only permutes the entries of one folder. A write job writes the
//! playlist's files into free clusters, links them in every FAT copy and adds
//! one folder entry, in the crash-safe order documented in p2u_core::fat::write.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use p2u_core::job::{create_exclusive, run_lock, run_write, HelperJob, HelperOutcome, LockOutcome};

const MAX_JOB_BYTES: u64 = 4 << 20;

fn fail(msg: String) -> HelperOutcome {
    HelperOutcome::Lock(LockOutcome { ok: false, error: Some(msg), report: None, check: None, size_mismatches: vec![], backup_file: None })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: p2u-helper <device> <job.json> <result.json>");
        return ExitCode::from(2);
    }
    let (device, job_path, result_path) = (&args[0], Path::new(&args[1]), Path::new(&args[2]));

    // Open the result file first so nothing runs if we cannot report.
    let mut result_file = match create_exclusive(result_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot create result file: {e}");
            return ExitCode::from(2);
        }
    };

    let outcome = (|| -> HelperOutcome {
        match std::fs::metadata(job_path) {
            Ok(m) if m.is_file() && m.len() <= MAX_JOB_BYTES => {}
            _ => return fail("job file missing or too large".into()),
        }
        let job: HelperJob = match std::fs::read(job_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(j) => j,
            None => return fail("job file is not a valid job".into()),
        };
        let mut raw = match p2u_device::open_direct(device) {
            Ok(r) => r,
            Err(e) => return fail(e.to_string()),
        };
        // Keep the OS from remounting (and writing to) the verified volume.
        match job {
            HelperJob::Lock(job) => {
                let mut outcome = run_lock(&mut raw, &job, result_path.parent());
                if outcome.ok && job.eject {
                    if let Err(e) = raw.eject() {
                        outcome.error = Some(format!("order verified; {e}"));
                    }
                }
                HelperOutcome::Lock(outcome)
            }
            HelperJob::Write(job) => {
                let mut outcome = run_write(&mut raw, &job, result_path.parent(), &mut |_| {});
                if outcome.ok && job.eject {
                    if let Err(e) = raw.eject() {
                        outcome.error = Some(format!("written and verified; {e}"));
                    }
                }
                HelperOutcome::Write(outcome)
            }
        }
    })();

    let ok = match &outcome {
        HelperOutcome::Lock(o) => o.ok,
        HelperOutcome::Write(o) => o.ok,
    };
    let _ = serde_json::to_writer_pretty(&mut result_file, &outcome);
    let _ = result_file.flush();
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
