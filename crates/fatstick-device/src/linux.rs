//! Linux: udisks2 over the system D-Bus.
//!
//! * `Filesystem.Unmount` needs no authentication for the user who mounted it.
//! * `Block.OpenDevice` on the whole disk is guarded by polkit action
//!   `org.freedesktop.udisks2.open-device` (`auth_admin_keep` for the active
//!   session). The desktop's polkit agent shows the prompt; if PAM's
//!   `polkit-1` stack includes `pam_fprintd`, a fingerprint works there.
//!   The resulting fd arrives in this (unprivileged) process.
//! * `Drive.PowerOff` afterwards, so nothing mounts the new partition before
//!   the stick is unplugged.
//!
//! Without udisks2, the pkexec helper does the same as root.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::{DeviceError, Finish, RawDisk, Result, StickInfo, StickVolume};

const UDISKS: &str = "org.freedesktop.UDisks2";
const IF_BLOCK: &str = "org.freedesktop.UDisks2.Block";
const IF_PART: &str = "org.freedesktop.UDisks2.Partition";
const IF_FS: &str = "org.freedesktop.UDisks2.Filesystem";
const IF_DRIVE: &str = "org.freedesktop.UDisks2.Drive";
/// _IO(0x12, 104): logical sector size.
const BLKSSZGET: libc::c_ulong = 0x1268;
/// Mount points that mean "this disk runs the system": never touch it.
const SYSTEM_MOUNTS: &[&str] = &["/", "/boot", "/boot/efi", "/efi", "/usr", "/var", "/home"];

type Props = HashMap<String, OwnedValue>;
type Objects = HashMap<OwnedObjectPath, HashMap<zbus::names::OwnedInterfaceName, Props>>;

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

fn objects(c: &Connection) -> Result<Objects> {
    let om = zbus::blocking::fdo::ObjectManagerProxy::builder(c)
        .destination(UDISKS)
        .and_then(|b| b.path("/org/freedesktop/UDisks2"))
        .and_then(|b| b.build())
        .map_err(map_dbus)?;
    om.get_managed_objects().map_err(|e| map_dbus(e.into()))
}

fn iface<'a>(ifaces: &'a HashMap<zbus::names::OwnedInterfaceName, Props>, name: &str) -> Option<&'a Props> {
    ifaces.iter().find(|(n, _)| n.as_str() == name).map(|(_, p)| p)
}

fn mount_points(fs: Option<&Props>) -> Vec<PathBuf> {
    let mps: Vec<Vec<u8>> = fs.and_then(|f| prop(f, "MountPoints")).unwrap_or_default();
    mps.into_iter().map(|m| PathBuf::from(bytes_to_string(m))).collect()
}

/// (stick, object paths of the disk and of every block on it that has a file system)
struct Found {
    info: StickInfo,
    drive: String,
    fs_objects: Vec<String>,
}

fn find_sticks(objs: &Objects) -> Vec<Found> {
    let mut out = Vec::new();
    for (path, ifaces) in objs {
        let Some(block) = iface(ifaces, IF_BLOCK) else { continue };
        if iface(ifaces, IF_PART).is_some() {
            continue; // partitions are listed under their disk
        }
        let Some(drive_path) = prop::<OwnedObjectPath>(block, "Drive") else { continue };
        let Some(drive) = objs.get(&drive_path).and_then(|i| iface(i, IF_DRIVE)) else { continue };
        let bus: String = prop(drive, "ConnectionBus").unwrap_or_default();
        let removable = prop::<bool>(drive, "Removable").unwrap_or(false)
            || prop::<bool>(drive, "MediaRemovable").unwrap_or(false)
            || bus == "usb"
            || bus == "sdio";
        let size: u64 = prop(block, "Size").unwrap_or(0);
        if !removable || size == 0 || prop::<bool>(block, "HintSystem").unwrap_or(true) {
            continue;
        }
        // Everything with a file system on this disk: its partitions, or the
        // disk itself for a "superfloppy" stick.
        let mut volumes = Vec::new();
        let mut fs_objects = Vec::new();
        let mut system = false;
        for (p2, if2) in objs {
            let on_disk = p2 == path
                || iface(if2, IF_PART).and_then(|pp| prop::<OwnedObjectPath>(pp, "Table")).as_ref() == Some(path);
            if !on_disk {
                continue;
            }
            let Some(b2) = iface(if2, IF_BLOCK) else { continue };
            let fs = iface(if2, IF_FS);
            let mps = mount_points(fs);
            system |= mps.iter().any(|m| SYSTEM_MOUNTS.contains(&m.to_string_lossy().as_ref()));
            if fs.is_some() {
                fs_objects.push(p2.to_string());
            }
            let fs_type: String = prop(b2, "IdType").unwrap_or_default();
            if p2 != path || !fs_type.is_empty() {
                volumes.push(StickVolume {
                    label: prop(b2, "IdLabel").unwrap_or_default(),
                    fs_type,
                    mount_point: mps.into_iter().next(),
                    size_bytes: prop(b2, "Size").unwrap_or(0),
                    serial: prop::<String>(b2, "IdUUID").unwrap_or_default().to_uppercase(),
                });
            }
        }
        if system {
            continue;
        }
        let vendor: String = prop(drive, "Vendor").unwrap_or_default();
        let model: String = prop(drive, "Model").unwrap_or_default();
        out.push(Found {
            info: StickInfo {
                id: path.to_string(),
                device: bytes_to_string(prop(block, "Device").unwrap_or_default()),
                name: format!("{} {}", vendor.trim(), model.trim()).trim().to_string(),
                size_bytes: size,
                bus,
                volumes,
            },
            drive: drive_path.to_string(),
            fs_objects,
        });
    }
    out.sort_by(|a, b| a.info.device.cmp(&b.info.device));
    out
}

pub fn list_sticks() -> Result<Vec<StickInfo>> {
    match conn().and_then(|c| objects(&c)) {
        Ok(objs) => Ok(find_sticks(&objs).into_iter().map(|f| f.info).collect()),
        Err(DeviceError::NoBroker(_)) => list_from_sysfs(),
        Err(e) => Err(e),
    }
}

fn empty_opts() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

fn sector_size(f: &File) -> u32 {
    let mut n: libc::c_int = 0;
    // SAFETY: BLKSSZGET writes one int.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), BLKSSZGET as _, &mut n) };
    if r == 0 && n > 0 {
        n as u32
    } else {
        512
    }
}

fn raw_disk(mut f: File, handle: String) -> Result<RawDisk> {
    let size = f.seek(SeekFrom::End(0))?;
    f.seek(SeekFrom::Start(0))?;
    let sector = sector_size(&f);
    Ok(RawDisk::new(f, size, sector, Finish { handle }))
}

pub fn acquire(stick: &StickInfo) -> Result<RawDisk> {
    let c = conn()?;
    let objs = objects(&c)?;
    let found = find_sticks(&objs)
        .into_iter()
        .find(|f| f.info.id == stick.id)
        .ok_or_else(|| DeviceError::Other("the stick is no longer attached".into()))?;
    for p in &found.fs_objects {
        let path = zbus::zvariant::ObjectPath::try_from(p.as_str()).map_err(|e| DeviceError::Other(e.to_string()))?;
        if let Err(e) = c.call_method(Some(UDISKS), &path, Some(IF_FS), "Unmount", &(empty_opts(),)) {
            let s = e.to_string();
            if !s.contains("NotMounted") {
                return Err(if s.contains("Busy") {
                    DeviceError::Other("a file on the stick is still open; close it and try again".into())
                } else {
                    map_dbus(e)
                });
            }
        }
    }
    let path = zbus::zvariant::ObjectPath::try_from(stick.id.as_str()).map_err(|e| DeviceError::Other(e.to_string()))?;
    let mut opts = empty_opts();
    // O_EXCL on a whole disk fails if anything on it is still mounted or held.
    opts.insert("flags", Value::I32(libc::O_EXCL | libc::O_CLOEXEC));
    let reply = c
        .call_method(Some(UDISKS), &path, Some(IF_BLOCK), "OpenDevice", &("rw", opts))
        .map_err(map_dbus)?;
    let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().map_err(|e| DeviceError::Other(e.to_string()))?;
    let fd: OwnedFd = fd.into();
    raw_disk(File::from(fd), found.drive)
}

/// Close the disk and power the stick off through udisks2, so nothing mounts
/// the new partition before it is unplugged. If something mounted it in the
/// meantime, unmount it and try again.
pub fn finish(disk: RawDisk) -> Result<()> {
    let drive = disk.finish.handle.clone();
    let _ = disk.file.sync_all();
    drop(disk);
    if drive.is_empty() {
        return Ok(());
    }
    let c = conn()?;
    let path = zbus::zvariant::ObjectPath::try_from(drive.as_str()).map_err(|e| DeviceError::Other(e.to_string()))?;
    let mut last = String::new();
    for _ in 0..5 {
        match c.call_method(Some(UDISKS), &path, Some(IF_DRIVE), "PowerOff", &(empty_opts(),)) {
            Ok(_) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        // The OS may have picked up the new partition and mounted it.
        if let Ok(objs) = objects(&c) {
            for f in find_sticks(&objs).into_iter().filter(|f| f.drive == drive) {
                for p in f.fs_objects {
                    if let Ok(p) = zbus::zvariant::ObjectPath::try_from(p.as_str()) {
                        let _ = c.call_method(Some(UDISKS), &p, Some(IF_FS), "Unmount", &(empty_opts(),));
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    Err(DeviceError::Other(format!("written and verified, but the stick could not be powered off ({last}); eject it before unplugging")))
}

// ---------------------------------------------------------------------------
// Without udisks2 (root helper via pkexec).

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

/// (device, mount point, fs type) for every mounted block device.
fn mounts() -> Vec<(PathBuf, PathBuf, String)> {
    let text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 3 || !f[0].starts_with("/dev/") {
                return None;
            }
            let dev = Path::new(f[0]).canonicalize().ok()?;
            Some((dev, PathBuf::from(unescape_mount(f[1])), f[2].to_string()))
        })
        .collect()
}

/// sysfs node of a whole disk that is removable or on USB/MMC.
fn removable_disk_sysfs(device: &str) -> Result<(String, PathBuf)> {
    let unsafe_target = |why: &str| DeviceError::UnsafeTarget(device.to_string(), why.to_string());
    let dev = Path::new(device).canonicalize()?;
    let name = dev.file_name().and_then(|n| n.to_str()).ok_or_else(|| unsafe_target("not a device path"))?.to_string();
    let sys = Path::new("/sys/class/block").join(&name).canonicalize().map_err(|_| unsafe_target("not a block device"))?;
    if sys.join("partition").exists() {
        return Err(unsafe_target("this is a partition; the whole stick is needed"));
    }
    let removable = std::fs::read_to_string(sys.join("removable")).map(|s| s.trim() == "1").unwrap_or(false);
    let s = sys.to_string_lossy();
    if !(removable || s.contains("/usb") || s.contains("/mmc")) {
        return Err(unsafe_target("not a removable USB or SD disk"));
    }
    Ok((name, sys))
}

/// Device nodes of the disk and its partitions.
fn disk_nodes(name: &str, sys: &Path) -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from(format!("/dev/{name}"))];
    if let Ok(rd) = std::fs::read_dir(sys) {
        for e in rd.flatten() {
            if e.path().join("partition").exists() {
                v.push(PathBuf::from(format!("/dev/{}", e.file_name().to_string_lossy())));
            }
        }
    }
    v
}

fn list_from_sysfs() -> Result<Vec<StickInfo>> {
    let mut out = Vec::new();
    let ms = mounts();
    for e in std::fs::read_dir("/sys/block")?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let device = format!("/dev/{name}");
        let Ok((name, sys)) = removable_disk_sysfs(&device) else { continue };
        let sectors: u64 = std::fs::read_to_string(sys.join("size")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if sectors == 0 {
            continue;
        }
        let read = |f: &str| std::fs::read_to_string(sys.join("device").join(f)).unwrap_or_default().trim().to_string();
        let nodes = disk_nodes(&name, &sys);
        if ms.iter().any(|(d, mp, _)| nodes.contains(d) && SYSTEM_MOUNTS.contains(&mp.to_string_lossy().as_ref())) {
            continue;
        }
        let volumes = ms
            .iter()
            .filter(|(d, _, _)| nodes.contains(d))
            .map(|(_, mp, t)| StickVolume { label: String::new(), fs_type: t.clone(), mount_point: Some(mp.clone()), size_bytes: 0, serial: String::new() })
            .collect();
        out.push(StickInfo {
            id: device.clone(),
            device,
            name: format!("{} {}", read("vendor"), read("model")).trim().to_string(),
            size_bytes: sectors * 512,
            bus: String::new(),
            volumes,
        });
    }
    Ok(out)
}

/// Root-only path (pkexec helper): check the target, unmount everything on
/// it, open the whole disk exclusively.
pub fn open_direct(device: &str) -> Result<RawDisk> {
    let (name, sys) = removable_disk_sysfs(device)?;
    let nodes = disk_nodes(&name, &sys);
    for (d, mp, _) in mounts() {
        if !nodes.contains(&d) {
            continue;
        }
        if SYSTEM_MOUNTS.contains(&mp.to_string_lossy().as_ref()) {
            return Err(DeviceError::UnsafeTarget(device.into(), "the running system is on this disk".into()));
        }
        let c = std::ffi::CString::new(mp.as_os_str().as_encoded_bytes())
            .map_err(|_| DeviceError::Other("bad mount point".into()))?;
        // SAFETY: c is a valid NUL-terminated path.
        if unsafe { libc::umount2(c.as_ptr(), 0) } != 0 {
            return Err(DeviceError::UnsafeTarget(
                device.into(),
                format!("could not unmount {}: {}", mp.display(), std::io::Error::last_os_error()),
            ));
        }
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_EXCL | libc::O_CLOEXEC)
        .open(format!("/dev/{name}"))?;
    raw_disk(f, String::new())
}

pub fn run_helper_elevated(helper: &Path, args: &[String]) -> Result<i32> {
    // pkexec only runs programs named by an installed polkit policy
    // (packaging/linux/net.fatstick.helper.policy).
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

/// Mounted file systems on a block device node or any of its partitions
/// (used by the CLI before writing to a path given on the command line).
pub fn mounted_on(device: &Path) -> Vec<PathBuf> {
    let Ok(dev) = device.canonicalize() else { return vec![] };
    let name = dev.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let sys = Path::new("/sys/class/block").join(&name);
    let nodes = if sys.exists() { disk_nodes(&name, &sys.canonicalize().unwrap_or(sys)) } else { vec![dev] };
    mounts().into_iter().filter(|(d, _, _)| nodes.contains(d)).map(|(_, mp, _)| mp).collect()
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
