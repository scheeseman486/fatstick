//! The music folder: every audio file under one folder, kept in the library
//! as the folder changes.
//!
//! A scan walks the folder (names, sizes and times only), reads new and
//! changed files, and recognizes a file that was moved or renamed by its size
//! and content key, so it keeps its id and playlists keep pointing at it.
//! Files that disappeared stay in the library as missing, as they may be on
//! a drive that isn't plugged in.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use p2u_audio::Tools;
use serde::Serialize;

use crate::scan::{is_audio, read_files, ScanProgress};
use crate::{stat, Library, LibraryError, Result};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderScan {
    /// Audio files in the folder now.
    pub files: usize,
    /// Tracks read (new or changed) or moved, by id.
    pub changed: Vec<String>,
    pub added: usize,
    pub moved: usize,
    /// Tracks the library had in the folder whose files are gone.
    pub missing: usize,
    /// Files that couldn't be read.
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderProgress {
    /// "walk" while looking, "read" while reading files.
    pub phase: String,
    pub done: usize,
    pub total: usize,
}

/// The folder as the library stores paths (canonical), or an error a person can act on.
pub fn root_path(root: &Path) -> Result<PathBuf> {
    let p = std::fs::canonicalize(root).map_err(|e| LibraryError::Other(format!("{} can't be opened: {e}", root.display())))?;
    if !p.is_dir() {
        return Err(LibraryError::Other(format!("{} isn't a folder", root.display())));
    }
    Ok(p)
}

/// Whether `path` is inside the music folder `root` (both canonical).
pub fn inside(root: &Path, path: &Path) -> bool {
    path != root && path.starts_with(root)
}

/// Every audio file under `root`, with its size and modified time. Hidden
/// files and folders (names starting with a dot) and symbolic links are
/// skipped.
pub fn walk(root: &Path) -> Vec<(PathBuf, u64, i64)> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                dirs.push(p);
            } else if ft.is_file() && is_audio(&p) {
                if let Some((size, mtime)) = stat(&p) {
                    out.push((p, size, mtime));
                }
            }
        }
    }
    out.sort();
    out
}

/// Bring the library up to date with the folder.
pub fn scan_folder(lib: &Mutex<Library>, tools: &Tools, root: &Path, progress: &(dyn Fn(FolderProgress) + Sync)) -> Result<FolderScan> {
    let root = root_path(root)?;
    progress(FolderProgress { phase: "walk".into(), done: 0, total: 0 });
    let files = walk(&root);
    let known = lib.lock().unwrap().stamps_under(&root)?;
    let by_path: HashMap<PathBuf, (String, u64, i64)> = known.iter().map(|k| (k.path.clone(), (k.id.clone(), k.size, k.mtime))).collect();
    let here: HashSet<&PathBuf> = files.iter().map(|(p, ..)| p).collect();
    // Tracks whose files are gone from where they were: maybe moved.
    let mut vanished: HashMap<(u64, String), String> = known
        .iter()
        .filter(|k| !here.contains(&k.path) && !k.content_key.is_empty() && !k.path.exists())
        .map(|k| ((k.size, k.content_key.clone()), k.id.clone()))
        .collect();
    let vanished_sizes: HashSet<u64> = vanished.keys().map(|(s, _)| *s).collect();

    let mut res = FolderScan { files: files.len(), ..Default::default() };
    let mut todo = Vec::new();
    let mut new_paths = HashSet::new();
    for (path, size, mtime) in &files {
        match by_path.get(path) {
            Some((_, s, m)) if (*s, *m) == (*size, *mtime) => {}
            Some(_) => todo.push(path.clone()),
            None => {
                if vanished_sizes.contains(size) {
                    let key = p2u_audio::probe::content_key(path).unwrap_or_default();
                    if let Some(id) = vanished.remove(&(*size, key)) {
                        let l = lib.lock().unwrap();
                        let id = l.move_track(&id, path)?;
                        l.set_stamp(&id, *size, *mtime)?;
                        res.moved += 1;
                        res.changed.push(id);
                        continue;
                    }
                }
                new_paths.insert(path.clone());
                todo.push(path.clone());
            }
        }
    }
    let total = todo.len();
    progress(FolderProgress { phase: "read".into(), done: 0, total });
    let read = read_files(lib, tools, &todo, &|p: ScanProgress| progress(FolderProgress { phase: "read".into(), done: p.done, total: p.total }));
    for (r, path) in read.into_iter().zip(&todo) {
        match r {
            Ok(id) => {
                if new_paths.contains(path) {
                    res.added += 1;
                }
                res.changed.push(id);
            }
            Err(_) => res.failed += 1,
        }
    }
    res.missing = known.iter().filter(|k| !k.path.exists()).count() - res.moved;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_the_folder() {
        let root = Path::new("/music");
        assert!(inside(root, Path::new("/music/a/b.mp3")));
        assert!(!inside(root, Path::new("/music")));
        assert!(!inside(root, Path::new("/musicals/b.mp3")));
        assert!(!inside(root, Path::new("/other/b.mp3")));
    }
}
