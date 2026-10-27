//! macOS: DiskArbitration (via `diskutil`) to unmount, and `authopen` to get a
//! raw read/write descriptor for `/dev/rdiskNsM` without running as root.
//!
//! `authopen` asks authd for the right `sys.openfile.readwrite.<path>` (admin
//! group, cached 300 s) and passes the opened descriptor back over a Unix
//! socket (SCM_RIGHTS). Raspberry Pi Imager uses the same mechanism.
//! Without `-extauth`, authopen itself (an Apple-signed binary) requests the
//! right, which is the only case where Touch ID could be offered; whether it
//! is needs testing on a Touch ID Mac (see the research doc).
//!
//! macOS also requires the app to have "Removable Volumes" access (TCC); the
//! first raw open triggers that consent prompt.

use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{DeviceError, RawVolume, Result, VolumeInfo};

fn diskutil(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("/usr/sbin/diskutil").args(args).output()?;
    if !out.status.success() {
        return Err(DeviceError::Other(format!(
            "diskutil {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

fn plist_of(bytes: &[u8]) -> Result<plist::Dictionary> {
    plist::Value::from_reader(io::Cursor::new(bytes))
        .map_err(|e| DeviceError::Other(format!("plist: {e}")))?
        .into_dictionary()
        .ok_or_else(|| DeviceError::Other("plist is not a dictionary".into()))
}

fn s(d: &plist::Dictionary, k: &str) -> String {
    d.get(k).and_then(|v| v.as_string()).unwrap_or_default().to_string()
}

pub fn list_volumes() -> Result<Vec<VolumeInfo>> {
    let top = plist_of(&diskutil(&["list", "-plist", "external", "physical"])?)?;
    let mut ids = Vec::new();
    if let Some(disks) = top.get("AllDisksAndPartitions").and_then(|v| v.as_array()) {
        for d in disks.iter().filter_map(|v| v.as_dictionary()) {
            match d.get("Partitions").and_then(|v| v.as_array()) {
                Some(parts) if !parts.is_empty() => {
                    for p in parts.iter().filter_map(|v| v.as_dictionary()) {
                        ids.push(s(p, "DeviceIdentifier"));
                    }
                }
                // A "superfloppy" stick: the file system is on the whole disk.
                _ => ids.push(s(d, "DeviceIdentifier")),
            }
        }
    }
    let mut out = Vec::new();
    for id in ids.into_iter().filter(|i| !i.is_empty()) {
        let Ok(info) = diskutil(&["info", "-plist", &id]).and_then(|b| plist_of(&b)) else { continue };
        if s(&info, "FilesystemType") != "msdos" {
            continue;
        }
        let mount = s(&info, "MountPoint");
        out.push(VolumeInfo {
            id: id.clone(),
            device: format!("/dev/{id}"),
            label: s(&info, "VolumeName"),
            fs_type: s(&info, "FilesystemName"),
            mount_point: if mount.is_empty() { None } else { Some(PathBuf::from(mount)) },
            size_bytes: info.get("TotalSize").or_else(|| info.get("Size")).and_then(|v| v.as_unsigned_integer()).unwrap_or(0),
            removable: info.get("Ejectable").and_then(|v| v.as_boolean()).unwrap_or(false)
                || info.get("RemovableMedia").and_then(|v| v.as_boolean()).unwrap_or(false),
            bus: s(&info, "BusProtocol").to_lowercase(),
        });
    }
    Ok(out)
}

fn raw_node(device: &str) -> String {
    device.replacen("/dev/disk", "/dev/rdisk", 1)
}

/// Run authopen and receive the descriptor it opened.
fn authopen(path: &str) -> Result<File> {
    let (ours, theirs) = UnixStream::pair()?;
    let mut child = Command::new("/usr/libexec/authopen")
        .arg("-stdoutpipe")
        .arg("-o")
        .arg(libc::O_RDWR.to_string())
        .arg(path)
        .stdout(Stdio::from(OwnedFd::from(theirs)))
        .stderr(Stdio::piped())
        .spawn()?;

    let mut data = [0u8; 1];
    let mut iov = libc::iovec { iov_base: data.as_mut_ptr().cast(), iov_len: data.len() };
    // SAFETY: CMSG_SPACE is a pure size computation.
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
    let mut control = vec![0u8; space];
    // SAFETY: msghdr is plain data; zero is a valid initial state.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    // SAFETY: msg points at buffers that outlive the call.
    let n = unsafe { libc::recvmsg(ours.as_raw_fd(), &mut msg, 0) };
    let status = child.wait()?;
    if n <= 0 || !status.success() {
        return Err(DeviceError::NotAuthorized);
    }
    // SAFETY: walking the control buffer recvmsg filled in.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if !cmsg.is_null() && (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
            let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const libc::c_int);
            return Ok(File::from_raw_fd(fd));
        }
    }
    Err(DeviceError::Other("authopen returned no descriptor".into()))
}

pub fn acquire(vol: &VolumeInfo) -> Result<RawVolume> {
    if vol.mount_point.is_some() {
        diskutil(&["unmount", &vol.device])?;
    }
    Ok(RawVolume::from_file(authopen(&raw_node(&vol.device))?))
}

pub fn remount(vol: &VolumeInfo) -> Result<Option<PathBuf>> {
    diskutil(&["mount", &vol.device])?;
    let info = plist_of(&diskutil(&["info", "-plist", &vol.device])?)?;
    let mp = s(&info, "MountPoint");
    Ok(if mp.is_empty() { None } else { Some(PathBuf::from(mp)) })
}

pub fn open_direct(device: &str) -> Result<RawVolume> {
    let info = plist_of(&diskutil(&["info", "-plist", device])?)?;
    if info.get("Internal").and_then(|v| v.as_boolean()).unwrap_or(true) {
        return Err(DeviceError::UnsafeTarget(device.into(), "internal disk".into()));
    }
    if !s(&info, "MountPoint").is_empty() {
        return Err(DeviceError::UnsafeTarget(device.into(), "still mounted".into()));
    }
    let f = std::fs::OpenOptions::new().read(true).write(true).open(raw_node(device))?;
    Ok(RawVolume::from_file(f))
}

pub fn run_helper_elevated(_helper: &Path, _args: &[String]) -> Result<i32> {
    Err(DeviceError::Other("macOS uses authopen; no elevated helper".into()))
}

pub fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}
