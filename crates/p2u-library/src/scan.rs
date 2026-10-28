//! Adding files: folders searched recursively, every file probed (in
//! parallel), playlists imported and exported, and missing files located.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};
use std::sync::Mutex;

use p2u_audio::{probe, Probe, Tools};
use serde::Serialize;

use crate::{stat, Library, Result, Track};

pub const AUDIO_EXT: &[&str] = &["mp3", "mp2", "flac", "m4a", "aac", "mp4", "ogg", "oga", "opus", "wma", "wav", "aif", "aiff", "wv", "ape"];
pub const PLAYLIST_EXT: &[&str] = &["m3u", "m3u8", "xspf", "pls"];

fn ext(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}
pub fn is_audio(p: &Path) -> bool {
    AUDIO_EXT.contains(&ext(p).as_str())
}
pub fn is_playlist(p: &Path) -> bool {
    PLAYLIST_EXT.contains(&ext(p).as_str())
}

/// Compare names with runs of digits compared as numbers ("2" before "10").
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().filter(|c| c.is_ascii_digit()) {
                    n.push(*c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(d) = y.peek().filter(|d| d.is_ascii_digit()) {
                    m.push(*d);
                    y.next();
                }
                let (n2, m2) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n2.len().cmp(&m2.len()).then(n2.cmp(m2));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// Audio files under the given paths (folders searched recursively), in
/// folder order, then by name. Playlist files are returned separately.
pub fn expand(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut audio = Vec::new();
    let mut lists = Vec::new();
    fn walk(dir: &Path, audio: &mut Vec<PathBuf>, depth: usize) {
        if depth > 32 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort_by(|a, b| natural(&a.to_string_lossy(), &b.to_string_lossy()));
        let (dirs, files): (Vec<_>, Vec<_>) = entries.into_iter().partition(|p| p.is_dir());
        audio.extend(files.into_iter().filter(|p| is_audio(p)));
        for d in dirs {
            let hidden = d.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            if !hidden {
                walk(&d, audio, depth + 1);
            }
        }
    }
    for p in paths {
        if p.is_dir() {
            walk(p, &mut audio, 0);
        } else if is_playlist(p) {
            lists.push(p.clone());
        } else if is_audio(p) {
            audio.push(p.clone());
        }
    }
    (audio, lists)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub done: usize,
    pub total: usize,
    pub path: String,
}

/// Read one file and record it. Unchanged files (same size and modified
/// time) aren't read again.
pub fn read_file(lib: &Mutex<Library>, tools: &Tools, path: &Path) -> Result<String> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Some((size, mtime)) = stat(&path) else {
        return lib.lock().unwrap().put_missing(&path, None);
    };
    {
        let l = lib.lock().unwrap();
        if let Some(t) = l.track_by_path(&path)? {
            if l.stamp_of(&t.id)? == Some((size, mtime)) && !t.codec.is_empty() {
                return Ok(t.id);
            }
        }
    }
    let key = p2u_audio::probe::content_key(&path).unwrap_or_default();
    match probe(tools, &path) {
        Ok(p) => lib.lock().unwrap().put_track(&path, size, mtime, &key, Some(&p), None),
        Err(e) => lib.lock().unwrap().put_track(&path, size, mtime, &key, None, Some(&e.to_string())),
    }
}

/// Read many files on all cores but one. Returns ids in the order given.
pub fn read_files(lib: &Mutex<Library>, tools: &Tools, paths: &[PathBuf], progress: &(dyn Fn(ScanProgress) + Sync)) -> Vec<Result<String>> {
    let n = paths.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<String>>>> = Mutex::new((0..n).map(|_| None).collect());
    let workers = std::thread::available_parallelism().map(|n| n.get().saturating_sub(1).max(1)).unwrap_or(2).min(8);
    std::thread::scope(|s| {
        for _ in 0..workers.min(n.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, AtOrd::Relaxed);
                if i >= n {
                    break;
                }
                let r = read_file(lib, tools, &paths[i]);
                results.lock().unwrap()[i] = Some(r);
                let d = done.fetch_add(1, AtOrd::Relaxed) + 1;
                progress(ScanProgress { done: d, total: n, path: paths[i].display().to_string() });
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.unwrap_or_else(|| Err(crate::LibraryError::Other("not read".into())))).collect()
}

/// Order tracks from dropped folders: by folder, then disc, track number and
/// file name (numbers compared as numbers). Tracks without numbers go after
/// the numbered ones in their folder.
pub fn album_order(tracks: &mut [Track]) {
    let folder = |t: &Track| Path::new(&t.path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    tracks.sort_by(|a, b| {
        natural(&folder(a), &folder(b))
            .then(a.disc.unwrap_or(0).cmp(&b.disc.unwrap_or(0)))
            .then(a.track.unwrap_or(u32::MAX).cmp(&b.track.unwrap_or(u32::MAX)))
            .then_with(|| natural(&a.filename, &b.filename))
    });
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub name: String,
    pub track_ids: Vec<String>,
    /// Streams and URLs, which can't go on a stick.
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

/// Import an M3U/M3U8, XSPF or PLS playlist. The playlist file isn't changed;
/// entries whose files are missing become missing tracks.
pub fn import(lib: &Mutex<Library>, tools: &Tools, path: &Path, progress: &(dyn Fn(ScanProgress) + Sync)) -> Result<Imported> {
    let pl = p2u_core::playlist::load(path).map_err(|e| crate::LibraryError::Other(e.to_string()))?;
    let mut files = Vec::new();
    let mut titles = Vec::new();
    let mut skipped = Vec::new();
    for e in &pl.entries {
        match &e.source {
            p2u_core::playlist::Source::File(p) => {
                files.push(p.clone());
                titles.push(e.title.clone());
            }
            p2u_core::playlist::Source::Url(u) => skipped.push(u.clone()),
        }
    }
    let ids = read_files(lib, tools, &files, progress);
    let mut track_ids = Vec::new();
    for ((r, p), title) in ids.into_iter().zip(&files).zip(&titles) {
        let id = match r {
            Ok(id) => id,
            Err(_) => lib.lock().unwrap().put_missing(p, title.as_deref())?,
        };
        if !p.is_file() {
            lib.lock().unwrap().put_missing(p, title.as_deref())?;
        }
        track_ids.push(id);
    }
    Ok(Imported { name: pl.name, track_ids, skipped, warnings: pl.warnings })
}

fn rel(target: &Path, base: &Path) -> Option<PathBuf> {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// Write a playlist file: M3U8 (or M3U), or XSPF when the name ends in .xspf.
/// Paths are absolute, or relative to the playlist's folder.
pub fn export(tracks: &[Track], name: &str, path: &Path, relative: bool) -> Result<()> {
    let base = path.parent().unwrap_or(Path::new("."));
    let loc = |t: &Track| -> String {
        let p = Path::new(&t.path);
        if relative {
            if let Some(r) = rel(p, base) {
                return r.to_string_lossy().replace('\\', "/");
            }
        }
        t.path.clone()
    };
    let text = if ext(path) == "xspf" {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
        let mut x = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<playlist xmlns=\"http://xspf.org/ns/0/\" version=\"1\">\n  <title>{}</title>\n  <trackList>\n", esc(name));
        for t in tracks {
            let l = loc(t);
            let uri = if relative && !Path::new(&l).is_absolute() {
                percent_encoding::utf8_percent_encode(&l, URI).to_string()
            } else {
                let abs = t.path.replace('\\', "/");
                let abs = if abs.starts_with('/') { abs } else { format!("/{abs}") };
                format!("file://{}", percent_encoding::utf8_percent_encode(&abs, URI))
            };
            x.push_str(&format!("    <track>\n      <location>{}</location>\n", esc(&uri)));
            if !t.title.is_empty() {
                x.push_str(&format!("      <title>{}</title>\n", esc(&t.title)));
            }
            if !t.artist.is_empty() {
                x.push_str(&format!("      <creator>{}</creator>\n", esc(&t.artist)));
            }
            if t.duration > 0.0 {
                x.push_str(&format!("      <duration>{}</duration>\n", (t.duration * 1000.0).round() as u64));
            }
            x.push_str("    </track>\n");
        }
        x.push_str("  </trackList>\n</playlist>\n");
        x
    } else {
        let mut m = String::from("#EXTM3U\n");
        if !name.is_empty() {
            m.push_str(&format!("#PLAYLIST:{name}\n"));
        }
        for t in tracks {
            let label = match (t.artist.is_empty(), t.title.is_empty()) {
                (_, true) => t.filename.clone(),
                (true, false) => t.title.clone(),
                (false, false) => format!("{} - {}", t.artist, t.title),
            };
            m.push_str(&format!("#EXTINF:{},{label}\n{}\n", t.duration.round() as i64, loc(t)));
        }
        m
    };
    std::fs::write(path, text)?;
    Ok(())
}

const URI: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS.add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'[').add(b']').add(b'`').add(b'{').add(b'}');

/// After locating one missing file in `new_dir`: other missing tracks whose
/// file names are there too.
pub fn same_folder_matches(missing: &[Track], new_dir: &Path) -> Vec<(String, PathBuf)> {
    missing
        .iter()
        .filter(|t| !t.exists)
        .filter_map(|t| {
            let name = Path::new(&t.path).file_name()?;
            let cand = new_dir.join(name);
            cand.is_file().then(|| (t.id.clone(), cand))
        })
        .collect()
}

/// Whether a stored probe still describes the file (size and modified time unchanged).
pub fn is_current(lib: &Library, t: &Track) -> bool {
    match (stat(Path::new(&t.path)), lib.stamp_of(&t.id).ok().flatten()) {
        (Some(now), Some(then)) => now == then,
        _ => false,
    }
}

/// Probe is used by the app to decide copy or encode at sync time.
pub fn probe_ok(p: &Probe) -> bool {
    !p.codec.is_empty() && !p.drm && p.duration > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["Track 10.mp3", "track 2.mp3", "Track 1.mp3", "a.mp3", "Track 02b.mp3"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["a.mp3", "Track 1.mp3", "track 2.mp3", "Track 02b.mp3", "Track 10.mp3"]);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(rel(Path::new("/m/a/b.mp3"), Path::new("/m/lists")).unwrap(), PathBuf::from("../a/b.mp3"));
        assert_eq!(rel(Path::new("/m/lists/b.mp3"), Path::new("/m/lists")).unwrap(), PathBuf::from("b.mp3"));
    }

    #[test]
    fn export_and_import_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("Music");
        std::fs::create_dir_all(&music).unwrap();
        let mk = |n: &str, title: &str| {
            let p = music.join(n);
            std::fs::write(&p, b"x").unwrap();
            Track { id: "t1".into(), path: p.display().to_string(), filename: n.into(), title: title.into(), artist: "Ärtist".into(), duration: 61.4, ..Default::default() }
        };
        let tracks = vec![mk("one #1.mp3", "One & Only"), mk("two.flac", "")];
        for (name, rel) in [("list.m3u8", true), ("list.xspf", true), ("abs.m3u8", false), ("abs.xspf", false)] {
            let out = dir.path().join(name);
            export(&tracks, "Road Trip", &out, rel).unwrap();
            let back = p2u_core::playlist::load(&out).unwrap();
            let paths: Vec<_> = back
                .entries
                .iter()
                .map(|e| match &e.source {
                    p2u_core::playlist::Source::File(p) => std::fs::canonicalize(p).unwrap_or_else(|_| panic!("{name}: {} ->\n{}", p.display(), std::fs::read_to_string(&out).unwrap())),
                    _ => panic!("url"),
                })
                .collect();
            let want: Vec<_> = tracks.iter().map(|t| std::fs::canonicalize(&t.path).unwrap()).collect();
            assert_eq!(paths, want, "{name}");
        }
    }
}
