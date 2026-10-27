//! Windows: volume enumeration via the Win32 drive APIs, raw access via
//! `\\.\E:` with FSCTL_LOCK_VOLUME + FSCTL_DISMOUNT_VOLUME, and elevation via
//! ShellExecuteEx "runas" (UAC). With Windows Hello set up, the UAC credential
//! prompt (and Windows 11 Administrator protection) accept it.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::{DeviceError, RawVolume, Result, VolumeInfo};

const DRIVE_REMOVABLE: u32 = 2;
const ERROR_CANCELLED: i32 = 1223;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn list_volumes() -> Result<Vec<VolumeInfo>> {
    let mut out = Vec::new();
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let wroot = wide(OsStr::new(&root));
        // SAFETY: wroot is NUL-terminated.
        if unsafe { GetDriveTypeW(wroot.as_ptr()) } != DRIVE_REMOVABLE {
            continue;
        }
        let mut label = [0u16; 261];
        let mut fsname = [0u16; 261];
        let mut serial = 0u32;
        let mut maxlen = 0u32;
        let mut flags = 0u32;
        // SAFETY: buffers sized as passed.
        let ok = unsafe {
            GetVolumeInformationW(
                wroot.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                &mut maxlen,
                &mut flags,
                fsname.as_mut_ptr(),
                fsname.len() as u32,
            )
        };
        if ok == 0 {
            continue; // no media
        }
        let fs = from_wide(&fsname);
        if !fs.starts_with("FAT") {
            continue; // FAT, FAT32 (exFAT not supported yet)
        }
        let mut total = 0u64;
        // SAFETY: out-pointers are valid.
        unsafe { GetDiskFreeSpaceExW(wroot.as_ptr(), null_mut(), &mut total, null_mut()) };
        out.push(VolumeInfo {
            id: format!("{letter}:"),
            device: format!("\\\\.\\{letter}:"),
            label: from_wide(&label),
            fs_type: fs,
            mount_point: Some(PathBuf::from(root)),
            size_bytes: total,
            removable: true,
            bus: String::new(),
        });
    }
    Ok(out)
}

fn ioctl(f: &File, code: u32) -> bool {
    let mut returned = 0u32;
    // SAFETY: control codes without buffers.
    unsafe {
        DeviceIoControl(f.as_raw_handle() as HANDLE, code, null_mut(), 0, null_mut(), 0, &mut returned, null_mut()) != 0
    }
}

pub(crate) fn unlock(f: &File) {
    ioctl(f, FSCTL_UNLOCK_VOLUME);
}

pub fn open_direct(device: &str) -> Result<RawVolume> {
    // Only drive-letter volumes on removable media.
    let letter = device.trim_start_matches("\\\\.\\").trim_end_matches(':');
    if letter.len() != 1 {
        return Err(DeviceError::UnsafeTarget(device.into(), "expected \\\\.\\X:".into()));
    }
    let root = wide(OsStr::new(&format!("{letter}:\\")));
    // SAFETY: NUL-terminated.
    if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_REMOVABLE {
        return Err(DeviceError::UnsafeTarget(device.into(), "not a removable drive".into()));
    }
    // CreateFile on a volume needs admin rights and FILE_SHARE_WRITE.
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(device)?;
    // Writes to a mounted volume's file-system sectors are blocked unless the
    // volume is locked or dismounted. Lock fails while files are open: retry.
    let mut locked = false;
    for _ in 0..20 {
        if ioctl(&f, FSCTL_LOCK_VOLUME) {
            locked = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if !locked {
        return Err(DeviceError::Other("the drive is in use (close Explorer windows and players using it)".into()));
    }
    if !ioctl(&f, FSCTL_DISMOUNT_VOLUME) {
        unlock(&f);
        return Err(DeviceError::Other("could not dismount the volume".into()));
    }
    let mut rv = RawVolume::from_file(f);
    rv.locked = true;
    Ok(rv)
}

pub fn acquire(vol: &VolumeInfo) -> Result<RawVolume> {
    if is_elevated() {
        open_direct(&vol.device)
    } else {
        Err(DeviceError::NeedsElevation)
    }
}

pub fn remount(_vol: &VolumeInfo) -> Result<Option<PathBuf>> {
    // Closing the handle unlocks the volume; Windows mounts it again on next access.
    Ok(None)
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
