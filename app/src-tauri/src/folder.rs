//! The music folder in the app: which folder it is, scanning it (at start,
//! when it changes, and when asked), and the two things the library view
//! does to files in it: writing tags and renaming. Nothing outside the folder
//! is ever written.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use p2u_audio::tagwrite::{write_tags, TagEdit};
use p2u_audio::Tools;
use p2u_library::folder::{inside, root_path, scan_folder, FolderProgress, FolderScan};
use p2u_library::{scan, Library, Track};
use serde::{Deserialize, Serialize};

pub const SETTING: &str = "libraryRoot";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    /// The music folder, as given; none until one is chosen.
    pub root: Option<String>,
    pub scanning: bool,
    pub watching: bool,
    /// When the last scan finished (ms since the epoch).
    pub last_scan: Option<u64>,
    pub last: Option<FolderScan>,
    pub error: Option<String>,
}

/// After a scan: what changed, for the window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scanned {
    pub info: FolderInfo,
    pub tracks: Vec<Track>,
}

type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub struct MusicFolder {
    lib: Arc<Mutex<Library>>,
    tools: Option<Tools>,
    emit: Emit,
    root: Mutex<Option<PathBuf>>,
    watcher: Mutex<Option<RecommendedWatcher>>,
    scanning: AtomicBool,
    again: AtomicBool,
    last: Mutex<Option<(u64, FolderScan)>>,
    error: Mutex<Option<String>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl MusicFolder {
    /// The folder from the settings, scanned and watched, if one was chosen.
    pub fn start(lib: Arc<Mutex<Library>>, tools: Option<Tools>, emit: impl Fn(&str, serde_json::Value) + Send + Sync + 'static) -> Arc<MusicFolder> {
        let root = lib.lock().unwrap().setting(SETTING).ok().flatten().map(PathBuf::from);
        let f = Arc::new(MusicFolder {
            lib,
            tools,
            emit: Arc::new(emit),
            root: Mutex::new(None),
            watcher: Mutex::new(None),
            scanning: AtomicBool::new(false),
            again: AtomicBool::new(false),
            last: Mutex::new(None),
            error: Mutex::new(None),
        });
        if let Some(r) = root {
            let _ = f.use_root(&r);
        }
        f
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.root.lock().unwrap().clone()
    }

    pub fn info(&self) -> FolderInfo {
        let last = self.last.lock().unwrap().clone();
        FolderInfo {
            root: self.root().map(|r| r.display().to_string()),
            scanning: self.scanning.load(Ordering::SeqCst),
            watching: self.watcher.lock().unwrap().is_some(),
            last_scan: last.as_ref().map(|(at, _)| *at),
            last: last.map(|(_, s)| s),
            error: self.error.lock().unwrap().clone(),
        }
    }

    /// Choose the music folder: remembered, scanned and watched.
    pub fn set_root(self: &Arc<Self>, path: &Path) -> Result<FolderInfo, String> {
        let root = root_path(path).map_err(|e| e.to_string())?;
        self.lib.lock().unwrap().set_setting(SETTING, &root.display().to_string()).map_err(|e| e.to_string())?;
        *self.last.lock().unwrap() = None;
        self.use_root(&root)?;
        Ok(self.info())
    }

    fn use_root(self: &Arc<Self>, path: &Path) -> Result<(), String> {
        *self.watcher.lock().unwrap() = None;
        let root = match root_path(path) {
            Ok(r) => r,
            Err(e) => {
                // Kept as chosen (it may be a drive that isn't plugged in).
                *self.root.lock().unwrap() = Some(path.to_path_buf());
                *self.error.lock().unwrap() = Some(e.to_string());
                return Err(e.to_string());
            }
        };
        *self.root.lock().unwrap() = Some(root.clone());
        *self.error.lock().unwrap() = None;
        // Watch it: changes are scanned two seconds after they stop.
        let (tx, rx) = channel::<()>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if changes_files(&ev) {
                    let _ = tx.send(());
                }
            }
        });
        match watcher {
            Ok(mut w) => {
                if w.watch(&root, RecursiveMode::Recursive).is_ok() {
                    *self.watcher.lock().unwrap() = Some(w);
                    let me = Arc::downgrade(self);
                    // Ends when the watcher goes (another folder, or the app quits).
                    std::thread::spawn(move || {
                        while rx.recv().is_ok() {
                            loop {
                                match rx.recv_timeout(Duration::from_secs(2)) {
                                    Ok(()) => continue,
                                    Err(RecvTimeoutError::Timeout) => break,
                                    Err(RecvTimeoutError::Disconnected) => return,
                                }
                            }
                            match me.upgrade() {
                                Some(f) => f.scan(),
                                None => return,
                            }
                        }
                    });
                }
            }
            Err(e) => eprintln!("music folder: can't watch {}: {e}", root.display()),
        }
        self.scan();
        Ok(())
    }

    /// Scan in the background; a scan asked for during one runs after it.
    pub fn scan(self: &Arc<Self>) {
        let Some(tools) = self.tools.clone() else { return };
        if self.root().is_none() {
            return;
        }
        if self.scanning.swap(true, Ordering::SeqCst) {
            self.again.store(true, Ordering::SeqCst);
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || loop {
            let root = me.root().unwrap_or_default();
            let emit = me.emit.clone();
            let progress = move |p: FolderProgress| emit("library-progress", serde_json::to_value(&p).unwrap_or_default());
            (me.emit)("library-progress", serde_json::json!({ "phase": "walk", "done": 0, "total": 0 }));
            let r = scan_folder(&me.lib, &tools, &root, &progress);
            let tracks = match &r {
                Ok(s) => {
                    *me.error.lock().unwrap() = None;
                    let l = me.lib.lock().unwrap();
                    s.changed.iter().filter_map(|id| l.track(id).ok().flatten()).collect()
                }
                Err(e) => {
                    *me.error.lock().unwrap() = Some(e.to_string());
                    Vec::new()
                }
            };
            if let Ok(s) = r {
                *me.last.lock().unwrap() = Some((now_ms(), s));
            }
            // Asked again while scanning: once more. Otherwise done.
            let more = me.again.swap(false, Ordering::SeqCst);
            if !more {
                me.scanning.store(false, Ordering::SeqCst);
            }
            let out = Scanned { info: me.info(), tracks };
            (me.emit)("library-scanned", serde_json::to_value(&out).unwrap_or_default());
            if !more {
                break;
            }
        });
    }

    /// A track's file, if it's inside the music folder (the only files the
    /// app writes to).
    fn writable(&self, t: &Track) -> Result<PathBuf, String> {
        let root = self.root().ok_or("Choose your music folder first.")?;
        let p = PathBuf::from(&t.path);
        if !inside(&root, &p) {
            return Err(format!("{} isn't in your music folder, so it's left as it is.", t.path));
        }
        if !p.is_file() {
            return Err(format!("{} isn't there any more.", t.path));
        }
        Ok(p)
    }
}

/// Whether a watched event may have changed the music: files made, removed,
/// renamed or written. Opening and reading don't count, or the scan's own
/// reading (and playing, and syncing) would set off another scan, forever.
/// Hidden files and pictures, text and playlist files don't count either.
fn changes_files(ev: &notify::Event) -> bool {
    match ev.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => {}
        EventKind::Access(_) => return false,
        _ => {}
    }
    const SKIP: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "txt", "nfo", "log", "cue", "m3u", "m3u8", "pls", "xspf", "db", "ini", "part", "crdownload", "tmp"];
    ev.paths.is_empty()
        || ev.paths.iter().any(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            !name.starts_with('.') && !SKIP.contains(&ext.as_str())
        })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagJob {
    pub track_id: String,
    pub edit: TagEdit,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameJob {
    pub track_id: String,
    /// The new file name, extension included.
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failed {
    pub track_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOps {
    /// The tracks as they are now.
    pub tracks: Vec<Track>,
    pub failed: Vec<Failed>,
}

/// Write tag edits into files in the music folder, then read them again.
pub fn write(folder: &MusicFolder, jobs: &[TagJob]) -> FileOps {
    let mut out = FileOps::default();
    let Some(tools) = folder.tools.clone() else {
        out.failed = jobs.iter().map(|j| Failed { track_id: j.track_id.clone(), error: "ffmpeg isn't available".into() }).collect();
        return out;
    };
    for j in jobs {
        let r = (|| -> Result<Track, String> {
            let t = folder.lib.lock().unwrap().track(&j.track_id).map_err(|e| e.to_string())?.ok_or("That track isn't in the library.")?;
            let path = folder.writable(&t)?;
            write_tags(&path, &j.edit).map_err(|e| e.to_string())?;
            scan::read_file(&folder.lib, &tools, &path).map_err(|e| e.to_string())?;
            folder.lib.lock().unwrap().track(&j.track_id).map_err(|e| e.to_string())?.ok_or_else(|| "The track went missing.".into())
        })();
        match r {
            Ok(t) => out.tracks.push(t),
            Err(error) => out.failed.push(Failed { track_id: j.track_id.clone(), error }),
        }
    }
    out
}

/// A file name that works on every system: no separators, no characters
/// Windows refuses, no trailing dot or space.
fn check_name(name: &str) -> Result<(), String> {
    const BAD: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
    if name.trim().is_empty() || name == "." || name == ".." {
        return Err("A file needs a name.".into());
    }
    if name.chars().any(|c| BAD.contains(&c) || (c as u32) < 0x20) || name.ends_with('.') || name.ends_with(' ') {
        return Err(format!("{name:?} can't be a file name on every system."));
    }
    if name.len() > 240 {
        return Err("That file name is too long.".into());
    }
    Ok(())
}

/// Rename files in the music folder, within their own folders.
pub fn rename(folder: &MusicFolder, jobs: &[RenameJob]) -> FileOps {
    let mut out = FileOps::default();
    for j in jobs {
        let r = (|| -> Result<Track, String> {
            let lib = &folder.lib;
            let t = lib.lock().unwrap().track(&j.track_id).map_err(|e| e.to_string())?.ok_or("That track isn't in the library.")?;
            let src = folder.writable(&t)?;
            check_name(&j.name)?;
            let ext = |p: &Path| p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            let dst = src.parent().ok_or("That file has no folder.")?.join(&j.name);
            if ext(&dst) != ext(&src) {
                return Err(format!("{} would change the file's type (.{}).", j.name, ext(&src)));
            }
            if dst == src {
                return Ok(t);
            }
            let same_file = dst.exists() && std::fs::canonicalize(&dst).ok() == std::fs::canonicalize(&src).ok();
            if dst.exists() && !same_file {
                return Err(format!("A file named {} is already there.", j.name));
            }
            if let Some(other) = lib.lock().unwrap().track_by_path(&dst).map_err(|e| e.to_string())? {
                if other.id != t.id && !same_file {
                    return Err(format!("The library already has a track at {} (its file is missing). Pick another name.", j.name));
                }
            }
            if same_file {
                // Only the case changes: go through another name, as some
                // systems treat the two names as one file.
                let tmp = src.with_file_name(format!(".p2u-rename-{}", j.track_id));
                std::fs::rename(&src, &tmp).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
            } else {
                std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
            }
            let canon = std::fs::canonicalize(&dst).unwrap_or(dst);
            let l = lib.lock().unwrap();
            l.move_track(&t.id, &canon).map_err(|e| e.to_string())?;
            l.track(&t.id).map_err(|e| e.to_string())?.ok_or_else(|| "The track went missing.".into())
        })();
        match r {
            Ok(t) => out.tracks.push(t),
            Err(error) => out.failed.push(Failed { track_id: j.track_id.clone(), error }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changes_count() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        let ev = |kind, p: &str| notify::Event { kind, paths: vec![PathBuf::from(p)], attrs: Default::default() };
        assert!(changes_files(&ev(EventKind::Create(CreateKind::File), "/m/a.mp3")));
        assert!(changes_files(&ev(EventKind::Remove(RemoveKind::Any), "/m/Album")));
        assert!(changes_files(&ev(EventKind::Modify(ModifyKind::Any), "/m/a.flac")));
        assert!(changes_files(&ev(EventKind::Access(AccessKind::Close(AccessMode::Write)), "/m/a.flac")));
        assert!(!changes_files(&ev(EventKind::Access(AccessKind::Open(AccessMode::Any)), "/m/a.flac")), "reading a file");
        assert!(!changes_files(&ev(EventKind::Access(AccessKind::Close(AccessMode::Read)), "/m/Album")), "listing a folder");
        assert!(!changes_files(&ev(EventKind::Create(CreateKind::File), "/m/.p2u-rename-t1")));
        assert!(!changes_files(&ev(EventKind::Modify(ModifyKind::Any), "/m/Album/cover.jpg")));
    }

    #[test]
    fn names_that_work_everywhere() {
        assert!(check_name("01 - Artist - Title.mp3").is_ok());
        assert!(check_name("AC/DC.mp3").is_err());
        assert!(check_name("What?.mp3").is_err());
        assert!(check_name("x.mp3 ").is_err());
        assert!(check_name("").is_err());
    }
}
