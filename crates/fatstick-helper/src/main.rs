//! fatstick-helper: the only part of Fatstick that ever runs with administrator
//! or root rights, and only on Windows (UAC) or on Linux without udisks2 (pkexec).
//!
//! Usage: fatstick-helper <device> <job.json> <result.json>
//!
//! * `<device>`: `\\.\PhysicalDriveN` on Windows, `/dev/sdX` on Linux: a whole
//!   removable USB/SD disk. The helper checks that, refuses the disk the system
//!   runs from, and unmounts (Linux) or locks and dismounts (Windows) every
//!   volume on it.
//! * `<job.json>`: a serialized `fatstick_core::job::StickJob`.
//! * `<result.json>`: created exclusively (fails if it exists); receives a
//!   `StickOutcome`.
//!
//! The job erases the stick and writes a fresh FAT32 file system holding the
//! playlists, then reads it all back (see fatstick_core::fat::stick).

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use fatstick_core::job::{create_exclusive, run_stick, StickJob, StickOutcome};

const MAX_JOB_BYTES: u64 = 4 << 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: fatstick-helper <device> <job.json> <result.json>");
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

    let outcome = (|| -> StickOutcome {
        match std::fs::metadata(job_path) {
            Ok(m) if m.is_file() && m.len() <= MAX_JOB_BYTES => {}
            _ => return StickOutcome::fail("job file missing or too large"),
        }
        let job: StickJob = match std::fs::read(job_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(j) => j,
            None => return StickOutcome::fail("job file is not a valid job"),
        };
        let mut raw = match fatstick_device::open_direct(device) {
            Ok(r) => r,
            Err(e) => return StickOutcome::fail(format!("nothing was written: {e}")),
        };
        let mut outcome = run_stick(&mut raw, &job, &mut |_| {}, None);
        // Keep the OS from mounting (and writing to) the verified stick.
        if outcome.ok && job.eject {
            if let Err(e) = raw.finish() {
                outcome.error = Some(e.to_string());
            }
        }
        outcome
    })();

    let _ = serde_json::to_writer_pretty(&mut result_file, &outcome);
    let _ = result_file.flush();
    if outcome.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
