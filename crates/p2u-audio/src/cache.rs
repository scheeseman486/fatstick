//! Prepared audio, kept between syncs: encodes and gain-adjusted copies,
//! audio frames only (tags are added while streaming onto the stick, so
//! changing names or tags never invalidates anything here).
//!
//! A file's name is a hash of the source's content key and the recipe. The
//! cache has a size cap; files used least recently go first. Deleting the
//! folder is always safe.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub files: u64,
    pub bytes: u64,
    pub dir: String,
}

impl Cache {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Cache> {
        let dir = dir.into();
        std::fs::create_dir_all(dir.join("audio"))?;
        std::fs::create_dir_all(dir.join("tmp"))?;
        Ok(Cache { dir })
    }

    /// The OS cache folder: ~/.cache/playlist2usb, %LOCALAPPDATA%\playlist2usb,
    /// ~/Library/Caches/playlist2usb.
    pub fn default_dir() -> PathBuf {
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
        } else {
            std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        };
        base.unwrap_or_else(std::env::temp_dir).join("playlist2usb")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn key(content_key: &str, recipe: &str) -> String {
        let mut h = blake3::Hasher::new();
        h.update(content_key.as_bytes());
        h.update(b"\0");
        h.update(recipe.as_bytes());
        h.finalize().to_hex()[..40].to_string()
    }

    pub fn path(&self, key: &str) -> PathBuf {
        self.dir.join("audio").join(format!("{key}.mp3"))
    }

    /// The prepared file, if it's there; marks it as just used.
    pub fn get(&self, key: &str) -> Option<PathBuf> {
        let p = self.path(key);
        if p.is_file() {
            let _ = filetime::set_file_mtime(&p, filetime::FileTime::now());
            Some(p)
        } else {
            None
        }
    }

    /// A fresh temporary path inside the cache (same file system, so the
    /// finished file can be renamed into place).
    pub fn temp(&self, what: &str) -> PathBuf {
        let n = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        self.dir.join("tmp").join(format!("{}-{n}-{what}", std::process::id()))
    }

    /// Return the cached file for `key`, making it with `make(tmp)` first if
    /// needed. A half-made file never appears under the key.
    pub fn get_or_make(&self, key: &str, make: impl FnOnce(&Path) -> Result<()>) -> Result<PathBuf> {
        if let Some(p) = self.get(key) {
            return Ok(p);
        }
        let tmp = self.temp("make.mp3");
        let r = make(&tmp);
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        let dst = self.path(key);
        if let Err(e) = std::fs::rename(&tmp, &dst) {
            let _ = std::fs::remove_file(&tmp);
            if !dst.is_file() {
                return Err(e.into());
            }
        }
        Ok(dst)
    }

    fn entries(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let Ok(rd) = std::fs::read_dir(self.dir.join("audio")) else { return Vec::new() };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file().then(|| (e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
            })
            .collect()
    }

    pub fn stats(&self) -> CacheStats {
        let e = self.entries();
        CacheStats { files: e.len() as u64, bytes: e.iter().map(|x| x.1).sum(), dir: self.dir.display().to_string() }
    }

    /// Delete the least recently used files until the cache is at most `cap`
    /// bytes, never touching `keep` (files a sync is about to use).
    pub fn trim(&self, cap: u64, keep: &[PathBuf]) -> u64 {
        let mut e = self.entries();
        let mut total: u64 = e.iter().map(|x| x.1).sum();
        e.sort_by_key(|x| x.2);
        let mut freed = 0;
        for (p, len, _) in e {
            if total <= cap {
                break;
            }
            if keep.contains(&p) {
                continue;
            }
            if std::fs::remove_file(&p).is_ok() {
                total -= len;
                freed += len;
            }
        }
        // Leftovers from interrupted work.
        if let Ok(rd) = std::fs::read_dir(self.dir.join("tmp")) {
            let old = SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
            for x in rd.flatten() {
                if x.metadata().and_then(|m| m.modified()).map_or(false, |t| t < old) {
                    let _ = std::fs::remove_file(x.path());
                }
            }
        }
        freed
    }

    /// Remove every prepared file.
    pub fn clear(&self) -> u64 {
        self.trim(0, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_once_and_trims_oldest_first() {
        let d = tempfile::tempdir().unwrap();
        let c = Cache::open(d.path()).unwrap();
        let k1 = Cache::key("abc", "r1");
        let mut calls = 0;
        for _ in 0..2 {
            c.get_or_make(&k1, |t| {
                calls += 1;
                std::fs::write(t, vec![1u8; 1000]).map_err(Into::into)
            })
            .unwrap();
        }
        assert_eq!(calls, 1);
        let k2 = Cache::key("abc", "r2");
        c.get_or_make(&k2, |t| std::fs::write(t, vec![2u8; 1000]).map_err(Into::into)).unwrap();
        filetime::set_file_mtime(c.path(&k1), filetime::FileTime::from_unix_time(1000, 0)).unwrap();
        assert_eq!(c.stats().bytes, 2000);
        assert_eq!(c.trim(1500, &[]), 1000);
        assert!(c.get(&k1).is_none() && c.get(&k2).is_some());
        // A failed make leaves nothing behind.
        let k3 = Cache::key("x", "y");
        assert!(c.get_or_make(&k3, |_| Err(crate::AudioError::Invalid("no".into()))).is_err());
        assert!(c.get(&k3).is_none());
    }
}
