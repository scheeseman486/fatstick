//! MP3 encoding with LAME (ffmpeg's libmp3lame), at a profile's bitrate cap.
//!
//! CBR is set with `-b:a`; LAME's quality goes through `-compression_level`,
//! because ffmpeg's `-q:a` switches the encoder to VBR (a 256k CBR request
//! with `-q:a 2` came out at about 115 kbps in testing). Every frame header of
//! the result is read back, and a VBR encode with any frame above the cap is
//! done again as CBR.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::mp3;
use crate::tools::{run, Tools};
use crate::{AudioError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeSpec {
    /// kbps: 320 or 256.
    pub cap: u32,
    pub vbr: bool,
    /// 44100 or 48000.
    pub sample_rate: u32,
    /// Exact gain, applied in floating point before the encoder.
    pub gain_db: f64,
}

impl EncodeSpec {
    /// The part of the cache key that describes this encode.
    pub fn recipe(&self) -> String {
        format!("lame3.100/{}{}/{}/{:+.1}dB", if self.vbr { "vbr" } else { "cbr" }, self.cap, self.sample_rate, self.gain_db)
    }
}

/// The output sample rate for a source: kept if the profile allows it and
/// MPEG-1 can carry it, otherwise 44.1 kHz.
pub fn output_rate(source: u32, allowed: &[u32]) -> u32 {
    if [44100, 48000, 32000].contains(&source) && allowed.contains(&source) {
        source
    } else if allowed.contains(&44100) || allowed.is_empty() {
        44100
    } else {
        allowed[0]
    }
}

fn once(tools: &Tools, src: &Path, dst: &Path, spec: &EncodeSpec, vbr: bool) -> Result<()> {
    let mut c = tools.ffmpeg();
    c.args(["-v", "error", "-y", "-i"]).arg(src);
    c.args(["-map", "0:a:0", "-vn", "-sn", "-dn", "-map_metadata", "-1", "-map_chapters", "-1"]);
    let mut filters = vec![format!("aresample={}:filter_size=64:phase_shift=10:cutoff=0.97", spec.sample_rate)];
    if spec.gain_db != 0.0 {
        filters.insert(0, format!("volume={:.2}dB:precision=float", spec.gain_db));
    }
    c.args(["-af", &filters.join(","), "-ac", "2", "-ar", &spec.sample_rate.to_string()]);
    c.args(["-c:a", "libmp3lame", "-compression_level", "2"]);
    if vbr {
        c.args(["-q:a", "0"]);
    } else {
        c.args(["-b:a", &format!("{}k", spec.cap)]);
    }
    c.args(["-id3v2_version", "0", "-write_id3v1", "0", "-write_xing", "1", "-f", "mp3"]).arg(dst);
    run(&mut c)?;
    Ok(())
}

/// Encode `src` to `dst` (raw MP3 frames, no tags). Returns the frame scan.
pub fn encode(tools: &Tools, src: &Path, dst: &Path, spec: &EncodeSpec) -> Result<mp3::Mp3Info> {
    once(tools, src, dst, spec, spec.vbr)?;
    let mut info = mp3::scan(dst)?.ok_or_else(|| AudioError::Tool("the encoder wrote no MP3 frames".into()))?;
    if info.peak_bitrate > spec.cap && spec.vbr {
        once(tools, src, dst, spec, false)?;
        info = mp3::scan(dst)?.ok_or_else(|| AudioError::Tool("the encoder wrote no MP3 frames".into()))?;
    }
    if info.peak_bitrate > spec.cap {
        return Err(AudioError::Tool(format!("the encode has frames at {} kbps, over the {} cap", info.peak_bitrate, spec.cap)));
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    #[test]
    fn output_rates() {
        use super::output_rate;
        assert_eq!(output_rate(48000, &[44100, 48000]), 48000);
        assert_eq!(output_rate(96000, &[44100, 48000]), 44100);
        assert_eq!(output_rate(22050, &[44100, 48000]), 44100);
        assert_eq!(output_rate(48000, &[44100]), 44100);
    }
}
