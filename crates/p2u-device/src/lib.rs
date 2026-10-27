//! Finding USB sticks and getting raw read/write access to a whole one
//! through each OS's own privilege broker.
//!
//! Writing erases the whole stick, so this works on whole disks, not volumes:
//! every partition on the stick is unmounted first, and the disk is then
//! opened exclusively.
//!
//! | OS      | Unmount                                   | Raw access to the whole disk                    | Prompt (biometrics)                 |
//! |---------|-------------------------------------------|-------------------------------------------------|-------------------------------------|
//! | Linux   | udisks2 `Filesystem.Unmount` per partition| udisks2 `Block.OpenDevice` hands back an fd     | polkit agent (fingerprint via PAM)  |
//! | macOS   | `diskutil unmountDisk`                    | `authopen -stdoutpipe` hands back an fd         | authd dialog                        |
//! | Windows | lock + dismount every volume on the disk  | elevated helper opens `\\.\PhysicalDriveN`      | UAC (Windows Hello when configured) |
//!
//! Only removable USB/SD disks are listed or accepted, never the disk the
//! system runs from.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(windows)]
use windows as imp;

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("the system's disk service is not available: {0}")]
    NoBroker(String),
    #[error("authorization was cancelled or denied; nothing was written")]
    NotAuthorized,
    #[error("this step needs administrator rights; run it through the elevated helper")]
    NeedsElevation,
    #[error("refusing to touch {0}: {1}")]
    UnsafeTarget(String, String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DeviceError>;

/// Something currently on a stick (shown before it is erased).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StickVolume {
    pub label: String,
    /// "vfat", "exfat", "ntfs", ... as the OS reports it; empty if unknown.
    pub fs_type: String,
    pub mount_point: Option<PathBuf>,
    pub size_bytes: u64,
}

/// A removable USB or SD disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StickInfo {
    /// Stable handle used by the UI: udisks object path, BSD name or PhysicalDriveN.
    pub id: String,
    /// Whole-disk node: /dev/sdb, /dev/disk4, \\.\PhysicalDrive3
    pub device: String,
    /// Vendor and model, e.g. "SanDisk Cruzer Blade".
    pub name: String,
    pub size_bytes: u64,
    /// "usb", "sdio", ... when the OS tells us.
    pub bus: String,
    pub volumes: Vec<StickVolume>,
}

/// An open raw handle to a whole stick. Reads and writes must be sector aligned.
pub struct RawDisk {
    file: File,
    size: u64,
    sector: u32,
    /// What to do after the job (power off, eject).
    #[allow(dead_code)]
    finish: Finish,
    /// Windows: locked volume handles, held until the disk is closed.
    #[cfg(windows)]
    held_volumes: Vec<File>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct Finish {
    /// udisks object path of the drive (Linux), BSD name (macOS).
    pub(crate) handle: String,
}

impl RawDisk {
    #[allow(dead_code)]
    pub(crate) fn new(file: File, size: u64, sector: u32, finish: Finish) -> Self {
        RawDisk {
            file,
            size,
            sector,
            finish,
            #[cfg(windows)]
            held_volumes: Vec::new(),
        }
    }

    pub fn size_bytes(&self) -> u64 {
        self.size
    }

    /// Hand the stick back so it can be unplugged without any OS writing to it
    /// first: power it off (Linux), eject it (macOS, Windows). Consumes the
    /// handle; the stick disappears from the system if this succeeds.
    pub fn finish(self) -> Result<()> {
        imp::finish(self)
    }
}

impl Read for RawDisk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}
impl Write for RawDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for RawDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl p2u_core::fat::Device for RawDisk {
    fn sync_device(&mut self) -> io::Result<()> {
        // macOS raw (/dev/rdisk*) nodes are uncached and reject F_FULLFSYNC;
        // Linux block devices and Windows disk handles need the flush.
        if cfg!(target_os = "macos") {
            Ok(())
        } else {
            self.file.sync_all()
        }
    }

    fn invalidate_cache(&mut self) -> io::Result<()> {
        // Linux reads a block device through the page cache; evict it so the
        // verification read comes from the stick. No-op elsewhere.
        p2u_core::fat::drop_page_cache(&self.file)
    }

    fn disk_size(&mut self) -> io::Result<u64> {
        Ok(self.size)
    }

    fn logical_sector_size(&mut self) -> io::Result<u32> {
        Ok(self.sector)
    }
}

/// Removable USB/SD disks currently attached, with what is on them.
pub fn list_sticks() -> Result<Vec<StickInfo>> {
    imp::list_sticks()
}

/// Unmount everything on the stick and obtain raw read/write access to the
/// whole disk through the OS broker. Shows the system authentication dialog.
pub fn acquire(stick: &StickInfo) -> Result<RawDisk> {
    imp::acquire(stick)
}

/// Open a whole disk directly. Only for a process that is already root/admin
/// (the elevated helper); checks the target is a removable USB/SD disk that
/// does not hold the running system, and unmounts it.
pub fn open_direct(device: &str) -> Result<RawDisk> {
    imp::open_direct(device)
}

/// Run the privileged helper with the OS elevation prompt and wait for it.
/// Windows: ShellExecuteEx "runas" (UAC). Linux: pkexec (needs the polkit
/// policy from packaging/linux installed). macOS: not used.
pub fn run_helper_elevated(helper: &Path, args: &[String]) -> Result<i32> {
    imp::run_helper_elevated(helper, args)
}

/// Whether the current process already has administrator/root rights.
pub fn is_elevated() -> bool {
    imp::is_elevated()
}

/// `p2u-helper` next to the running executable.
pub fn helper_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) { "p2u-helper.exe" } else { "p2u-helper" };
    let p = exe.parent().map(|d| d.join(name)).filter(|p| p.exists());
    p.ok_or_else(|| DeviceError::Other(format!("{name} not found next to {}", exe.display())))
}

/// Run a stick job in the elevated helper and collect its outcome.
/// The job and result files live in a fresh private temp folder.
pub fn write_via_helper(stick: &StickInfo, job: &p2u_core::job::StickJob) -> Result<p2u_core::job::StickOutcome> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("p2u-{}-{nanos}", std::process::id()));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(&dir)?;
    let job_path = dir.join("job.json");
    let result_path = dir.join("result.json");
    std::fs::write(&job_path, serde_json::to_vec(job).map_err(|e| DeviceError::Other(e.to_string()))?)?;
    let args = vec![stick.device.clone(), job_path.display().to_string(), result_path.display().to_string()];
    let code = run_helper_elevated(&helper_path()?, &args)?;
    let bytes = std::fs::read(&result_path)
        .map_err(|_| DeviceError::Other(format!("helper exited with code {code} and wrote no result")))?;
    let _ = std::fs::remove_dir_all(&dir);
    serde_json::from_slice(&bytes).map_err(|e| DeviceError::Other(format!("helper result: {e}")))
}

/// Human-readable size, e.g. "15.6 GB".
pub fn human_size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

/// Mount points of file systems on a device node or any of its partitions
/// (Linux; empty elsewhere). The CLI refuses to write to a mounted disk.
pub fn mounted_on(device: &Path) -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        linux::mounted_on(device)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        Vec::new()
    }
}
