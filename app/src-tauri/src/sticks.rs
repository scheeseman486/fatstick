//! The sticks the window offers: removable USB/SD disks from the device layer,
//! plus, for testing, disk images named in `FATSTICK_TEST_STICKS` (a list of image
//! paths separated like PATH). An image behaves like a stick throughout,
//! except that writing it needs no OS prompt and there's nothing to eject.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fatstick_core::fat::stick::read_top_level_file;
use fatstick_core::fat::{EntryKind, Volume};
use fatstick_device::{StickInfo, StickVolume};
use serde::Serialize;

use crate::exclude::{self, Excluded, Ident};

pub const IMAGE_PREFIX: &str = "image:";
/// Fatstick's note of what a stick holds: a small hidden JSON file at the
/// top level, written last by every sync. Read from wherever the OS mounted
/// the stick, so recognizing a stick needs no password.
pub const NOTE_NAME: &str = ".fatstick.json";
/// The note a stick got when this app was called Playlist2USB: still read,
/// so sticks synced before the rename are recognized.
const OLD_NOTE_NAME: &str = ".playlist2usb.json";
const NOTE_MAX: u64 = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StickView {
    pub id: String,
    pub device: String,
    pub name: String,
    pub size_bytes: u64,
    pub bus: String,
    /// One line per volume: "CAR (FAT32), 2 folders, 31 files".
    pub volumes: Vec<String>,
    pub image: bool,
    /// The note a sync left on the stick, if it's readable (mounted, or an image).
    pub note: Option<serde_json::Value>,
    /// Volume serial numbers, as "1A2B-3C4D", where the OS reports them.
    pub serials: Vec<String>,
    /// The device's own serial number, if the OS gives it: the same whatever
    /// is written to it, so the window can recognize a device it was told
    /// to leave out.
    pub hw_serial: String,
}

/// A note is ours if it says so; anything else is ignored.
fn parse_note(bytes: &[u8]) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let app = v.get("app").and_then(|a| a.as_str());
    (matches!(app, Some("Fatstick" | "Playlist2USB")) && v.get("setId").is_some_and(|s| s.is_string())).then_some(v)
}

fn note_in_folder(mount: &Path) -> Option<serde_json::Value> {
    [NOTE_NAME, OLD_NOTE_NAME].iter().find_map(|name| {
        let p = mount.join(name);
        if std::fs::metadata(&p).ok()?.len() > NOTE_MAX {
            return None;
        }
        parse_note(&std::fs::read(p).ok()?)
    })
}

fn note_in_image(path: &Path) -> (Option<serde_json::Value>, Vec<String>) {
    let Ok(mut f) = OpenOptions::new().read(true).open(path) else { return (None, vec![]) };
    let serial = Volume::open(&mut f).ok().map(|v| serial_text(v.geometry().volume_serial)).into_iter().collect();
    let note = [NOTE_NAME, OLD_NOTE_NAME].iter().find_map(|name| read_top_level_file(&mut f, name, NOTE_MAX).ok().flatten().and_then(|b| parse_note(&b)));
    (note, serial)
}

/// How OSes show a FAT volume serial: "1A2B-3C4D".
pub fn serial_text(serial: u32) -> String {
    format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF)
}

fn test_images() -> Vec<PathBuf> {
    std::env::var_os("FATSTICK_TEST_STICKS").map(|v| std::env::split_paths(&v).filter(|p| p.is_file()).collect()).unwrap_or_default()
}

fn fs_name(t: &str) -> &str {
    match t {
        "vfat" | "fat32" => "FAT32",
        "exfat" => "exFAT",
        "ntfs" => "NTFS",
        "" => "unknown format",
        x => x,
    }
}

fn describe_volume(v: &StickVolume) -> String {
    let label = if v.label.is_empty() { "no label".to_string() } else { v.label.clone() };
    let mut s = format!("{label} ({})", fs_name(&v.fs_type));
    if let Some(m) = &v.mount_point {
        s.push_str(&format!(", mounted at {}", m.display()));
    }
    s
}

/// What a FAT32 image holds at the top level, read the way a player reads it.
fn describe_image(path: &Path) -> Vec<String> {
    let Ok(f) = OpenOptions::new().read(true).open(path) else { return vec![] };
    let Ok(mut vol) = Volume::open(f) else { return vec!["Not FAT32 (or blank)".into()] };
    let label = vol.geometry().volume_label.trim().to_string();
    let Ok(root) = vol.list_dir(fatstick_core::fat::DirRef::Root) else { return vec![format!("{label} (FAT32)")] };
    let dirs = root.groups.iter().filter(|g| g.kind == EntryKind::Dir).count();
    let files = root.groups.iter().filter(|g| g.kind == EntryKind::File && !g.is_hidden()).count();
    let mut parts = vec![format!("{} (FAT32)", if label.is_empty() { "no label" } else { &label })];
    if dirs > 0 {
        parts.push(format!("{dirs} folder{}", if dirs == 1 { "" } else { "s" }));
    }
    if files > 0 {
        parts.push(format!("{files} file{} at the top level", if files == 1 { "" } else { "s" }));
    }
    vec![parts.join(", ")]
}

/// A device found, before anything on it is read.
pub struct Found {
    pub view: StickView,
    mounts: Vec<PathBuf>,
    image: Option<PathBuf>,
}

/// What a device is known by, to leave it out if it's excluded.
pub fn ident(v: &StickView) -> Ident {
    Ident { serial: v.hw_serial.clone(), name: v.name.clone(), size_bytes: v.size_bytes, image: v.image.then(|| v.id.clone()) }
}

fn stick_name(s: &StickInfo) -> String {
    if s.name.is_empty() {
        "USB stick".into()
    } else {
        s.name.clone()
    }
}

/// Every device plugged in (and test image), as the system describes it:
/// nothing on them is read yet.
pub fn found() -> (Vec<Found>, Option<String>) {
    let mut out = Vec::new();
    let mut error = None;
    match fatstick_device::list_sticks() {
        Ok(v) => out.extend(v.into_iter().map(|s| Found {
            mounts: s.volumes.iter().filter_map(|v| v.mount_point.clone()).collect(),
            image: None,
            view: StickView {
                note: None,
                serials: s.volumes.iter().filter(|v| !v.serial.is_empty()).map(|v| v.serial.clone()).collect(),
                hw_serial: s.serial.clone(),
                volumes: s.volumes.iter().map(describe_volume).collect(),
                name: stick_name(&s),
                id: s.id,
                device: s.device,
                size_bytes: s.size_bytes,
                bus: s.bus,
                image: false,
            },
        })),
        Err(e) => error = Some(e.to_string()),
    }
    for p in test_images() {
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        out.push(Found {
            view: StickView {
                id: format!("{IMAGE_PREFIX}{}", p.display()),
                device: String::new(),
                name: format!("Disk image {}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                size_bytes: size,
                bus: "image".into(),
                volumes: describe_image(&p),
                image: true,
                note: None,
                serials: vec![],
                hw_serial: String::new(),
            },
            mounts: vec![],
            image: Some(p),
        });
    }
    (out, error)
}

/// The devices to offer: the excluded ones left out, unread, and the note a
/// sync left read from each of the others.
pub fn shown(found: Vec<Found>, excluded: &[Excluded]) -> Vec<StickView> {
    found
        .into_iter()
        .filter(|f| !exclude::is_excluded(excluded, &ident(&f.view)))
        .map(|f| {
            let mut v = f.view;
            if let Some(p) = &f.image {
                (v.note, v.serials) = note_in_image(p);
            } else {
                v.note = f.mounts.iter().find_map(|m| note_in_folder(m));
            }
            v
        })
        .collect()
}

/// The devices to offer, with no exclusions (tests).
#[cfg(test)]
pub fn list() -> (Vec<StickView>, Option<String>) {
    let (f, e) = found();
    (shown(f, &[]), e)
}

pub enum Target {
    Image(PathBuf),
    Stick(StickInfo),
}

/// The device to write, unless it's gone or excluded.
pub fn find(id: &str, excluded: &[Excluded]) -> Result<Target, String> {
    const LEFT_OUT: &str = "That device is excluded, so nothing is written to it. Nothing was written.";
    if let Some(p) = id.strip_prefix(IMAGE_PREFIX) {
        let p = PathBuf::from(p);
        if !test_images().contains(&p) {
            return Err("That disk image isn't offered any more.".into());
        }
        if exclude::is_excluded(excluded, &Ident { image: Some(id.to_string()), ..Default::default() }) {
            return Err(LEFT_OUT.into());
        }
        return Ok(Target::Image(p));
    }
    let s = fatstick_device::list_sticks()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "The stick was removed or changed. Plug it in and pick it again.".to_string())?;
    let me = Ident { serial: s.serial.clone(), name: stick_name(&s), size_bytes: s.size_bytes, image: None };
    if exclude::is_excluded(excluded, &me) {
        return Err(LEFT_OUT.into());
    }
    Ok(Target::Stick(s))
}

#[cfg(test)]
mod note_tests {
    use super::*;

    #[test]
    fn notes_from_before_the_rename_are_still_read() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".playlist2usb.json"), br#"{"app":"Playlist2USB","format":1,"setId":"set-old"}"#).unwrap();
        assert_eq!(note_in_folder(d.path()).unwrap()["setId"], "set-old");
        // A sync since writes the new note (and erases the old one); the new one wins anyway.
        std::fs::write(d.path().join(".fatstick.json"), br#"{"app":"Fatstick","format":1,"setId":"set-new"}"#).unwrap();
        assert_eq!(note_in_folder(d.path()).unwrap()["setId"], "set-new");
        assert!(parse_note(br#"{"app":"Something else","setId":"x"}"#).is_none());
    }
}
