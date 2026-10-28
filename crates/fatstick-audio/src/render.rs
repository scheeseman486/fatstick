//! Rendering: one stick file made of stretches of audio and silence.
//!
//! Each part is a track's audio, cut at either end, shifted to another key
//! (pitch without tempo, by Signalsmith Stretch, the whole track at once and
//! the same length out), faded in and out and with a gain, with silence
//! before and after it; or silence alone. The parts are decoded to
//! 32-bit float stereo PCM one after another and streamed, as one WAV stream,
//! into a single LAME encode. That covers a track cut or padded at its ends,
//! a stretch of silence between playlists, and a whole playlist joined into
//! one track, all with the same encoder settings as any other encode.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use crate::cache::Cache;
use crate::decode::PcmStream;
use crate::prepare::Prepared;
use crate::tools::Tools;
use crate::{mp3, AudioError, Result};

/// Frames are stereo f32: 8 bytes each.
const FRAME: usize = 8;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderPart {
    /// The track, by the window's id; `None` for silence alone, `pad_start` long.
    #[serde(default)]
    pub track_id: Option<String>,
    /// Exact gain, applied in floating point.
    #[serde(default)]
    pub gain_db: f64,
    /// Seconds cut from the start and from the end of the track.
    #[serde(default)]
    pub cut_start: f64,
    #[serde(default)]
    pub cut_end: f64,
    /// Seconds of silence before and after it.
    #[serde(default)]
    pub pad_start: f64,
    #[serde(default)]
    pub pad_end: f64,
    /// Semitones up or down (cents as fractions), keeping the length.
    #[serde(default)]
    pub pitch: f64,
    /// When shifting: voices keep their natural character (formants).
    #[serde(default = "yes")]
    pub keep_formants: bool,
    /// Seconds of fade in at the start and fade out at the end (after the cuts).
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
}

/// The fade curve: gain from 0 to 1 as `x` goes from 0 to 1, an S (half a
/// cosine), so a fade starts and ends without a corner. The player's live
/// fades use it too.
pub fn fade_curve(x: f64) -> f64 {
    0.5 - 0.5 * (std::f64::consts::PI * x.clamp(0.0, 1.0)).cos()
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderSpec {
    /// kbps: 320 or 256.
    pub cap: u32,
    #[serde(default)]
    pub vbr: bool,
    /// 44100 or 48000.
    pub sample_rate: u32,
    pub parts: Vec<RenderPart>,
}

/// A part's source, as last read.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    /// The file's content key (see `probe::content_key`).
    pub content_key: String,
}

impl RenderSpec {
    /// Describes the work for the cache key: sources by content, not by id or
    /// path, so moving a file or renaming a playlist reuses the result.
    pub fn recipe(&self, sources: &[Option<Source>]) -> String {
        let mut s = format!("render1/lame3.100/{}{}/{}", if self.vbr { "vbr" } else { "cbr" }, self.cap, self.sample_rate);
        for (p, src) in self.parts.iter().zip(sources) {
            let what = match (&p.track_id, src) {
                (Some(_), Some(x)) => x.content_key.as_str(),
                _ => "silence",
            };
            s.push_str(&format!("|{what}:{:.3}:{:.3}:{:.3}:{:.3}:{:+.2}", p.cut_start, p.cut_end, p.pad_start, p.pad_end, p.gain_db));
            if p.pitch != 0.0 {
                s.push_str(&format!(":key{:+.2}{}:ss1.3.1", p.pitch, if p.keep_formants { "f" } else { "" }));
            }
            if p.fade_in > 0.0 || p.fade_out > 0.0 {
                s.push_str(&format!(":fade{:.3}/{:.3}", p.fade_in, p.fade_out));
            }
        }
        s
    }

    fn check(&self, sources: &[Option<Source>]) -> Result<()> {
        if self.parts.is_empty() {
            return Err(AudioError::Invalid("nothing to render".into()));
        }
        if sources.len() != self.parts.len() {
            return Err(AudioError::Invalid("every part needs its source".into()));
        }
        for (p, s) in self.parts.iter().zip(sources) {
            let secs = [p.cut_start, p.cut_end, p.pad_start, p.pad_end];
            if secs.iter().any(|x| !x.is_finite() || *x < 0.0 || *x > 24.0 * 3600.0) {
                return Err(AudioError::Invalid("cuts and silences are seconds from 0 to a day".into()));
            }
            if p.track_id.is_some() && s.is_none() {
                return Err(AudioError::Invalid("a part's track has no source".into()));
            }
            if [p.fade_in, p.fade_out].iter().any(|x| !x.is_finite() || *x < 0.0 || *x > 600.0) {
                return Err(AudioError::Invalid("fades are seconds from 0 to 600".into()));
            }
            if !p.pitch.is_finite() || p.pitch.abs() > 24.0 {
                return Err(AudioError::Invalid("a key shift is up to 24 semitones either way".into()));
            }
        }
        Ok(())
    }
}

/// A WAV header for 32-bit float stereo with unknown lengths: the encoder
/// reads it with `-ignore_length 1`, to the end of the stream.
fn wav_header(rate: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&u32::MAX.to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * FRAME as u32).to_le_bytes());
    h.extend_from_slice(&(FRAME as u16).to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&u32::MAX.to_le_bytes());
    h
}

fn frames(secs: f64, rate: u32) -> usize {
    (secs * rate as f64).round().max(0.0) as usize
}

fn silence(out: &mut impl Write, n_frames: usize) -> std::io::Result<()> {
    let zeros = [0u8; 64 * 1024];
    let mut left = n_frames * FRAME;
    while left > 0 {
        let n = left.min(zeros.len());
        out.write_all(&zeros[..n])?;
        left -= n;
    }
    Ok(())
}

/// A track's audio from `cut_start`, with the last `cut_end` seconds held
/// back and dropped at the end, so the cut is exact whatever the file's
/// stated length.
fn track_audio(tools: &Tools, out: &mut impl Write, path: &Path, p: &RenderPart, rate: u32) -> Result<()> {
    let mut faded = Faded::new(out, p.fade_in, p.fade_out, rate);
    if p.pitch != 0.0 {
        // The cut track through the shifter, a block at a time (the same as
        // shifting it whole, in constant memory), the same length out.
        let mut sink = Shifted { out: &mut faded, st: Some(crate::stretch::ExactStream::new(2, rate, crate::stretch::Key { semitones: p.pitch as f32, keep_formants: p.keep_formants })), carry: Vec::new(), buf: Vec::new() };
        cut_audio(tools, &mut sink, path, p, rate)?;
        sink.finish()?;
    } else {
        cut_audio(tools, &mut faded, path, p, rate)?;
    }
    faded.finish()
}

/// A writer that fades the float stereo PCM going through it in at the start
/// and out at the end, holding back the end until it knows where it is.
struct Faded<'a, W: Write> {
    out: &'a mut W,
    fade_in: usize,
    fade_out: usize,
    /// Frames passed on so far.
    at: usize,
    carry: Vec<u8>,
    /// The last frames, held back for the fade out.
    held: std::collections::VecDeque<[f32; 2]>,
}

impl<'a, W: Write> Faded<'a, W> {
    fn new(out: &'a mut W, fade_in: f64, fade_out: f64, rate: u32) -> Self {
        Faded { out, fade_in: frames(fade_in, rate), fade_out: frames(fade_out, rate), at: 0, carry: Vec::new(), held: Default::default() }
    }
    fn send(&mut self, f: [f32; 2]) -> std::io::Result<()> {
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&f[0].to_le_bytes());
        b[4..].copy_from_slice(&f[1].to_le_bytes());
        self.out.write_all(&b)
    }
    fn finish(mut self) -> Result<()> {
        let n = self.held.len();
        for (j, f) in std::mem::take(&mut self.held).into_iter().enumerate() {
            // Frames left to the end, this one included.
            let g = fade_curve((n - j) as f64 / self.fade_out.max(1) as f64) as f32;
            self.send([f[0] * g, f[1] * g])?;
        }
        Ok(())
    }
}

impl<W: Write> Write for Faded<'_, W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.fade_in == 0 && self.fade_out == 0 {
            self.out.write_all(data)?;
            return Ok(data.len());
        }
        self.carry.extend_from_slice(data);
        let whole = self.carry.len() / FRAME * FRAME;
        let mut bytes = Vec::with_capacity(whole);
        for c in self.carry[..whole].chunks_exact(FRAME) {
            let mut f = [f32::from_le_bytes(c[..4].try_into().unwrap()), f32::from_le_bytes(c[4..].try_into().unwrap())];
            if self.at < self.fade_in {
                let g = fade_curve(self.at as f64 / self.fade_in as f64) as f32;
                f = [f[0] * g, f[1] * g];
            }
            self.at += 1;
            self.held.push_back(f);
            if self.held.len() > self.fade_out {
                let o = self.held.pop_front().unwrap();
                bytes.extend_from_slice(&o[0].to_le_bytes());
                bytes.extend_from_slice(&o[1].to_le_bytes());
            }
        }
        self.carry.drain(..whole);
        self.out.write_all(&bytes)?;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// The track's audio, cut and with its gain.
fn cut_audio(tools: &Tools, out: &mut dyn Write, path: &Path, p: &RenderPart, rate: u32) -> Result<()> {
    let mut pcm = PcmStream::start(tools, path, p.cut_start, rate, p.gain_db)?;
    let hold = frames(p.cut_end, rate) * FRAME;
    let mut held: Vec<u8> = Vec::with_capacity(hold + 256 * 1024);
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0usize;
    loop {
        let n = match pcm.out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        got += n;
        held.extend_from_slice(&buf[..n]);
        // Pass on whole frames beyond what's held back.
        if held.len() > hold {
            let pass = (held.len() - hold) / FRAME * FRAME;
            out.write_all(&held[..pass])?;
            held.drain(..pass);
        }
    }
    if got == 0 {
        return Err(AudioError::Tool(format!("{} gave no audio{}", path.display(), if p.cut_start > 0.0 { " after the cut" } else { "" })));
    }
    Ok(())
}

/// A writer that shifts the float stereo PCM going through it.
struct Shifted<'a, W: Write> {
    out: &'a mut W,
    st: Option<crate::stretch::ExactStream>,
    /// Bytes short of a whole sample.
    carry: Vec<u8>,
    buf: Vec<f32>,
}

impl<W: Write> Shifted<'_, W> {
    fn emit(&mut self) -> std::io::Result<()> {
        let mut bytes = Vec::with_capacity(self.buf.len() * 4);
        for x in self.buf.drain(..) {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        self.out.write_all(&bytes)
    }
    fn finish(mut self) -> Result<()> {
        if let Some(st) = self.st.take() {
            st.finish(&mut self.buf);
        }
        self.emit()?;
        Ok(())
    }
}

impl<W: Write> Write for Shifted<'_, W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.carry.extend_from_slice(data);
        let whole = self.carry.len() / FRAME * FRAME;
        let samples: Vec<f32> = self.carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        self.carry.drain(..whole);
        if let Some(st) = self.st.as_mut() {
            st.push(&samples, &mut self.buf);
        }
        self.emit()?;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

fn once(tools: &Tools, spec: &RenderSpec, sources: &[Option<Source>], dst: &Path, vbr: bool) -> Result<()> {
    let mut c = tools.ffmpeg();
    c.args(["-v", "error", "-y", "-ignore_length", "1", "-f", "wav", "-i", "pipe:0"]);
    c.args(["-ac", "2", "-ar", &spec.sample_rate.to_string(), "-c:a", "libmp3lame", "-compression_level", "2"]);
    if vbr {
        c.args(["-q:a", "0"]);
    } else {
        c.args(["-b:a", &format!("{}k", spec.cap)]);
    }
    c.args(["-id3v2_version", "0", "-write_id3v1", "0", "-write_xing", "1", "-f", "mp3"]).arg(dst);
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    let mut err = child.stderr.take().expect("piped stderr");
    let errs = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let fed = (|| -> Result<()> {
        let stdin = child.stdin.take().expect("piped stdin");
        let mut w = std::io::BufWriter::with_capacity(256 * 1024, stdin);
        w.write_all(&wav_header(spec.sample_rate))?;
        for (p, src) in spec.parts.iter().zip(sources) {
            silence(&mut w, frames(p.pad_start, spec.sample_rate))?;
            if let (Some(_), Some(s)) = (&p.track_id, src) {
                track_audio(tools, &mut w, &s.path, p, spec.sample_rate)?;
            }
            silence(&mut w, frames(p.pad_end, spec.sample_rate))?;
        }
        w.flush()?;
        Ok(())
    })();
    let status = child.wait()?;
    let stderr = errs.join().unwrap_or_default();
    if !status.success() {
        return Err(AudioError::Tool(format!("the encoder stopped: {}", stderr.trim())));
    }
    fed
}

/// Render to `dst` (raw MP3 frames, no tags). Returns the frame scan. A VBR
/// render with any frame over the cap is done again as CBR.
pub fn render(tools: &Tools, spec: &RenderSpec, sources: &[Option<Source>], dst: &Path) -> Result<mp3::Mp3Info> {
    spec.check(sources)?;
    once(tools, spec, sources, dst, spec.vbr)?;
    let mut info = mp3::scan(dst)?.ok_or_else(|| AudioError::Tool("the encoder wrote no MP3 frames".into()))?;
    if info.peak_bitrate > spec.cap && spec.vbr {
        once(tools, spec, sources, dst, false)?;
        info = mp3::scan(dst)?.ok_or_else(|| AudioError::Tool("the encoder wrote no MP3 frames".into()))?;
    }
    if info.peak_bitrate > spec.cap {
        return Err(AudioError::Tool(format!("the render has frames at {} kbps, over the {} cap", info.peak_bitrate, spec.cap)));
    }
    Ok(info)
}

pub fn cache_key(spec: &RenderSpec, sources: &[Option<Source>]) -> String {
    Cache::key("render", &spec.recipe(sources))
}

/// The rendered file, if it's in the cache.
pub fn ready(cache: &Cache, spec: &RenderSpec, sources: &[Option<Source>]) -> Option<Prepared> {
    let p = cache.get(&cache_key(spec, sources))?;
    let len = std::fs::metadata(&p).ok()?.len();
    Some(Prepared { path: p, offset: 0, len })
}

/// Render into the cache (or find it done).
pub fn prepare(tools: &Tools, cache: &Cache, spec: &RenderSpec, sources: &[Option<Source>]) -> Result<Prepared> {
    if let Some(p) = ready(cache, spec, sources) {
        return Ok(p);
    }
    let path = cache.get_or_make(&cache_key(spec, sources), |tmp| render(tools, spec, sources, tmp).map(|_| ()))?;
    let len = std::fs::metadata(&path)?.len();
    Ok(Prepared { path, offset: 0, len })
}
