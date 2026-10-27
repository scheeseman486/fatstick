//! p2u-helper: the only part of Playlist2USB that ever runs with administrator
//! or root rights, and only on Windows (UAC) or on Linux without udisks2 (pkexec).
//!
//! Usage: p2u-helper <device> <job.json> <result.json>
//!
//! * `<device>`: `\\.\E:` on Windows, `/dev/sdX[N]` on Linux. Must be a
//!   removable FAT volume; the helper unmounts (Linux) or locks (Windows) it.
//! * `<job.json>`: a serialized `p2u_core::job::LockJob`.
//! * `<result.json>`: created exclusively (fails if it exists); receives a
//!   `LockOutcome`. A directory backup is written next to it.
//!
//! The helper never touches the FAT itself or any file data: it only permutes
//! the entries of the one folder named in the job.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use p2u_core::job::{create_exclusive, run_lock, LockJob, LockOutcome};

const MAX_JOB_BYTES: u64 = 4 << 20;

fn fail(msg: String) -> LockOutcome {
    LockOutcome { ok: false, error: Some(msg), report: None, check: None, backup_file: None }
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

    let outcome = (|| -> LockOutcome {
        match std::fs::metadata(job_path) {
            Ok(m) if m.is_file() && m.len() <= MAX_JOB_BYTES => {}
            _ => return fail("job file missing or too large".into()),
        }
        let job: LockJob = match std::fs::read(job_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(j) => j,
            None => return fail("job file is not a valid job".into()),
        };
        let raw = match p2u_device::open_direct(device) {
            Ok(r) => r,
            Err(e) => return fail(e.to_string()),
        };
        run_lock(raw, &job, result_path.parent())
    })();

    let ok = outcome.ok;
    let _ = serde_json::to_writer_pretty(&mut result_file, &outcome);
    let _ = result_file.flush();
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
