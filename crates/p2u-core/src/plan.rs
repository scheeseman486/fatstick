//! Turning a playlist into a list of files to copy, with FAT-safe target names.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::fat::normalize_name;
use crate::playlist::{Playlist, Source};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanOptions {
    /// Folder to create at the volume root; defaults to the playlist's name.
    pub folder_name: Option<String>,
    /// Prefix names with "001 - " so players that sort by name agree too.
    pub number_prefix: bool,
    /// Transliterate names to ASCII for players that cannot show Unicode.
    pub ascii_names: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions { folder_name: None, number_prefix: true, ascii_names: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub index: usize,
    pub source: PathBuf,
    pub size: u64,
    pub target_name: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub playlist_name: String,
    /// Folder name created at the volume root.
    pub folder_name: String,
    /// Where the folder will be created (volume mount point / folder).
    pub dest_dir: PathBuf,
    /// Path of the folder inside the volume, e.g. "/Road Trip" (used by the raw step).
    pub volume_path: String,
    pub items: Vec<PlanItem>,
    pub missing: Vec<String>,
    pub skipped: Vec<String>,
    pub total_bytes: u64,
}

impl Plan {
    pub fn target_names(&self) -> Vec<String> {
        self.items.iter().map(|i| i.target_name.clone()).collect()
    }
}

/// Characters FAT long names may not contain.
const FORBIDDEN: &[char] = &['"', '*', '/', ':', '<', '>', '?', '\\', '|'];
/// FAT LFN limit is 255 UTF-16 code units per name.
const MAX_UNITS: usize = 255;

pub fn sanitize_component(name: &str, ascii: bool) -> String {
    let name: String = if ascii { deunicode::deunicode(name) } else { name.nfc().collect() };
    let mut s: String = name.chars().map(|c| if FORBIDDEN.contains(&c) || (c as u32) < 0x20 { '_' } else { c }).collect();
    // Trailing dots and spaces are stripped by every FAT driver; leading spaces confuse some players.
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let s = s.trim_start().to_string();
    if s.is_empty() {
        "_".into()
    } else {
        s
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Keep the extension, shorten the stem until the whole name fits `max` UTF-16 units.
fn fit_name(stem: &str, ext: &str, max: usize) -> String {
    let mut stem: String = stem.to_string();
    let suffix = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    while utf16_len(&stem) + utf16_len(&suffix) > max && !stem.is_empty() {
        stem.pop();
    }
    let stem = stem.trim_end_matches([' ', '.']).to_string();
    format!("{stem}{suffix}")
}

/// Build the copy plan. `dest_root` is the volume's mount point (or drive root).
pub fn make_plan(playlist: &Playlist, dest_root: &Path, opts: &PlanOptions) -> Plan {
    let folder_name = sanitize_component(opts.folder_name.as_deref().unwrap_or(&playlist.name), opts.ascii_names);
    let files: Vec<(usize, &PathBuf, Option<&String>)> = playlist
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.source {
            Source::File(p) => Some((i, p, e.title.as_ref())),
            Source::Url(_) => None,
        })
        .collect();
    let skipped = playlist
        .entries
        .iter()
        .filter_map(|e| match &e.source {
            Source::Url(u) => Some(u.clone()),
            _ => None,
        })
        .collect();

    let mut missing = Vec::new();
    let mut present = Vec::new();
    for (_, p, title) in files {
        match std::fs::metadata(p) {
            Ok(m) if m.is_file() => present.push((p.clone(), m.len(), title.cloned())),
            _ => missing.push(p.display().to_string()),
        }
    }
    let width = present.len().to_string().len().max(2);
    let mut used: HashSet<String> = HashSet::new();
    let mut items = Vec::new();
    let mut total = 0u64;
    for (k, (p, size, title)) in present.into_iter().enumerate() {
        let file_name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "track".into());
        let (stem, ext) = match file_name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && e.len() <= 8 => (s.to_string(), e.to_string()),
            _ => (file_name.clone(), String::new()),
        };
        let stem = sanitize_component(&stem, opts.ascii_names);
        let ext = if ext.is_empty() { String::new() } else { sanitize_component(&ext, true) };
        let prefix = if opts.number_prefix { format!("{:0width$} - ", k + 1, width = width) } else { String::new() };
        let mut candidate = fit_name(&format!("{prefix}{stem}"), &ext, MAX_UNITS);
        let mut n = 2;
        while !used.insert(normalize_name(&candidate)) {
            candidate = fit_name(&format!("{prefix}{stem} ({n})"), &ext, MAX_UNITS);
            n += 1;
        }
        total += size;
        items.push(PlanItem { index: k, source: p, size, target_name: candidate, title });
    }
    Plan {
        playlist_name: playlist.name.clone(),
        dest_dir: dest_root.join(&folder_name),
        volume_path: format!("/{folder_name}"),
        folder_name,
        items,
        missing,
        skipped,
        total_bytes: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_forbidden_and_trailing() {
        assert_eq!(sanitize_component("AC/DC: Live? ", false), "AC_DC_ Live_");
        assert_eq!(sanitize_component("...", false), "_");
        assert_eq!(sanitize_component("Café", true), "Cafe");
    }

    #[test]
    fn long_names_keep_extension() {
        let stem = "x".repeat(300);
        let n = fit_name(&stem, "mp3", 255);
        assert_eq!(n.encode_utf16().count(), 255);
        assert!(n.ends_with(".mp3"));
    }
}
