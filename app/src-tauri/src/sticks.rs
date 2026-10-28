//! The sticks the window offers: removable USB/SD disks from the device layer,
//! plus, for testing, disk images named in `P2U_TEST_STICKS` (a list of image
//! paths separated like PATH). An image behaves like a stick throughout,
//! except that writing it needs no OS prompt and there's nothing to eject.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use p2u_core::fat::stick::read_top_level_file;
use p2u_core::fat::{EntryKind, Volume};
use p2u_device::{StickInfo, StickVolume};
use serde::Serialize;

pub const IMAGE_PREFIX: &str = "image:";
/// Playlist2USB's note of what a stick holds: a small hidden JSON file at the
/// top level, written last by every sync. Read from wherever the OS mounted
/// the stick, so recognizing a stick needs no password.
pub const NOTE_NAME: &str = ".playlist2usb.json";
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
}

/// A note is ours if it says so; anything else is ignored.
fn parse_note(bytes: &[u8]) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    (v.get("app").and_then(|a| a.as_str()) == Some("Playlist2USB") && v.get("setId").is_some_and(|s| s.is_string())).then_some(v)
}

fn note_in_folder(mount: &Path) -> Option<serde_json::Value> {
    let p = mount.join(NOTE_NAME);
    if std::fs::metadata(&p).ok()?.len() > NOTE_MAX {
        return None;
    }
    parse_note(&std::fs::read(p).ok()?)
}

fn note_in_image(path: &Path) -> (Option<serde_json::Value>, Vec<String>) {
    let Ok(mut f) = OpenOptions::new().read(true).open(path) else { return (None, vec![]) };
    let serial = Volume::open(&mut f).ok().map(|v| serial_text(v.geometry().volume_serial)).into_iter().collect();
    let note = read_top_level_file(&mut f, NOTE_NAME, NOTE_MAX).ok().flatten().and_then(|b| parse_note(&b));
    (note, serial)
}

/// How OSes show a FAT volume serial: "1A2B-3C4D".
pub fn serial_text(serial: u32) -> String {
    format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF)
}

fn test_images() -> Vec<PathBuf> {
    std::env::var_os("P2U_TEST_STICKS").map(|v| std::env::split_paths(&v).filter(|p| p.is_file()).collect()).unwrap_or_default()
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
    let Ok(root) = vol.list_dir(p2u_core::fat::DirRef::Root) else { return vec![format!("{label} (FAT32)")] };
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

pub fn list() -> (Vec<StickView>, Option<String>) {
    let mut out = Vec::new();
    let mut error = None;
    match p2u_device::list_sticks() {
        Ok(v) => out.extend(v.into_iter().map(|s| StickView {
            note: s.volumes.iter().filter_map(|v| v.mount_point.as_deref()).find_map(note_in_folder),
            serials: s.volumes.iter().filter(|v| !v.serial.is_empty()).map(|v| v.serial.clone()).collect(),
            volumes: s.volumes.iter().map(describe_volume).collect(),
            id: s.id,
            device: s.device,
            name: if s.name.is_empty() { "USB stick".into() } else { s.name },
            size_bytes: s.size_bytes,
            bus: s.bus,
            image: false,
        })),
        Err(e) => error = Some(e.to_string()),
    }
    for p in test_images() {
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let (note, serials) = note_in_image(&p);
        out.push(StickView {
            id: format!("{IMAGE_PREFIX}{}", p.display()),
            device: String::new(),
            name: format!("Disk image {}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            size_bytes: size,
            bus: "image".into(),
            volumes: describe_image(&p),
            image: true,
            note,
            serials,
        });
    }
    (out, error)
}

pub enum Target {
    Image(PathBuf),
    Stick(StickInfo),
}

pub fn find(id: &str) -> Result<Target, String> {
    if let Some(p) = id.strip_prefix(IMAGE_PREFIX) {
        let p = PathBuf::from(p);
        if test_images().contains(&p) {
            return Ok(Target::Image(p));
        }
        return Err("That disk image isn't offered any more.".into());
    }
    p2u_device::list_sticks()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|s| s.id == id)
        .map(Target::Stick)
        .ok_or_else(|| "The stick was removed or changed. Plug it in and pick it again.".into())
}
