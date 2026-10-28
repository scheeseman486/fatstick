//! What a file is: codec, stream facts and tags (from ffprobe), plus every MP3
//! frame header (from our own scan) and a content key that survives moves.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mp3::{self, Mp3Info};
use crate::tools::{run, Tools};
use crate::{AudioError, Result};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub albumartist: String,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub genre: String,
    #[serde(default)]
    pub composer: String,
    /// The user's own labels, from the Grouping field (see `tagwrite`).
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    /// mp3, mp2, flac, alac, aac, vorbis, opus, wma, wmapro, wmalossless,
    /// wav, aiff, wavpack, ape, or ffmpeg's own name for anything else.
    pub codec: String,
    /// Average and highest frame bitrate in kbps (0 for lossless).
    pub bitrate: u32,
    pub peak_bitrate: u32,
    pub vbr: bool,
    pub sample_rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub duration: f64,
    pub tags: Tags,
    /// An embedded picture (front cover or otherwise).
    pub cover: bool,
    /// Copy-protected: can't be decoded.
    pub drm: bool,
    pub mp3: Option<Mp3Info>,
}

pub const LOSSLESS: &[&str] = &["flac", "alac", "wav", "aiff", "wavpack", "ape", "wmalossless"];

fn s(v: &Value) -> String {
    match v {
        Value::String(x) => x.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::String(x) => x.trim().parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}
fn leading_u32(x: &str) -> Option<u32> {
    let d: String = x.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    d.parse().ok().filter(|&n| n > 0)
}

fn read_tags(maps: &[&Value]) -> Tags {
    let mut t = Tags::default();
    for m in maps {
        let Some(obj) = m.as_object() else { continue };
        for (k, v) in obj {
            let key: String = k.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let val = s(v);
            if val.is_empty() {
                continue;
            }
            let slot = match key.as_str() {
                "title" => &mut t.title,
                "artist" => &mut t.artist,
                "album" => &mut t.album,
                "albumartist" => &mut t.albumartist,
                "genre" => &mut t.genre,
                "composer" => &mut t.composer,
                "grouping" | "contentgroup" | "tit1" => {
                    if t.labels.is_empty() {
                        t.labels = crate::tagwrite::split_labels(&val);
                    }
                    continue;
                }
                "track" | "tracknumber" => {
                    t.track = t.track.or(leading_u32(&val));
                    continue;
                }
                "disc" | "discnumber" => {
                    t.disc = t.disc.or(leading_u32(&val));
                    continue;
                }
                "date" | "year" | "originaldate" => {
                    t.year = t.year.or(leading_u32(&val).filter(|y| (1000..3000).contains(y)).or_else(|| {
                        val.get(0..4).and_then(|y| y.parse().ok()).filter(|y| (1000..3000).contains(y))
                    }));
                    continue;
                }
                _ => continue,
            };
            if slot.is_empty() {
                *slot = val;
            }
        }
    }
    t
}

fn codec_name(stream: &Value, format: &str) -> String {
    let c = s(&stream["codec_name"]);
    match c.as_str() {
        "mp3" | "mp3float" => "mp3".into(),
        "mp1" | "mp2" => "mp2".into(),
        "wmav1" | "wmav2" => "wma".into(),
        "wavpack" => "wavpack".into(),
        c if c.starts_with("pcm_") => {
            if format.contains("aiff") {
                "aiff".into()
            } else {
                "wav".into()
            }
        }
        _ => c,
    }
}

/// Probe a file. Errors only when ffprobe can't run or finds no audio.
pub fn probe(tools: &Tools, path: &Path) -> Result<Probe> {
    let o = run(tools.ffprobe().args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]).arg(path))?;
    let j: Value = serde_json::from_slice(&o.stdout).map_err(|e| AudioError::Tool(format!("ffprobe output: {e}")))?;
    let streams = j["streams"].as_array().cloned().unwrap_or_default();
    let audio = streams
        .iter()
        .find(|st| st["codec_type"] == "audio")
        .ok_or_else(|| AudioError::Invalid(format!("{} holds no audio", path.display())))?;
    let format = s(&j["format"]["format_name"]);
    let mut p = Probe {
        codec: codec_name(audio, &format),
        sample_rate: num(&audio["sample_rate"]).unwrap_or(0.0) as u32,
        channels: num(&audio["channels"]).unwrap_or(2.0) as u32,
        duration: num(&audio["duration"]).or_else(|| num(&j["format"]["duration"])).unwrap_or(0.0),
        tags: read_tags(&[&j["format"]["tags"], &audio["tags"]]),
        cover: streams.iter().any(|st| st["disposition"]["attached_pic"] == 1),
        ..Default::default()
    };
    p.bits = num(&audio["bits_per_raw_sample"]).or_else(|| num(&audio["bits_per_sample"])).unwrap_or(0.0) as u32;
    if p.bits == 0 {
        p.bits = if p.codec == "flac" || p.codec == "alac" || p.codec == "wav" { 16 } else { 0 };
    }
    let kbps = num(&audio["bit_rate"]).or_else(|| num(&j["format"]["bit_rate"])).map(|b| (b / 1000.0).round() as u32).unwrap_or(0);
    if !LOSSLESS.contains(&p.codec.as_str()) {
        p.bitrate = kbps;
        p.peak_bitrate = kbps;
    }
    let tag = s(&audio["codec_tag_string"]).to_lowercase();
    p.drm = tag == "drms" || tag == "drmi" || s(&audio["codec_name"]).is_empty();
    if p.codec == "mp3" {
        if let Some(m) = mp3::scan(path)? {
            p.bitrate = m.avg_bitrate;
            p.peak_bitrate = m.peak_bitrate;
            p.vbr = m.vbr;
            p.sample_rate = m.sample_rate;
            if p.duration <= 0.0 {
                p.duration = m.duration;
            }
            p.mp3 = Some(m);
        }
    }
    Ok(p)
}

/// A key for a file's contents that survives moving and renaming it: its
/// size and its first and last 64 KiB, hashed.
pub fn content_key(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    h.update(&len.to_le_bytes());
    let mut buf = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut f, &mut buf)?;
    h.update(&buf[..n]);
    if len > 128 * 1024 {
        f.seek(SeekFrom::Start(len - 64 * 1024))?;
        let n = read_up_to(&mut f, &mut buf)?;
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex()[..32].to_string())
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_from_mixed_key_styles() {
        let j: Value = serde_json::json!({ "TITLE": "Hoppípolla", "ARTIST": "Sigur Rós", "ALBUMARTIST": "Sigur Rós",
            "TRACKNUMBER": "2/11", "DATE": "2005-09-12", "album_artist": "ignored, first wins", "disc": "1/2" });
        let t = read_tags(&[&j]);
        assert_eq!(t.title, "Hoppípolla");
        assert_eq!(t.albumartist, "Sigur Rós");
        assert_eq!((t.track, t.disc, t.year), (Some(2), Some(1), Some(2005)));
    }
}
