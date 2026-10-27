//! Finding removable FAT volumes and getting raw read/write access to one
//! through each OS's own privilege broker.
//!
//! | OS      | Unmount                         | Raw access                                    | Prompt (biometrics)                  |
//! |---------|---------------------------------|-----------------------------------------------|--------------------------------------|
//! | Linux   | udisks2 `Filesystem.Unmount`    | udisks2 `Block.OpenDevice` hands back an fd   | polkit agent (fingerprint via PAM)   |
//! | macOS   | `diskutil unmount`              | `authopen -stdoutpipe` hands back an fd       | authd dialog                          |
//! | Windows | `FSCTL_LOCK_VOLUME`/`DISMOUNT`  | elevated helper opens `\\.\E:`                | UAC (Windows Hello when configured)  |
//!
//! On Linux and macOS the GUI process stays unprivileged: the broker gives it
//! a file descriptor for exactly one device. On Windows a raw volume handle
//! needs an elevated process, so the GUI runs `p2u-helper` via UAC.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

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
    #[error("authorization was cancelled or denied")]
    NotAuthorized,
    #[error("this step needs administrator rights; run it through the elevated helper")]
    NeedsElevation,
    #[error("refusing to touch {0}: {1}")]
    UnsafeTarget(String, String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DeviceError>;

/// A FAT volume on removable media.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct VolumeInfo {
    /// Stable handle used by the UI: udisks object path, BSD name or drive letter.
    pub id: String,
    /// Device node: /dev/sdb1, /dev/disk4s1, \\.\E:
    pub device: String,
    pub label: String,
    /// "vfat", "msdos", "FAT32", ...
    pub fs_type: String,
    pub mount_point: Option<PathBuf>,
    pub size_bytes: u64,
    pub removable: bool,
    /// "usb", "sdio", ... when the OS tells us.
    pub bus: String,
}

/// An open raw handle to the volume. Reads and writes must be sector aligned.
pub struct RawVolume {
    file: File,
    #[cfg(windows)]
    locked: bool,
}

impl RawVolume {
    #[allow(dead_code)]
    pub(crate) fn from_file(file: File) -> Self {
        RawVolume {
            file,
            #[cfg(windows)]
            locked: false,
        }
    }
}

impl Read for RawVolume {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}
impl Write for RawVolume {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for RawVolume {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl p2u_core::fat::Device for RawVolume {
    fn sync_device(&mut self) -> io::Result<()> {
        // macOS raw (/dev/rdisk*) nodes are uncached and reject F_FULLFSYNC;
        // Linux block devices and Windows volume handles need the flush.
        if cfg!(target_os = "macos") {
            Ok(())
        } else {
            self.file.sync_all()
        }
    }
}

impl Drop for RawVolume {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.locked {
            windows::unlock(&self.file);
        }
    }
}

/// Removable FAT volumes currently attached.
pub fn list_volumes() -> Result<Vec<VolumeInfo>> {
    imp::list_volumes()
}

/// Unmount the volume and obtain raw read/write access through the OS broker.
/// Shows the system authentication dialog when needed.
pub fn acquire(vol: &VolumeInfo) -> Result<RawVolume> {
    imp::acquire(vol)
}

/// Close raw access and mount the volume again. Returns the new mount point if known.
pub fn release(vol: &VolumeInfo, raw: RawVolume) -> Result<Option<PathBuf>> {
    drop(raw);
    imp::remount(vol)
}

/// Open a device directly. Only for a process that is already root/admin
/// (the elevated helper); checks the target is a removable, unmounted volume.
pub fn open_direct(device: &str) -> Result<RawVolume> {
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

/// Mount the volume again after its [`RawVolume`] has been dropped
/// (for example after [`p2u_core::job::run_lock`] consumed it).
pub fn remount_after(vol: &VolumeInfo) -> Result<Option<PathBuf>> {
    imp::remount(vol)
}

/// `p2u-helper` next to the running executable.
pub fn helper_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) { "p2u-helper.exe" } else { "p2u-helper" };
    let p = exe.parent().map(|d| d.join(name)).filter(|p| p.exists());
    p.ok_or_else(|| DeviceError::Other(format!("{name} not found next to {}", exe.display())))
}

/// Run a lock job in the elevated helper and collect its outcome.
/// The job and result files live in a fresh private temp folder.
pub fn lock_via_helper(vol: &VolumeInfo, job: &p2u_core::job::LockJob) -> Result<p2u_core::job::LockOutcome> {
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
    let args = vec![
        vol.device.clone(),
        job_path.display().to_string(),
        result_path.display().to_string(),
    ];
    let code = run_helper_elevated(&helper_path()?, &args)?;
    let bytes = std::fs::read(&result_path)
        .map_err(|_| DeviceError::Other(format!("helper exited with code {code} and wrote no result")))?;
    serde_json::from_slice(&bytes).map_err(|e| DeviceError::Other(format!("helper result: {e}")))
}
