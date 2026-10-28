//! Decoding for the player: ffmpeg turns any supported file into 32-bit float
//! stereo PCM at the output device's rate, from any position, with an
//! optional gain (to hear a track as the next sync will write it).

use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};

use crate::tools::Tools;
use crate::Result;

pub struct PcmStream {
    child: Child,
    pub out: ChildStdout,
}

impl PcmStream {
    pub fn start(tools: &Tools, path: &Path, from_secs: f64, rate: u32, gain_db: f64) -> Result<PcmStream> {
        let mut c = tools.ffmpeg();
        c.args(["-v", "error"]);
        if from_secs > 0.0 {
            c.args(["-ss", &format!("{from_secs:.3}")]);
        }
        c.arg("-i").arg(path);
        let mut af = format!("aresample={rate}");
        if gain_db != 0.0 {
            af = format!("volume={gain_db:.2}dB:precision=float,{af}");
        }
        c.args(["-map", "0:a:0", "-vn", "-af", &af, "-ac", "2", "-ar", &rate.to_string(), "-f", "f32le", "-"]);
        let mut child = c.stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let out = child.stdout.take().expect("piped stdout");
        Ok(PcmStream { child, out })
    }
}

impl Drop for PcmStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How loud a whole file is, for drawing it: the peak of each `step` seconds
/// (both channels), from 0 to 255. Decoded at a low rate; it's a picture,
/// not a measurement.
pub fn peaks(tools: &Tools, path: &Path, step: f64) -> Result<Vec<u8>> {
    const RATE: u32 = 11025;
    let per = ((step * RATE as f64).round() as usize).max(1) * 2;
    let mut pcm = PcmStream::start(tools, path, 0.0, RATE, 0.0)?;
    let mut raw = vec![0u8; 1 << 16];
    let (mut carry, mut out) = (Vec::<u8>::new(), Vec::new());
    let (mut peak, mut n) = (0f32, 0usize);
    loop {
        let got = pcm.out.read(&mut raw)?;
        if got == 0 {
            break;
        }
        carry.extend_from_slice(&raw[..got]);
        let whole = carry.len() / 4 * 4;
        for b in carry[..whole].chunks_exact(4) {
            peak = peak.max(f32::from_le_bytes(b.try_into().unwrap()).abs());
            n += 1;
            if n == per {
                out.push((peak.min(1.0) * 255.0).round() as u8);
                (peak, n) = (0.0, 0);
            }
        }
        carry.drain(..whole);
    }
    if n > 0 {
        out.push((peak.min(1.0) * 255.0).round() as u8);
    }
    Ok(out)
}
