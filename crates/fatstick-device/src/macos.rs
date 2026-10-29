//! macOS: DiskArbitration (via `diskutil`) to unmount the whole disk, and
//! `authopen` to get a raw read/write descriptor for `/dev/rdiskN` without
//! running as root.
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
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{DeviceError, Finish, RawDisk, Result, StickInfo, StickVolume};

/// _IOR('d', 24, uint32_t)
const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
/// _IOR('d', 25, uint64_t)
const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

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

fn u(d: &plist::Dictionary, k: &str) -> u64 {
    d.get(k).and_then(|v| v.as_unsigned_integer()).unwrap_or(0)
}

fn b(d: &plist::Dictionary, k: &str) -> Option<bool> {
    d.get(k).and_then(|v| v.as_boolean())
}

/// The USB serial number of each disk below a USB device, by BSD name.
fn usb_serials() -> std::collections::HashMap<String, String> {
    match Command::new("/usr/sbin/ioreg").args(["-a", "-l", "-r", "-c", "IOUSBHostDevice"]).output() {
        Ok(out) if out.status.success() => crate::ioreg::serials_by_disk(&out.stdout),
        _ => Default::default(),
    }
}

/// BSD name of the disk the system boots from (never offered).
fn boot_disk() -> String {
    diskutil(&["info", "-plist", "/"]).ok().and_then(|x| plist_of(&x).ok()).map(|d| s(&d, "ParentWholeDisk")).unwrap_or_default()
}

pub fn list_sticks() -> Result<Vec<StickInfo>> {
    let top = plist_of(&diskutil(&["list", "-plist", "external", "physical"])?)?;
    let boot = boot_disk();
    let mut out = Vec::new();
    let Some(disks) = top.get("AllDisksAndPartitions").and_then(|v| v.as_array()) else { return Ok(out) };
    let serials = if disks.is_empty() { Default::default() } else { usb_serials() };
    for d in disks.iter().filter_map(|v| v.as_dictionary()) {
        let id = s(d, "DeviceIdentifier");
        if id.is_empty() || id == boot {
            continue;
        }
        let Ok(info) = diskutil(&["info", "-plist", &id]).and_then(|x| plist_of(&x)) else { continue };
        if b(&info, "Internal").unwrap_or(true) {
            continue;
        }
        let mut volumes = Vec::new();
        let parts = d.get("Partitions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let entries: Vec<plist::Dictionary> = if parts.is_empty() {
            vec![d.clone()] // a "superfloppy" stick: the file system is on the whole disk
        } else {
            parts.iter().filter_map(|v| v.as_dictionary().cloned()).collect()
        };
        for p in entries {
            let mp = s(&p, "MountPoint");
            let content = s(&p, "Content");
            if p.get("Partitions").is_some() && content.is_empty() && mp.is_empty() {
                continue;
            }
            volumes.push(StickVolume {
                label: s(&p, "VolumeName"),
                fs_type: content,
                mount_point: if mp.is_empty() { None } else { Some(PathBuf::from(mp)) },
                size_bytes: u(&p, "Size"),
                serial: String::new(),
            });
        }
        out.push(StickInfo {
            id: id.clone(),
            device: format!("/dev/{id}"),
            name: s(&info, "MediaName"),
            size_bytes: u(&info, "TotalSize").max(u(&info, "Size")).max(u(d, "Size")),
            bus: s(&info, "BusProtocol").to_lowercase(),
            volumes,
            serial: serials.get(&id).cloned().unwrap_or_default(),
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

fn raw_disk(f: File, bsd: &str) -> Result<RawDisk> {
    let mut bs: u32 = 0;
    let mut count: u64 = 0;
    // SAFETY: each ioctl writes one integer of the stated width.
    let ok = unsafe {
        libc::ioctl(f.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut bs) == 0
            && libc::ioctl(f.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut count) == 0
    };
    if !ok || bs == 0 {
        return Err(DeviceError::Other(format!("could not read the size of {bsd}: {}", io::Error::last_os_error())));
    }
    Ok(RawDisk::new(f, count * bs as u64, bs, Finish { handle: bsd.to_string() }))
}

pub fn acquire(stick: &StickInfo) -> Result<RawDisk> {
    // Unmounts every volume on the disk; no authorization needed for the console user.
    diskutil(&["unmountDisk", &stick.device])?;
    let f = authopen(&raw_node(&stick.device))?;
    raw_disk(f, &stick.id)
}

/// Close the disk and eject it, so macOS doesn't mount the new partition
/// before it is unplugged.
pub fn finish(disk: RawDisk) -> Result<()> {
    let bsd = disk.finish.handle.clone();
    drop(disk);
    if bsd.is_empty() {
        return Ok(());
    }
    diskutil(&["eject", &format!("/dev/{bsd}")])
        .map(|_| ())
        .map_err(|e| DeviceError::Other(format!("written and verified, but the stick could not be ejected ({e}); eject it before unplugging")))
}

pub fn open_direct(device: &str) -> Result<RawDisk> {
    let info = plist_of(&diskutil(&["info", "-plist", device])?)?;
    if b(&info, "Internal").unwrap_or(true) {
        return Err(DeviceError::UnsafeTarget(device.into(), "internal disk".into()));
    }
    if !b(&info, "WholeDisk").unwrap_or(false) {
        return Err(DeviceError::UnsafeTarget(device.into(), "this is a partition; the whole stick is needed".into()));
    }
    let bsd = s(&info, "DeviceIdentifier");
    if bsd == boot_disk() {
        return Err(DeviceError::UnsafeTarget(device.into(), "the running system is on this disk".into()));
    }
    diskutil(&["unmountDisk", device])?;
    let f = std::fs::OpenOptions::new().read(true).write(true).open(raw_node(device))?;
    raw_disk(f, &bsd)
}

pub fn run_helper_elevated(_helper: &Path, _args: &[String]) -> Result<i32> {
    Err(DeviceError::Other("macOS uses authopen; no elevated helper".into()))
}

pub fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}
