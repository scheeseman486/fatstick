//! Linux: udisks2 over the system D-Bus.
//!
//! * `Filesystem.Unmount` needs no authentication for the user who mounted it.
//! * `Block.OpenDevice` is guarded by polkit action
//!   `org.freedesktop.udisks2.open-device` (`auth_admin_keep` for the active
//!   session). The desktop's polkit agent shows the prompt; if PAM's
//!   `polkit-1` stack includes `pam_fprintd`, a fingerprint works there.
//!   The resulting fd arrives in this (unprivileged) process.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::{DeviceError, RawVolume, Result, VolumeInfo};

const UDISKS: &str = "org.freedesktop.UDisks2";
const IF_BLOCK: &str = "org.freedesktop.UDisks2.Block";
const IF_FS: &str = "org.freedesktop.UDisks2.Filesystem";
const IF_DRIVE: &str = "org.freedesktop.UDisks2.Drive";

type Props = HashMap<String, OwnedValue>;

fn prop<T: TryFrom<OwnedValue>>(p: &Props, key: &str) -> Option<T> {
    p.get(key)?.try_clone().ok()?.try_into().ok()
}

fn bytes_to_string(mut b: Vec<u8>) -> String {
    while b.last() == Some(&0) {
        b.pop();
    }
    String::from_utf8_lossy(&b).into_owned()
}

fn conn() -> Result<Connection> {
    Connection::system().map_err(|e| DeviceError::NoBroker(format!("system D-Bus: {e}")))
}

fn map_dbus(e: zbus::Error) -> DeviceError {
    let s = e.to_string();
    if s.contains("NotAuthorized") || s.contains("Cancelled") || s.contains("Dismissed") {
        DeviceError::NotAuthorized
    } else if s.contains("ServiceUnknown") {
        DeviceError::NoBroker("udisks2 is not running".into())
    } else {
        DeviceError::Other(format!("udisks2: {s}"))
    }
}

pub fn list_volumes() -> Result<Vec<VolumeInfo>> {
    match list_udisks() {
        Err(DeviceError::NoBroker(_)) => list_from_mounts(),
        other => other,
    }
}

/// Fallback without udisks2: mounted vfat file systems on removable/USB block
/// devices. Locking such a volume goes through the pkexec helper.
fn list_from_mounts() -> Result<Vec<VolumeInfo>> {
    let mounts = std::fs::read_to_string("/proc/self/mounts")?;
    let mut out = Vec::new();
    for line in mounts.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 || !(f[2] == "vfat" || f[2] == "msdos") || !f[0].starts_with("/dev/") {
            continue;
        }
        let Ok(sys) = removable_sysfs(f[0]) else { continue };
        let mp = PathBuf::from(unescape_mount(f[1]));
        let sectors: u64 = std::fs::read_to_string(sys.join("size")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        out.push(VolumeInfo {
            id: f[0].to_string(),
            device: f[0].to_string(),
            label: mp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            fs_type: f[2].to_string(),
            mount_point: Some(mp),
            size_bytes: sectors * 512,
            removable: true,
            bus: String::new(),
        });
    }
    Ok(out)
}

/// /proc/mounts escapes space, tab, newline and backslash as octal.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 4]).unwrap(), 8).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn list_udisks() -> Result<Vec<VolumeInfo>> {
    let c = conn()?;
    let om = zbus::blocking::fdo::ObjectManagerProxy::builder(&c)
        .destination(UDISKS)
        .and_then(|b| b.path("/org/freedesktop/UDisks2"))
        .and_then(|b| b.build())
        .map_err(map_dbus)?;
    let objects = om.get_managed_objects().map_err(|e| map_dbus(e.into()))?;

    let mut drives: HashMap<String, Props> = HashMap::new();
    for (path, ifaces) in &objects {
        for (name, props) in ifaces {
            if name.as_str() == IF_DRIVE {
                drives.insert(path.to_string(), props.clone());
            }
        }
    }

    let mut out = Vec::new();
    for (path, ifaces) in &objects {
        let mut block = None;
        let mut fs = None;
        for (name, props) in ifaces {
            match name.as_str() {
                IF_BLOCK => block = Some(props),
                IF_FS => fs = Some(props),
                _ => {}
            }
        }
        let (Some(block), Some(fs)) = (block, fs) else { continue };
        let id_type: String = prop(block, "IdType").unwrap_or_default();
        if id_type != "vfat" && id_type != "msdos" {
            continue;
        }
        if prop::<bool>(block, "HintSystem").unwrap_or(true) {
            continue;
        }
        let drive_path: Option<OwnedObjectPath> = prop(block, "Drive");
        let drive = drive_path.and_then(|d| drives.get(d.as_str()));
        let removable = drive
            .map(|d| prop::<bool>(d, "Removable").unwrap_or(false) || prop::<bool>(d, "MediaRemovable").unwrap_or(false))
            .unwrap_or(false);
        let bus: String = drive.and_then(|d| prop(d, "ConnectionBus")).unwrap_or_default();
        let mounts: Vec<Vec<u8>> = prop(fs, "MountPoints").unwrap_or_default();
        out.push(VolumeInfo {
            id: path.to_string(),
            device: bytes_to_string(prop(block, "Device").unwrap_or_default()),
            label: prop(block, "IdLabel").unwrap_or_default(),
            fs_type: id_type,
            mount_point: mounts.into_iter().next().map(|m| PathBuf::from(bytes_to_string(m))),
            size_bytes: prop(block, "Size").unwrap_or(0),
            removable,
            bus,
        });
    }
    out.sort_by(|a, b| a.device.cmp(&b.device));
    Ok(out)
}

fn empty_opts() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

pub fn acquire(vol: &VolumeInfo) -> Result<RawVolume> {
    let c = conn()?;
    let path = zbus::zvariant::ObjectPath::try_from(vol.id.as_str()).map_err(|e| DeviceError::Other(e.to_string()))?;
    if vol.mount_point.is_some() {
        if let Err(e) = c.call_method(Some(UDISKS), &path, Some(IF_FS), "Unmount", &(empty_opts(),)) {
            let s = e.to_string();
            if !s.contains("NotMounted") {
                return Err(map_dbus(e));
            }
        }
    }
    let mut opts = empty_opts();
    // O_EXCL on a block device fails if anything still has it mounted.
    opts.insert("flags", Value::I32(libc::O_EXCL | libc::O_CLOEXEC));
    let reply = c
        .call_method(Some(UDISKS), &path, Some(IF_BLOCK), "OpenDevice", &("rw", opts))
        .map_err(map_dbus)?;
    let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().map_err(|e| DeviceError::Other(e.to_string()))?;
    let fd: OwnedFd = fd.into();
    Ok(RawVolume::from_file(File::from(fd)))
}

pub fn remount(vol: &VolumeInfo) -> Result<Option<PathBuf>> {
    let c = conn()?;
    let path = zbus::zvariant::ObjectPath::try_from(vol.id.as_str()).map_err(|e| DeviceError::Other(e.to_string()))?;
    let reply = c.call_method(Some(UDISKS), &path, Some(IF_FS), "Mount", &(empty_opts(),)).map_err(map_dbus)?;
    let mp: String = reply.body().deserialize().map_err(|e| DeviceError::Other(e.to_string()))?;
    Ok(Some(PathBuf::from(mp)))
}

/// The sysfs node of `device` if it is a removable or USB/MMC block device.
fn removable_sysfs(device: &str) -> Result<PathBuf> {
    let unsafe_target = |why: &str| DeviceError::UnsafeTarget(device.to_string(), why.to_string());
    let dev = Path::new(device).canonicalize()?;
    let name = dev.file_name().and_then(|n| n.to_str()).ok_or_else(|| unsafe_target("not a device path"))?;
    let sys = Path::new("/sys/class/block").join(name).canonicalize().map_err(|_| unsafe_target("not a block device"))?;
    let sys_s = sys.to_string_lossy().into_owned();
    // For a partition the "removable" flag lives on the parent disk.
    let disk = if sys.join("partition").exists() { sys.parent().map(Path::to_path_buf).unwrap_or(sys.clone()) } else { sys.clone() };
    let removable = std::fs::read_to_string(disk.join("removable")).map(|s| s.trim() == "1").unwrap_or(false);
    if !(removable || sys_s.contains("/usb") || sys_s.contains("/mmc")) {
        return Err(unsafe_target("not a removable or USB device"));
    }
    Ok(sys)
}

fn mount_points_of(device: &str) -> Result<Vec<PathBuf>> {
    let dev = Path::new(device).canonicalize()?;
    let mounts = std::fs::read_to_string("/proc/self/mounts")?;
    Ok(mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let src = f.next()?;
            let mp = f.next()?;
            (Path::new(src).canonicalize().ok()? == dev).then(|| PathBuf::from(unescape_mount(mp)))
        })
        .collect())
}

/// Root-only path (pkexec helper): check the target, unmount it, open it exclusively.
pub fn open_direct(device: &str) -> Result<RawVolume> {
    removable_sysfs(device)?;
    for mp in mount_points_of(device)? {
        let c = std::ffi::CString::new(mp.as_os_str().as_encoded_bytes())
            .map_err(|_| DeviceError::Other("bad mount point".into()))?;
        // SAFETY: c is a valid NUL-terminated path.
        if unsafe { libc::umount2(c.as_ptr(), 0) } != 0 {
            return Err(DeviceError::UnsafeTarget(device.into(), format!("could not unmount {}: {}", mp.display(), std::io::Error::last_os_error())));
        }
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_EXCL | libc::O_CLOEXEC)
        .open(device)?;
    Ok(RawVolume::from_file(f))
}

pub fn run_helper_elevated(helper: &Path, args: &[String]) -> Result<i32> {
    // pkexec only runs programs named by an installed polkit policy
    // (packaging/linux/net.playlist2usb.helper.policy).
    let status = std::process::Command::new("pkexec")
        .arg("--disable-internal-agent")
        .arg(helper)
        .args(args)
        .status()
        .map_err(|e| DeviceError::NoBroker(format!("pkexec: {e}")))?;
    match status.code() {
        Some(126) | Some(127) => Err(DeviceError::NotAuthorized),
        Some(c) => Ok(c),
        None => Err(DeviceError::Other("helper killed by a signal".into())),
    }
}

pub fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mount_paths_unescape() {
        assert_eq!(super::unescape_mount(r"/media/me/MY\040STICK"), "/media/me/MY STICK");
        assert_eq!(super::unescape_mount(r"/a\134b"), r"/a\b");
        assert_eq!(super::unescape_mount(r"/plain"), "/plain");
    }
}
