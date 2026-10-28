//! The app's own records about your files, in one SQLite database in the
//! app's data folder. Nothing is ever stored inside the audio files, and the
//! library never writes to them.
//!
//! * Tracks: path, size and modified time, a content key (see
//!   `p2u_audio::probe::content_key`) and what ffprobe found. Re-read when the
//!   size or modified time changes.
//! * Analysis: loudness and true peak by content key, so a moved or renamed
//!   file keeps its measurement.
//! * Playlists, player profiles and sticks: JSON documents the window owns,
//!   saved whole and atomically on every edit.
//! * Sync records: what was written to which stick, and when.

pub mod scan;

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use p2u_audio::loudness::Loudness;
use p2u_audio::Probe;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("library database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Audio(#[from] p2u_audio::AudioError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, LibraryError>;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tracks (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  size INTEGER NOT NULL DEFAULT 0,
  mtime INTEGER NOT NULL DEFAULT 0,
  content_key TEXT NOT NULL DEFAULT '',
  probe TEXT,
  error TEXT,
  hint_title TEXT,
  added INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS analysis (
  content_key TEXT PRIMARY KEY,
  lufs REAL NOT NULL,
  true_peak REAL NOT NULL,
  measured INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS docs (
  kind TEXT NOT NULL,
  id TEXT NOT NULL,
  pos INTEGER NOT NULL DEFAULT 0,
  json TEXT NOT NULL,
  PRIMARY KEY (kind, id)
);
CREATE TABLE IF NOT EXISTS syncs (
  id INTEGER PRIMARY KEY,
  set_id TEXT NOT NULL,
  at INTEGER NOT NULL,
  json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

/// A track as the window sees it: the file's facts and tags, flattened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub path: String,
    /// File name without extension.
    pub filename: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub albumartist: String,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub genre: String,
    pub codec: String,
    pub bitrate: u32,
    pub peak_bitrate: u32,
    pub vbr: bool,
    pub sample_rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub duration: f64,
    pub size: u64,
    pub cover: bool,
    pub exists: bool,
    pub drm: bool,
    pub lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub content_key: String,
    /// Why the file couldn't be read, if it couldn't.
    pub error: Option<String>,
}

pub fn track_id(rowid: i64) -> String {
    format!("t{rowid}")
}
pub fn rowid(id: &str) -> Option<i64> {
    id.strip_prefix('t')?.parse().ok()
}

/// Where the library lives: ~/.local/share/playlist2usb, %APPDATA%\playlist2usb,
/// ~/Library/Application Support/playlist2usb.
pub fn default_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("playlist2usb")
}

/// Size and modified time (nanoseconds) of a file, if it is one.
pub fn stat(path: &Path) -> Option<(u64, i64)> {
    let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let t = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0);
    Some((m.len(), t))
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub struct Library {
    conn: Connection,
}

impl Library {
    pub fn open(path: &Path) -> Result<Library> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Library { conn })
    }

    pub fn open_in_memory() -> Result<Library> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Library { conn })
    }

    // ---- tracks -----------------------------------------------------------

    fn row_to_track(&self, row: &rusqlite::Row) -> rusqlite::Result<Track> {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let size: i64 = row.get(2)?;
        let content_key: String = row.get(4)?;
        let probe: Option<String> = row.get(5)?;
        let error: Option<String> = row.get(6)?;
        let hint: Option<String> = row.get(7)?;
        let lufs: Option<f64> = row.get(8)?;
        let peak: Option<f64> = row.get(9)?;
        let p: Probe = probe.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        let filename = Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let exists = Path::new(&path).is_file();
        let mut title = p.tags.title.clone();
        if title.is_empty() && p.codec.is_empty() {
            title = hint.unwrap_or_default();
        }
        Ok(Track {
            id: track_id(id),
            filename,
            title,
            artist: p.tags.artist,
            album: p.tags.album,
            albumartist: p.tags.albumartist,
            year: p.tags.year,
            track: p.tags.track,
            disc: p.tags.disc,
            genre: p.tags.genre,
            codec: p.codec,
            bitrate: p.bitrate,
            peak_bitrate: p.peak_bitrate,
            vbr: p.vbr,
            sample_rate: p.sample_rate,
            bits: p.bits,
            channels: p.channels,
            duration: p.duration,
            size: size as u64,
            cover: p.cover,
            exists,
            drm: p.drm,
            lufs,
            true_peak: peak,
            content_key,
            error,
            path,
        })
    }

    const SELECT: &'static str = "SELECT t.id, t.path, t.size, t.mtime, t.content_key, t.probe, t.error, t.hint_title, a.lufs, a.true_peak
         FROM tracks t LEFT JOIN analysis a ON a.content_key = t.content_key AND t.content_key != ''";

    pub fn tracks(&self) -> Result<Vec<Track>> {
        let mut st = self.conn.prepare(&format!("{} ORDER BY t.id", Self::SELECT))?;
        let rows = st.query_map([], |r| self.row_to_track(r))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn track(&self, id: &str) -> Result<Option<Track>> {
        let Some(n) = rowid(id) else { return Ok(None) };
        let mut st = self.conn.prepare(&format!("{} WHERE t.id = ?1", Self::SELECT))?;
        Ok(st.query_row([n], |r| self.row_to_track(r)).optional()?)
    }

    pub fn track_by_path(&self, path: &Path) -> Result<Option<Track>> {
        let mut st = self.conn.prepare(&format!("{} WHERE t.path = ?1", Self::SELECT))?;
        Ok(st.query_row([path.to_string_lossy()], |r| self.row_to_track(r)).optional()?)
    }

    /// The probe as stored, for the sync.
    pub fn probe_of(&self, id: &str) -> Result<Option<(PathBuf, String, Probe)>> {
        let Some(n) = rowid(id) else { return Ok(None) };
        let r: Option<(String, String, Option<String>)> = self
            .conn
            .query_row("SELECT path, content_key, probe FROM tracks WHERE id = ?1", [n], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        Ok(r.and_then(|(p, k, j)| Some((PathBuf::from(p), k, serde_json::from_str(&j?).ok()?))))
    }

    /// Size and modified time as last read.
    pub fn stamp_of(&self, id: &str) -> Result<Option<(u64, i64)>> {
        let Some(n) = rowid(id) else { return Ok(None) };
        Ok(self
            .conn
            .query_row("SELECT size, mtime FROM tracks WHERE id = ?1", [n], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)))
            .optional()?)
    }

    /// Record a file that was read (or failed to read) at `path`.
    pub fn put_track(&self, path: &Path, size: u64, mtime: i64, content_key: &str, probe: Option<&Probe>, error: Option<&str>) -> Result<String> {
        let probe_json = probe.map(serde_json::to_string).transpose()?;
        self.conn.execute(
            "INSERT INTO tracks (path, size, mtime, content_key, probe, error, added) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET size = ?2, mtime = ?3, content_key = ?4, probe = ?5, error = ?6",
            params![path.to_string_lossy(), size as i64, mtime, content_key, probe_json, error, now()],
        )?;
        let id: i64 = self.conn.query_row("SELECT id FROM tracks WHERE path = ?1", [path.to_string_lossy()], |r| r.get(0))?;
        Ok(track_id(id))
    }

    /// A track a playlist refers to whose file isn't there (yet).
    pub fn put_missing(&self, path: &Path, title_hint: Option<&str>) -> Result<String> {
        self.conn.execute(
            "INSERT INTO tracks (path, hint_title, added) VALUES (?1, ?2, ?3) ON CONFLICT(path) DO NOTHING",
            params![path.to_string_lossy(), title_hint, now()],
        )?;
        let id: i64 = self.conn.query_row("SELECT id FROM tracks WHERE path = ?1", [path.to_string_lossy()], |r| r.get(0))?;
        Ok(track_id(id))
    }

    /// Point a track at a new path (Locate). If another track already has that
    /// path, that one is returned instead and this one is left alone.
    pub fn move_track(&self, id: &str, new_path: &Path) -> Result<String> {
        if let Some(t) = self.track_by_path(new_path)? {
            return Ok(t.id);
        }
        let n = rowid(id).ok_or_else(|| LibraryError::Other(format!("no track {id}")))?;
        self.conn.execute("UPDATE tracks SET path = ?1 WHERE id = ?2", params![new_path.to_string_lossy(), n])?;
        Ok(id.to_string())
    }

    // ---- analysis -----------------------------------------------------------

    pub fn analysis(&self, content_key: &str) -> Result<Option<Loudness>> {
        Ok(self
            .conn
            .query_row("SELECT lufs, true_peak FROM analysis WHERE content_key = ?1", [content_key], |r| {
                Ok(Loudness { integrated: r.get(0)?, true_peak: r.get(1)? })
            })
            .optional()?)
    }

    pub fn put_analysis(&self, content_key: &str, l: &Loudness) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO analysis (content_key, lufs, true_peak, measured) VALUES (?1, ?2, ?3, ?4)",
            params![content_key, l.integrated, l.true_peak, now()],
        )?;
        Ok(())
    }

    // ---- documents (playlists, profiles, sticks) ------------------------------

    pub fn docs(&self, kind: &str) -> Result<Vec<serde_json::Value>> {
        let mut st = self.conn.prepare("SELECT json FROM docs WHERE kind = ?1 ORDER BY pos, rowid")?;
        let rows = st.query_map([kind], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for j in rows {
            if let Ok(v) = serde_json::from_str(&j?) {
                out.push(v);
            }
        }
        Ok(out)
    }

    pub fn put_doc(&self, kind: &str, id: &str, json: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO docs (kind, id, pos, json) VALUES (?1, ?2, (SELECT COALESCE(MAX(pos), 0) + 1 FROM docs WHERE kind = ?1), ?3)
             ON CONFLICT(kind, id) DO UPDATE SET json = ?3",
            params![kind, id, json.to_string()],
        )?;
        Ok(())
    }

    pub fn delete_doc(&self, kind: &str, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM docs WHERE kind = ?1 AND id = ?2", params![kind, id])?;
        Ok(())
    }

    /// Save several documents and deletions of one kind in one transaction,
    /// with `order` (ids) setting their positions.
    pub fn save_docs(&mut self, kind: &str, put: &[serde_json::Value], delete: &[String], order: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for d in put {
            let id = d["id"].as_str().ok_or_else(|| LibraryError::Other(format!("a {kind} has no id")))?;
            tx.execute(
                "INSERT INTO docs (kind, id, pos, json) VALUES (?1, ?2, 0, ?3) ON CONFLICT(kind, id) DO UPDATE SET json = ?3",
                params![kind, id, d.to_string()],
            )?;
        }
        for id in delete {
            tx.execute("DELETE FROM docs WHERE kind = ?1 AND id = ?2", params![kind, id])?;
        }
        for (i, id) in order.iter().enumerate() {
            tx.execute("UPDATE docs SET pos = ?3 WHERE kind = ?1 AND id = ?2", params![kind, id, i as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- sync records -------------------------------------------------------------

    pub fn add_sync(&self, set_id: &str, record: &serde_json::Value) -> Result<()> {
        self.conn.execute("INSERT INTO syncs (set_id, at, json) VALUES (?1, ?2, ?3)", params![set_id, now(), record.to_string()])?;
        Ok(())
    }

    /// The latest record for each stick.
    pub fn last_syncs(&self) -> Result<Vec<serde_json::Value>> {
        let mut st = self.conn.prepare("SELECT json FROM syncs WHERE id IN (SELECT MAX(id) FROM syncs GROUP BY set_id)")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok().and_then(|j| serde_json::from_str(&j).ok())).collect())
    }

    // ---- settings --------------------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tracks_docs_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open(&dir.path().join("lib.db")).unwrap();
        let f = dir.path().join("a song.mp3");
        std::fs::write(&f, b"x").unwrap();
        let mut p = Probe { codec: "mp3".into(), bitrate: 320, duration: 200.0, ..Default::default() };
        p.tags.title = "A Song".into();
        let id = lib.put_track(&f, 1, 5, "key1", Some(&p), None).unwrap();
        // The same path again updates the same track.
        assert_eq!(lib.put_track(&f, 1, 6, "key1", Some(&p), None).unwrap(), id);
        lib.put_analysis("key1", &Loudness { integrated: -11.0, true_peak: -0.5 }).unwrap();
        let t = lib.track(&id).unwrap().unwrap();
        assert_eq!((t.title.as_str(), t.filename.as_str(), t.bitrate, t.lufs, t.exists), ("A Song", "a song", 320, Some(-11.0), true));
        let gone = lib.put_missing(Path::new("/nowhere/x.flac"), Some("Lost Track")).unwrap();
        let g = lib.track(&gone).unwrap().unwrap();
        assert_eq!((g.exists, g.title.as_str()), (false, "Lost Track"));
        // Locate onto a path another track has: that track wins.
        assert_eq!(lib.move_track(&gone, &f).unwrap(), id);

        lib.save_docs("playlist", &[json!({"id": "p1", "name": "One"}), json!({"id": "p2", "name": "Two"})], &[], &["p2".into(), "p1".into()]).unwrap();
        let names: Vec<_> = lib.docs("playlist").unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["Two", "One"]);
        lib.save_docs("playlist", &[], &["p2".into()], &[]).unwrap();
        assert_eq!(lib.docs("playlist").unwrap().len(), 1);

        lib.add_sync("s1", &json!({"setId": "s1", "n": 1})).unwrap();
        lib.add_sync("s1", &json!({"setId": "s1", "n": 2})).unwrap();
        lib.add_sync("s2", &json!({"setId": "s2", "n": 3})).unwrap();
        let mut last: Vec<i64> = lib.last_syncs().unwrap().iter().map(|r| r["n"].as_i64().unwrap()).collect();
        last.sort();
        assert_eq!(last, [2, 3]);
        lib.set_setting("theme", "dark").unwrap();
        assert_eq!(lib.setting("theme").unwrap().as_deref(), Some("dark"));
    }
}
