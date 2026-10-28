//! Getting a track's audio ready for the stick: a byte range of the source
//! (MP3s copied as they are), or a file in the cache (gain-adjusted copies
//! and encodes). The result is audio frames only; tags come separately.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::Cache;
use crate::encode::{encode, EncodeSpec};
use crate::render::RenderSpec;
use crate::probe::Probe;
use crate::tools::Tools;
use crate::{mp3, AudioError, Result};

/// What to do with a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Recipe {
    /// The MP3's audio frames, bit for bit.
    Copy,
    /// The MP3's frames with global_gain moved by whole 1.5 dB steps.
    Gain { steps: i32 },
    /// Encoded with LAME.
    Encode(EncodeSpec),
    /// Rendered from parts (cut, padded, joined, or silence) and encoded with
    /// LAME. Its sources are the parts' tracks, so it goes through
    /// [`crate::render`] rather than [`prepare`].
    Render(RenderSpec),
}

impl Recipe {
    pub fn describe(&self) -> String {
        match self {
            Recipe::Copy => "copy".into(),
            Recipe::Gain { steps } => format!("gain/{steps:+}"),
            Recipe::Encode(e) => e.recipe(),
            Recipe::Render(r) => format!("render/{}parts", r.parts.len()),
        }
    }
    /// Whether this needs work in the cache (a plain copy streams from the source).
    pub fn needs_cache(&self) -> bool {
        !matches!(self, Recipe::Copy)
    }
}

/// Where the audio for the stick is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub path: PathBuf,
    pub offset: u64,
    pub len: u64,
}

/// Whether an MP3 can go on the stick as it is under `cap` and `rates`.
pub fn copy_allowed(p: &Probe, cap: u32, rates: &[u32]) -> bool {
    p.codec == "mp3"
        && p.mp3.as_ref().is_some_and(|m| m.layer == 3 && m.peak_bitrate <= cap && rates.contains(&m.sample_rate) && m.frames > 0)
}

/// The cache key for a track's recipe.
pub fn cache_key(content_key: &str, recipe: &Recipe) -> String {
    Cache::key(content_key, &recipe.describe())
}

/// Look up without doing any work: `Some` if ready (always for a plain copy).
pub fn ready(cache: &Cache, src: &Path, probe: &Probe, content_key: &str, recipe: &Recipe) -> Option<Prepared> {
    match recipe {
        Recipe::Copy => probe.mp3.as_ref().map(|m| Prepared { path: src.to_path_buf(), offset: m.audio_start, len: m.audio_len }),
        Recipe::Render(_) => None,
        r => cache.get(&cache_key(content_key, r)).and_then(|p| {
            let len = std::fs::metadata(&p).ok()?.len();
            Some(Prepared { path: p, offset: 0, len })
        }),
    }
}

fn copy_range(src: &Path, offset: u64, len: u64, dst: &Path) -> Result<()> {
    let mut f = std::fs::File::open(src)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut out = std::fs::File::create(dst)?;
    let n = std::io::copy(&mut f.take(len), &mut out)?;
    if n != len {
        return Err(AudioError::Invalid(format!("{} is shorter than when it was read", src.display())));
    }
    out.flush()?;
    Ok(())
}

/// Do the work (or find it done).
pub fn prepare(tools: &Tools, cache: &Cache, src: &Path, probe: &Probe, content_key: &str, recipe: &Recipe) -> Result<Prepared> {
    if let Some(p) = ready(cache, src, probe, content_key, recipe) {
        return Ok(p);
    }
    let key = cache_key(content_key, recipe);
    let path = match recipe {
        Recipe::Copy => return Err(AudioError::Invalid(format!("{} has no MP3 frames to copy", src.display()))),
        Recipe::Render(_) => return Err(AudioError::Invalid("a render takes its parts' sources: use render::prepare".into())),
        &Recipe::Gain { steps } => {
            let m = probe.mp3.as_ref().ok_or_else(|| AudioError::Invalid("not an MP3".into()))?;
            cache.get_or_make(&key, |tmp| {
                let raw = cache.temp("raw.mp3");
                copy_range(src, m.audio_start, m.audio_len, &raw)?;
                let r = mp3rgain::GainOptions::new(steps).apply_to_path(&raw, tmp);
                let _ = std::fs::remove_file(&raw);
                r.map_err(|e| AudioError::Tool(format!("lossless gain: {e}")))?;
                // The frames must still be the same frames.
                let after = mp3::scan(tmp)?.ok_or_else(|| AudioError::Tool("lossless gain left no frames".into()))?;
                if after.frames != m.frames {
                    return Err(AudioError::Tool("lossless gain changed the frame count".into()));
                }
                Ok(())
            })?
        }
        Recipe::Encode(spec) => cache.get_or_make(&key, |tmp| encode(tools, src, tmp, spec).map(|_| ()))?,
    };
    let len = std::fs::metadata(&path)?.len();
    Ok(Prepared { path, offset: 0, len })
}
