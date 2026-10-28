//! Integrated loudness and true peak (ITU-R BS.1770, as ReplayGain 2.0 uses),
//! measured on the decoded audio by ffmpeg's ebur128 filter.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tools::{run, Tools};
use crate::{AudioError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loudness {
    /// LUFS.
    pub integrated: f64,
    /// dBTP.
    pub true_peak: f64,
}

pub fn measure(tools: &Tools, path: &Path) -> Result<Loudness> {
    let o = run(tools
        .ffmpeg()
        .args(["-nostats", "-v", "info", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-vn", "-af", "ebur128=peak=true", "-f", "null", "-"]))?;
    parse(&String::from_utf8_lossy(&o.stderr)).ok_or_else(|| AudioError::Tool("ebur128 printed no summary".into()))
}

/// Read the filter's closing summary.
pub(crate) fn parse(log: &str) -> Option<Loudness> {
    let summary = &log[log.rfind("Summary:")?..];
    let value = |section: &str, key: &str| -> Option<f64> {
        let s = &summary[summary.find(section)?..];
        let line = s.lines().find(|l| l.trim_start().starts_with(key))?;
        let v = line.trim_start()[key.len()..].split_whitespace().next()?;
        if v == "-inf" {
            Some(f64::NEG_INFINITY)
        } else {
            v.parse().ok()
        }
    };
    let integrated = value("Integrated loudness:", "I:")?;
    let true_peak = value("True peak:", "Peak:")?;
    // Digital silence measures -70 LUFS (the gate) or -inf; keep it finite.
    Some(Loudness { integrated: integrated.max(-70.0), true_peak: true_peak.max(-100.0) })
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_the_summary() {
        let log = "[Parsed_ebur128_0 @ 0x5] t: 1.0 M: -20.0\n[Parsed_ebur128_0 @ 0x5] Summary:\n\n  Integrated loudness:\n    I:         -14.2 LUFS\n    Threshold: -24.4 LUFS\n\n  Loudness range:\n    LRA:         5.1 LU\n\n  True peak:\n    Peak:       -0.3 dBFS\n";
        let l = super::parse(log).unwrap();
        assert_eq!((l.integrated, l.true_peak), (-14.2, -0.3));
        assert!(super::parse("no summary here").is_none());
    }
}
