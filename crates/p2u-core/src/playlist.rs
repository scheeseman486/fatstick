//! Reading M3U/M3U8, XSPF (VLC's native format) and PLS playlists.

use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use quick_xml::events::Event;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum PlaylistError {
    #[error("cannot read playlist: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad XSPF playlist: {0}")]
    Xml(String),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum Source {
    File(PathBuf),
    /// A stream or remote URL; cannot be copied.
    Url(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistEntry {
    pub source: Source,
    pub title: Option<String>,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Playlist {
    pub name: String,
    pub path: PathBuf,
    pub entries: Vec<PlaylistEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    M3u,
    Xspf,
    Pls,
}

fn sniff(path: &Path, text: &str) -> Format {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "xspf" => return Format::Xspf,
        "pls" => return Format::Pls,
        "m3u" | "m3u8" => return Format::M3u,
        _ => {}
    }
    let head = text.trim_start();
    if head.starts_with("<?xml") || head.starts_with("<playlist") {
        Format::Xspf
    } else if head.to_ascii_lowercase().starts_with("[playlist]") {
        Format::Pls
    } else {
        Format::M3u
    }
}

/// Decode playlist bytes: UTF-8 (with or without BOM), else Windows-1252,
/// which is what legacy `.m3u` files written on Windows usually are.
fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Turn a playlist location (path, relative path or file:// URI) into a Source.
pub fn resolve_location(loc: &str, base_dir: &Path) -> Source {
    let loc = loc.trim();
    if let Some(rest) = strip_prefix_ci(loc, "file:") {
        return Source::File(file_uri_to_path(rest));
    }
    if let Some(i) = loc.find("://") {
        if loc[..i].chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
            return Source::Url(loc.to_string());
        }
    }
    let p = native_path(loc);
    if p.is_absolute() || is_windows_absolute(loc) {
        Source::File(p)
    } else {
        Source::File(base_dir.join(p))
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn is_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
}

/// Playlists written on Windows use backslashes; elsewhere they are separators too
/// for relative entries, since no sane audio file name contains one.
fn native_path(s: &str) -> PathBuf {
    if cfg!(windows) || is_windows_absolute(s) {
        PathBuf::from(s)
    } else {
        PathBuf::from(s.replace('\\', "/"))
    }
}

fn file_uri_to_path(rest: &str) -> PathBuf {
    // file:///abs, file://localhost/abs, file://server/share (UNC), file:/abs
    let after = if let Some(r) = rest.strip_prefix("//") {
        if let Some(r2) = strip_prefix_ci(r, "localhost/") {
            format!("/{r2}")
        } else if r.starts_with('/') {
            r.to_string()
        } else {
            // UNC host
            format!("//{r}")
        }
    } else {
        rest.to_string()
    };
    let decoded: Vec<u8> = percent_decode_str(&after).collect();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(decoded))
    }
    #[cfg(not(unix))]
    {
        let s = String::from_utf8_lossy(&decoded).into_owned();
        // "/C:/Music/x.mp3" -> "C:/Music/x.mp3"
        let b = s.as_bytes();
        let s = if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            s[1..].to_string()
        } else if let Some(unc) = s.strip_prefix("//") {
            format!("\\\\{}", unc)
        } else {
            s
        };
        PathBuf::from(s.replace('/', "\\"))
    }
}

fn parse_m3u(text: &str, base: &Path, warnings: &mut Vec<String>) -> Vec<PlaylistEntry> {
    let mut out = Vec::new();
    let mut title: Option<String> = None;
    let mut dur: Option<f64> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            // #EXTINF:<seconds>[ attrs],<title>
            let (meta, t) = info.split_once(',').unwrap_or((info, ""));
            dur = meta.split_whitespace().next().and_then(|d| d.parse::<f64>().ok()).filter(|d| *d >= 0.0);
            title = Some(t.trim().to_string()).filter(|t| !t.is_empty());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let source = resolve_location(line, base);
        if let Source::Url(u) = &source {
            warnings.push(format!("line {}: stream URL skipped: {u}", n + 1));
        }
        out.push(PlaylistEntry { source, title: title.take(), duration_secs: dur.take() });
    }
    out
}

fn parse_pls(text: &str, base: &Path) -> Vec<PlaylistEntry> {
    use std::collections::BTreeMap;
    let mut files: BTreeMap<u32, String> = BTreeMap::new();
    let mut titles: BTreeMap<u32, String> = BTreeMap::new();
    let mut lengths: BTreeMap<u32, f64> = BTreeMap::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let num = |p: &str| k.strip_prefix(p).and_then(|n| n.parse::<u32>().ok());
        if let Some(i) = num("file") {
            files.insert(i, v.trim().to_string());
        } else if let Some(i) = num("title") {
            titles.insert(i, v.trim().to_string());
        } else if let Some(i) = num("length") {
            if let Ok(l) = v.trim().parse::<f64>() {
                lengths.insert(i, l);
            }
        }
    }
    files
        .into_iter()
        .map(|(i, f)| PlaylistEntry {
            source: resolve_location(&f, base),
            title: titles.get(&i).cloned(),
            duration_secs: lengths.get(&i).copied().filter(|l| *l >= 0.0),
        })
        .collect()
}

fn parse_xspf(text: &str, base: &Path) -> Result<Vec<PlaylistEntry>, PlaylistError> {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut out = Vec::new();
    let mut in_track = false;
    let mut field: Option<&'static str> = None;
    let mut loc: Option<String> = None;
    let mut title: Option<String> = None;
    let mut dur: Option<f64> = None;
    let mut depth_in_track = 0usize;
    loop {
        match reader.read_event() {
            Err(e) => return Err(PlaylistError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                let name = name.as_ref();
                if name == b"track" {
                    in_track = true;
                    depth_in_track = 0;
                    loc = None;
                    title = None;
                    dur = None;
                } else if in_track {
                    depth_in_track += 1;
                    // Only direct children of <track>; VLC nests <extension> blocks.
                    field = if depth_in_track == 1 {
                        match name {
                            b"location" if loc.is_none() => Some("location"),
                            b"title" => Some("title"),
                            b"duration" => Some("duration"),
                            _ => None,
                        }
                    } else {
                        None
                    };
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(f) = field {
                    let v = t.decode().map_err(|e| PlaylistError::Xml(e.to_string()))?;
                    let v = quick_xml::escape::unescape(&v).map_err(|e| PlaylistError::Xml(e.to_string()))?.into_owned();
                    match f {
                        "location" => loc.get_or_insert_with(String::new).push_str(&v),
                        "title" => title.get_or_insert_with(String::new).push_str(&v),
                        "duration" => dur = v.trim().parse::<f64>().ok().map(|ms| ms / 1000.0),
                        _ => {}
                    }
                }
            }
            Ok(Event::GeneralRef(r)) => {
                // Character/entity references inside text arrive separately in quick-xml >= 0.38.
                if let Some(f) = field {
                    let s = match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => {
                            let raw = r.decode().map_err(|e| PlaylistError::Xml(e.to_string()))?;
                            quick_xml::escape::unescape(&format!("&{raw};")).map(|c| c.into_owned()).unwrap_or_default()
                        }
                    };
                    match f {
                        "location" => loc.get_or_insert_with(String::new).push_str(&s),
                        "title" => title.get_or_insert_with(String::new).push_str(&s),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == b"track" {
                    in_track = false;
                    if let Some(l) = loc.take() {
                        let title = title.take().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                        out.push(PlaylistEntry { source: resolve_location(l.trim(), base), title, duration_secs: dur.take() });
                    }
                } else if in_track {
                    depth_in_track = depth_in_track.saturating_sub(1);
                    field = None;
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Load a playlist file. Relative entries resolve against the playlist's folder.
pub fn load(path: &Path) -> Result<Playlist, PlaylistError> {
    let bytes = std::fs::read(path)?;
    let text = decode_text(&bytes);
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut warnings = Vec::new();
    let entries = match sniff(path, &text) {
        Format::M3u => parse_m3u(&text, &base, &mut warnings),
        Format::Pls => parse_pls(&text, &base),
        Format::Xspf => parse_xspf(&text, &base)?,
    };
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Playlist").to_string();
    Ok(Playlist { name, path: path.to_path_buf(), entries, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_with_extinf_and_relative_paths() {
        let base = Path::new("/music/lists");
        let t = "#EXTM3U\n#EXTINF:215,Artist - First\n../a/01 First.mp3\n\nhttp://radio/stream\n/abs/Second.mp3\n";
        let mut w = Vec::new();
        let e = parse_m3u(t, base, &mut w);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].title.as_deref(), Some("Artist - First"));
        assert_eq!(e[0].duration_secs, Some(215.0));
        assert_eq!(e[0].source, Source::File(PathBuf::from("/music/lists/../a/01 First.mp3")));
        assert!(matches!(e[1].source, Source::Url(_)));
        assert_eq!(e[2].source, Source::File(PathBuf::from("/abs/Second.mp3")));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn xspf_like_vlc_writes_it() {
        let x = r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist xmlns="http://xspf.org/ns/0/" xmlns:vlc="http://www.videolan.org/vlc/playlist/ns/0/" version="1">
  <title>Playlist</title>
  <trackList>
    <track>
      <location>file:///home/me/Music/Caf%C3%A9%20Song.mp3</location>
      <title>Caf&#233; &amp; Bar</title>
      <duration>201000</duration>
      <extension application="http://www.videolan.org/vlc/playlist/0"><vlc:id>0</vlc:id></extension>
    </track>
    <track><location>file:///home/me/Music/Two.flac</location></track>
  </trackList>
</playlist>"#;
        let e = parse_xspf(x, Path::new("/")).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].source, Source::File(PathBuf::from("/home/me/Music/Café Song.mp3")));
        assert_eq!(e[0].title.as_deref(), Some("Café & Bar"));
        assert_eq!(e[0].duration_secs, Some(201.0));
        assert_eq!(e[1].source, Source::File(PathBuf::from("/home/me/Music/Two.flac")));
    }

    #[test]
    fn pls_entries_sorted_by_number() {
        let t = "[playlist]\nFile2=b.mp3\nTitle2=B\nFile1=a.mp3\nNumberOfEntries=2\n";
        let e = parse_pls(t, Path::new("/m"));
        assert_eq!(e[0].source, Source::File(PathBuf::from("/m/a.mp3")));
        assert_eq!(e[1].title.as_deref(), Some("B"));
    }

    #[test]
    fn latin1_m3u_decodes() {
        assert_eq!(decode_text(b"Caf\xe9.mp3"), "Café.mp3");
    }
}
