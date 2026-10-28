//! The sticks the window offers: removable USB/SD disks from the device layer,
//! plus, for testing, disk images named in `P2U_TEST_STICKS` (a list of image
//! paths separated like PATH). An image behaves like a stick throughout,
//! except that writing it needs no OS prompt and there's nothing to eject.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use p2u_core::fat::{EntryKind, Volume};
use p2u_device::{StickInfo, StickVolume};
use serde::Serialize;

pub const IMAGE_PREFIX: &str = "image:";

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
    let files = root.groups.iter().filter(|g| g.kind == EntryKind::File).count();
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
        out.push(StickView {
            id: format!("{IMAGE_PREFIX}{}", p.display()),
            device: p.display().to_string(),
            name: format!("Disk image {}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            size_bytes: size,
            bus: "image".into(),
            volumes: describe_image(&p),
            image: true,
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
