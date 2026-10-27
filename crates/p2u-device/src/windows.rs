//! Windows: disks found via `\\.\PhysicalDriveN` and storage queries, raw
//! access by an elevated helper that locks and dismounts every volume on the
//! stick first, and elevation via ShellExecuteEx "runas" (UAC). With Windows
//! Hello set up, the UAC credential prompt (and Windows 11 Administrator
//! protection) accept it.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeMmc, BusTypeSd, BusTypeUsb, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetVolumeInformationW,
    GetVolumePathNamesForVolumeNameW, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, DISK_GEOMETRY_EX, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_UPDATE_PROPERTIES, IOCTL_STORAGE_EJECT_MEDIA,
    IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY, PREVENT_MEDIA_REMOVAL, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::{DeviceError, Finish, RawDisk, Result, StickInfo, StickVolume};

const ERROR_CANCELLED: i32 = 1223;
const MAX_DISKS: u32 = 32;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// DeviceIoControl with optional input and an output buffer; returns bytes returned.
fn ioctl(f: &File, code: u32, input: &[u8], out: &mut [u8]) -> Option<u32> {
    let mut returned = 0u32;
    // SAFETY: buffers are valid for the lengths passed.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as HANDLE,
            code,
            if input.is_empty() { null_mut() } else { input.as_ptr().cast() },
            input.len() as u32,
            if out.is_empty() { null_mut() } else { out.as_mut_ptr().cast() },
            out.len() as u32,
            &mut returned,
            null_mut(),
        )
    };
    (ok != 0).then_some(returned)
}

/// Open for queries only (no read/write access needs no elevation).
fn open_query(path: &str) -> Option<File> {
    OpenOptions::new().access_mode(0).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path).ok()
}

fn open_rw(path: &str) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path)
}

struct Descriptor {
    bus: i32,
    name: String,
}

fn descriptor(f: &File) -> Option<Descriptor> {
    // SAFETY: zeroed is a valid STORAGE_PROPERTY_QUERY.
    let mut q: STORAGE_PROPERTY_QUERY = unsafe { std::mem::zeroed() };
    q.PropertyId = StorageDeviceProperty;
    q.QueryType = PropertyStandardQuery;
    // SAFETY: viewing a plain struct as bytes.
    let input = unsafe {
        std::slice::from_raw_parts((&q as *const STORAGE_PROPERTY_QUERY).cast::<u8>(), std::mem::size_of::<STORAGE_PROPERTY_QUERY>())
    };
    let mut out = vec![0u8; 1024];
    let n = ioctl(f, IOCTL_STORAGE_QUERY_PROPERTY, input, &mut out)? as usize;
    if n < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return None;
    }
    // SAFETY: the buffer holds a STORAGE_DEVICE_DESCRIPTOR.
    let d: STORAGE_DEVICE_DESCRIPTOR = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
    let text = |off: u32| -> String {
        if off == 0 || off as usize >= n {
            return String::new();
        }
        let s = &out[off as usize..n];
        let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).trim().to_string()
    };
    let name = format!("{} {}", text(d.VendorIdOffset), text(d.ProductIdOffset)).trim().to_string();
    Some(Descriptor { bus: d.BusType, name })
}

/// (size in bytes, logical sector size)
fn geometry(f: &File) -> Option<(u64, u32)> {
    let mut out = vec![0u8; 256];
    ioctl(f, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut out)?;
    // SAFETY: the buffer holds a DISK_GEOMETRY_EX.
    let g: DISK_GEOMETRY_EX = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
    Some((g.DiskSize as u64, g.Geometry.BytesPerSector))
}

/// Disk numbers a volume (opened for query) lives on.
fn volume_disks(f: &File) -> Vec<u32> {
    let mut out = vec![0u8; 1024];
    let Some(n) = ioctl(f, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut out) else { return vec![] };
    if n < 8 {
        return vec![];
    }
    let count = u32::from_le_bytes(out[0..4].try_into().unwrap()) as usize;
    // DISK_EXTENT is 24 bytes (u32 + padding + i64 + i64), starting at offset 8.
    (0..count)
        .filter_map(|i| {
            let o = 8 + i * 24;
            (o + 4 <= n as usize).then(|| u32::from_le_bytes(out[o..o + 4].try_into().unwrap()))
        })
        .collect()
}

/// Disks holding the running Windows (never offered).
fn system_disks() -> Vec<u32> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    open_query(&format!("\\\\.\\{drive}")).map(|f| volume_disks(&f)).unwrap_or_default()
}

/// Every volume GUID path (`\\?\Volume{...}\`) on the system.
fn all_volumes() -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = [0u16; 261];
    // SAFETY: buffer sized as passed.
    let h = unsafe { FindFirstVolumeW(buf.as_mut_ptr(), buf.len() as u32) };
    if h == INVALID_HANDLE_VALUE {
        return out;
    }
    loop {
        out.push(from_wide(&buf));
        // SAFETY: h is a valid find handle; buffer sized as passed.
        if unsafe { FindNextVolumeW(h, buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            break;
        }
    }
    // SAFETY: closing our own handle.
    unsafe { FindVolumeClose(h) };
    out
}

/// Volume GUID paths on disk `n`.
fn volumes_on(n: u32) -> Vec<String> {
    all_volumes()
        .into_iter()
        .filter(|v| open_query(v.trim_end_matches('\\')).map(|f| volume_disks(&f).contains(&n)).unwrap_or(false))
        .collect()
}

fn volume_info(v: &str) -> StickVolume {
    let wv = wide(OsStr::new(v));
    let mut label = [0u16; 261];
    let mut fsname = [0u16; 261];
    let (mut serial, mut maxlen, mut flags) = (0u32, 0u32, 0u32);
    // SAFETY: buffers sized as passed.
    unsafe {
        GetVolumeInformationW(
            wv.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut maxlen,
            &mut flags,
            fsname.as_mut_ptr(),
            fsname.len() as u32,
        )
    };
    let mut paths = [0u16; 1024];
    let mut len = 0u32;
    // SAFETY: buffer sized as passed.
    let ok = unsafe { GetVolumePathNamesForVolumeNameW(wv.as_ptr(), paths.as_mut_ptr(), paths.len() as u32, &mut len) };
    let mp = if ok != 0 { from_wide(&paths) } else { String::new() };
    StickVolume {
        label: from_wide(&label),
        fs_type: from_wide(&fsname),
        mount_point: if mp.is_empty() { None } else { Some(PathBuf::from(mp)) },
        size_bytes: 0,
    }
}

fn is_stick_bus(bus: i32) -> bool {
    bus == BusTypeUsb || bus == BusTypeSd || bus == BusTypeMmc
}

pub fn list_sticks() -> Result<Vec<StickInfo>> {
    let system = system_disks();
    let mut out = Vec::new();
    for n in 0..MAX_DISKS {
        let path = format!("\\\\.\\PhysicalDrive{n}");
        let Some(f) = open_query(&path) else { continue };
        let Some(d) = descriptor(&f) else { continue };
        if !is_stick_bus(d.bus) || system.contains(&n) {
            continue;
        }
        let Some((size, _)) = geometry(&f) else { continue };
        if size == 0 {
            continue; // card reader without a card
        }
        out.push(StickInfo {
            id: format!("PhysicalDrive{n}"),
            device: path,
            name: d.name,
            size_bytes: size,
            bus: if d.bus == BusTypeUsb { "usb".into() } else { "sd".into() },
            volumes: volumes_on(n).iter().map(|v| volume_info(v)).collect(),
        });
    }
    Ok(out)
}

fn plain_ioctl(f: &File, code: u32) -> bool {
    ioctl(f, code, &[], &mut []).is_some()
}

pub fn open_direct(device: &str) -> Result<RawDisk> {
    let n: u32 = device
        .trim_start_matches("\\\\.\\")
        .strip_prefix("PhysicalDrive")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| DeviceError::UnsafeTarget(device.into(), "expected \\\\.\\PhysicalDriveN".into()))?;
    let q = open_query(device).ok_or_else(|| DeviceError::Other(format!("{device} not found")))?;
    let d = descriptor(&q).ok_or_else(|| DeviceError::Other(format!("{device}: no storage descriptor")))?;
    if !is_stick_bus(d.bus) {
        return Err(DeviceError::UnsafeTarget(device.into(), "not a USB or SD disk".into()));
    }
    if system_disks().contains(&n) {
        return Err(DeviceError::UnsafeTarget(device.into(), "the running system is on this disk".into()));
    }
    drop(q);
    // Writes to sectors of a mounted volume are blocked unless the volume is
    // locked and dismounted. Lock fails while files are open: retry for a while.
    let mut held = Vec::new();
    for v in volumes_on(n) {
        let vf = open_rw(v.trim_end_matches('\\'))?;
        let mut locked = false;
        for _ in 0..20 {
            if plain_ioctl(&vf, FSCTL_LOCK_VOLUME) {
                locked = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if !locked {
            return Err(DeviceError::Other("the stick is in use (close Explorer windows and players using it)".into()));
        }
        if !plain_ioctl(&vf, FSCTL_DISMOUNT_VOLUME) {
            return Err(DeviceError::Other("could not dismount a volume on the stick".into()));
        }
        held.push(vf);
    }
    let f = open_rw(device)?;
    let (size, sector) = geometry(&f).ok_or_else(|| DeviceError::Other(format!("could not read the size of {device}")))?;
    let mut disk = RawDisk::new(f, size, sector, Finish { handle: n.to_string() });
    disk.held_volumes = held;
    Ok(disk)
}

pub fn acquire(stick: &StickInfo) -> Result<RawDisk> {
    if is_elevated() {
        open_direct(&stick.device)
    } else {
        Err(DeviceError::NeedsElevation)
    }
}

/// Tell Windows the partition table changed, then eject: allow removal, then
/// eject, Microsoft's documented sequence. Sticks that are not "removable
/// media" may refuse the eject; that is reported.
pub fn finish(disk: RawDisk) -> Result<()> {
    let _ = disk.file.sync_all();
    plain_ioctl(&disk.file, IOCTL_DISK_UPDATE_PROPERTIES);
    let allow = PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: false };
    // SAFETY: viewing a plain struct as bytes.
    let input = unsafe {
        std::slice::from_raw_parts((&allow as *const PREVENT_MEDIA_REMOVAL).cast::<u8>(), std::mem::size_of::<PREVENT_MEDIA_REMOVAL>())
    };
    ioctl(&disk.file, IOCTL_STORAGE_MEDIA_REMOVAL, input, &mut []);
    let ejected = plain_ioctl(&disk.file, IOCTL_STORAGE_EJECT_MEDIA);
    let err = std::io::Error::last_os_error();
    drop(disk);
    if ejected {
        Ok(())
    } else {
        Err(DeviceError::Other(format!(
            "written and verified, but the stick could not be ejected ({err}); use \"Safely Remove\" before unplugging"
        )))
    }
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut s = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                s.push_str(&"\\".repeat(backslashes * 2 + 1));
                s.push('"');
                backslashes = 0;
            }
            _ => {
                s.push_str(&"\\".repeat(backslashes));
                s.push(c);
                backslashes = 0;
            }
        }
    }
    s.push_str(&"\\".repeat(backslashes * 2));
    s.push('"');
    s
}

pub fn run_helper_elevated(helper: &Path, args: &[String]) -> Result<i32> {
    let file = wide(helper.as_os_str());
    let params = wide(OsStr::new(&args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")));
    let verb = wide(OsStr::new("runas"));
    // SAFETY: zeroed SHELLEXECUTEINFOW is a valid starting point; strings outlive the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: info is fully initialised.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let e = std::io::Error::last_os_error();
        return Err(if e.raw_os_error() == Some(ERROR_CANCELLED) { DeviceError::NotAuthorized } else { e.into() });
    }
    let h = info.hProcess;
    let mut code = 1u32;
    // SAFETY: h is a process handle we own.
    unsafe {
        if WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0 {
            GetExitCodeProcess(h, &mut code);
        }
        CloseHandle(h);
    }
    Ok(code as i32)
}

pub fn is_elevated() -> bool {
    let mut token: HANDLE = null_mut();
    // SAFETY: standard token query.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elev as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elev.TokenIsElevated != 0
    }
}
