//! Decoding for the player: ffmpeg turns any supported file into 32-bit float
//! stereo PCM at the output device's rate, from any position, with an
//! optional gain (to hear a track as the next sync will write it).

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
