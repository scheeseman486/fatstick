//! MPEG audio frames: where the audio is in an MP3 file (between any ID3v2,
//! APEv2, Lyrics3 and ID3v1 tags), and what its frames are. Every frame
//! header is read, because a VBR file's average bitrate says nothing about
//! its loudest frames, and one frame above a player's limit can stop it.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

const BR_V1: [[u32; 15]; 3] = [
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448], // layer I
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],    // layer II
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],     // layer III
];
const BR_V2: [[u32; 15]; 2] = [
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256], // layer I
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],      // layers II and III
];

/// MPEG version: 1, 2, or 25 for MPEG 2.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeader {
    pub version: u8,
    pub layer: u8,
    pub crc: bool,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub padding: bool,
    pub mono: bool,
    pub len: usize,
}

impl FrameHeader {
    pub fn parse(h: &[u8]) -> Option<FrameHeader> {
        if h.len() < 4 || h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
            return None;
        }
        let version = match (h[1] >> 3) & 3 {
            0 => 25,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let layer = match (h[1] >> 1) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let bri = (h[2] >> 4) as usize;
        let sri = ((h[2] >> 2) & 3) as usize;
        if bri == 0 || bri == 15 || sri == 3 {
            return None; // free format or reserved
        }
        let bitrate = if version == 1 {
            BR_V1[(layer - 1) as usize][bri]
        } else {
            BR_V2[if layer == 1 { 0 } else { 1 }][bri]
        };
        let base = [44100, 48000, 32000][sri];
        let sample_rate = match version {
            1 => base,
            2 => base / 2,
            _ => base / 4,
        };
        let padding = (h[2] >> 1) & 1 == 1;
        let len = if layer == 1 {
            ((12 * bitrate * 1000 / sample_rate) as usize + padding as usize) * 4
        } else {
            let k = if layer == 3 && version != 1 { 72 } else { 144 };
            (k * bitrate * 1000 / sample_rate) as usize + padding as usize
        };
        Some(FrameHeader { version, layer, crc: h[1] & 1 == 0, bitrate, sample_rate, padding, mono: h[3] >> 6 == 3, len })
    }

    /// Bytes of side information after the header (and its CRC).
    pub fn side_info_len(&self) -> usize {
        match (self.version == 1, self.mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        }
    }
}

/// What a scan of every frame found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mp3Info {
    /// Where the audio frames start and how many bytes they take; the rest
    /// of the file is tags (or junk) that the copy on the stick leaves out.
    pub audio_start: u64,
    pub audio_len: u64,
    pub frames: u64,
    pub version: u8,
    pub layer: u8,
    pub sample_rate: u32,
    pub channels: u32,
    /// Highest bitrate of any frame, and the average, in kbps.
    pub peak_bitrate: u32,
    pub avg_bitrate: u32,
    /// A Xing or VBRI header, or frames of differing bitrates.
    pub vbr: bool,
    /// Seconds, from the frame count.
    pub duration: f64,
}

fn syncsafe(b: &[u8]) -> u64 {
    b.iter().take(4).fold(0u64, |a, &x| (a << 7) | (x & 0x7F) as u64)
}

/// The byte range between the leading ID3v2 tag(s) and the trailing tags.
pub fn tag_bounds(f: &mut File) -> io::Result<(u64, u64)> {
    let len = f.metadata()?.len();
    let mut start = 0u64;
    let mut h = [0u8; 10];
    loop {
        if start + 10 > len {
            break;
        }
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut h)?;
        if &h[0..3] != b"ID3" || h[3] == 0xFF || h[4] == 0xFF {
            break;
        }
        let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
        start += 10 + syncsafe(&h[6..10]) + footer;
    }
    let mut end = len;
    // ID3v1 (and the "TAG+" extension before it).
    if end >= start + 128 {
        let mut t = [0u8; 3];
        f.seek(SeekFrom::Start(end - 128))?;
        f.read_exact(&mut t)?;
        if &t == b"TAG" {
            end -= 128;
            if end >= start + 227 {
                let mut t4 = [0u8; 4];
                f.seek(SeekFrom::Start(end - 227))?;
                f.read_exact(&mut t4)?;
                if &t4 == b"TAG+" {
                    end -= 227;
                }
            }
        }
    }
    // APEv2 and Lyrics3v2, in either order, before ID3v1.
    for _ in 0..2 {
        if end >= start + 32 {
            let mut footer = [0u8; 32];
            f.seek(SeekFrom::Start(end - 32))?;
            f.read_exact(&mut footer)?;
            if &footer[0..8] == b"APETAGEX" {
                let size = u32::from_le_bytes(footer[12..16].try_into().unwrap()) as u64;
                let flags = u32::from_le_bytes(footer[20..24].try_into().unwrap());
                let header = if flags & 0x8000_0000 != 0 { 32 } else { 0 };
                end = end.saturating_sub(size + header).max(start);
                continue;
            }
        }
        if end >= start + 15 {
            let mut t = [0u8; 15];
            f.seek(SeekFrom::Start(end - 15))?;
            f.read_exact(&mut t)?;
            if &t[6..15] == b"LYRICS200" {
                if let Ok(n) = std::str::from_utf8(&t[0..6]).unwrap_or("").parse::<u64>() {
                    end = end.saturating_sub(15 + n).max(start);
                    continue;
                }
            }
        }
        break;
    }
    Ok((start, end))
}

/// Read every frame header between the tags.
pub fn scan(path: &Path) -> io::Result<Option<Mp3Info>> {
    let mut f = File::open(path)?;
    let (start, end) = tag_bounds(&mut f)?;
    f.seek(SeekFrom::Start(start))?;
    let mut data = Vec::with_capacity((end - start) as usize);
    (&mut f).take(end - start).read_to_end(&mut data)?;
    Ok(scan_bytes(&data).map(|mut i| {
        i.audio_start += start;
        i
    }))
}

/// Scan frames in `data` (the bytes between the tags). The first frame is the
/// first sync whose next frame also parses, so a stray 0xFF in junk doesn't count.
pub fn scan_bytes(data: &[u8]) -> Option<Mp3Info> {
    let at = |p: usize| data.get(p..p + 4).and_then(FrameHeader::parse);
    let mut first = None;
    for p in 0..data.len().min(1 << 20) {
        if let Some(h) = at(p) {
            match at(p + h.len) {
                Some(n) if n.version == h.version && n.layer == h.layer && n.sample_rate == h.sample_rate => {
                    first = Some((p, h));
                    break;
                }
                None if p + h.len == data.len() => {
                    first = Some((p, h));
                    break;
                }
                _ => {}
            }
        }
    }
    let (p0, h0) = first?;
    let mut info = Mp3Info {
        audio_start: p0 as u64,
        version: h0.version,
        layer: h0.layer,
        sample_rate: h0.sample_rate,
        channels: if h0.mono { 1 } else { 2 },
        ..Default::default()
    };
    // Xing/Info and VBRI headers live in the first frame.
    let side = 4 + if h0.crc { 2 } else { 0 } + h0.side_info_len();
    let tag = data.get(p0 + side..p0 + side + 4);
    if tag == Some(b"Xing") || data.get(p0 + 36..p0 + 40) == Some(b"VBRI") {
        info.vbr = true;
    }
    let mut p = p0;
    let mut bytes = 0u64;
    let mut first_rate = 0;
    while let Some(h) = at(p) {
        if h.version != h0.version || h.layer != h0.layer || h.sample_rate != h0.sample_rate || p + h.len > data.len() {
            break;
        }
        info.frames += 1;
        info.peak_bitrate = info.peak_bitrate.max(h.bitrate);
        if first_rate == 0 {
            first_rate = h.bitrate;
        } else if h.bitrate != first_rate && info.frames > 2 {
            info.vbr = true;
        }
        bytes += h.len as u64;
        p += h.len;
    }
    info.audio_len = (p - p0) as u64;
    let spf = match (h0.layer, h0.version) {
        (1, _) => 384.0,
        (2, _) | (3, 1) => 1152.0,
        _ => 576.0,
    };
    info.duration = info.frames as f64 * spf / h0.sample_rate as f64;
    if info.duration > 0.0 {
        info.avg_bitrate = ((bytes as f64 * 8.0 / info.duration) / 1000.0).round() as u32;
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of `kbps` at 44.1 kHz, stereo, filled with zeros after the header.
    pub(crate) fn frame(kbps: u32) -> Vec<u8> {
        let bri = BR_V1[2].iter().position(|&b| b == kbps).unwrap() as u8;
        let h = [0xFF, 0xFB, bri << 4, 0x44];
        let len = FrameHeader::parse(&h).unwrap().len;
        let mut f = vec![0u8; len];
        f[..4].copy_from_slice(&h);
        f
    }

    #[test]
    fn frame_lengths() {
        let h = FrameHeader::parse(&[0xFF, 0xFB, 0x90, 0x44]).unwrap();
        assert_eq!((h.version, h.layer, h.bitrate, h.sample_rate, h.len), (1, 3, 128, 44100, 417));
        let h = FrameHeader::parse(&[0xFF, 0xFB, 0xE0, 0x44]).unwrap();
        assert_eq!((h.bitrate, h.len), (320, 1044));
        // MPEG-2 layer III, 22.05 kHz, 64 kbps: 72 * 64000 / 22050 = 208.
        let h = FrameHeader::parse(&[0xFF, 0xF3, 0x80, 0x44]).unwrap();
        assert_eq!((h.version, h.bitrate, h.sample_rate, h.len), (2, 64, 22050, 208));
        assert!(FrameHeader::parse(&[0xFF, 0xFB, 0x00, 0x44]).is_none(), "free format");
        assert!(FrameHeader::parse(&[0xFF, 0xFB, 0xF0, 0x44]).is_none(), "bad bitrate");
    }

    #[test]
    fn finds_peak_and_tags() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.mp3");
        let mut d = Vec::new();
        // ID3v2 with 20 bytes of payload, then junk, frames, APE, ID3v1.
        d.extend_from_slice(b"ID3\x03\x00\x00\x00\x00\x00\x14");
        d.extend_from_slice(&[0u8; 20]);
        d.extend_from_slice(&[0xFF, 0x00, 0x12]);
        let audio_at = d.len();
        for k in [192, 192, 320, 192, 256] {
            d.extend(frame(k));
        }
        let audio_end = d.len();
        let mut ape = b"APETAGEX".to_vec();
        ape.extend_from_slice(&2000u32.to_le_bytes());
        ape.extend_from_slice(&32u32.to_le_bytes());
        ape.extend_from_slice(&0u32.to_le_bytes());
        ape.extend_from_slice(&0u32.to_le_bytes());
        ape.extend_from_slice(&[0u8; 8]);
        d.extend(&ape);
        let mut v1 = b"TAG".to_vec();
        v1.resize(128, 0);
        d.extend(v1);
        std::fs::write(&p, &d).unwrap();
        let i = scan(&p).unwrap().unwrap();
        assert_eq!(i.audio_start as usize, audio_at);
        assert_eq!(i.audio_len as usize, audio_end - audio_at);
        assert_eq!((i.frames, i.peak_bitrate, i.vbr, i.sample_rate, i.channels), (5, 320, true, 44100, 2));
    }

    #[test]
    fn cbr_is_not_vbr() {
        let mut d = Vec::new();
        for _ in 0..10 {
            d.extend(frame(256));
        }
        let i = scan_bytes(&d).unwrap();
        assert_eq!((i.vbr, i.peak_bitrate, i.avg_bitrate, i.frames), (false, 256, 256, 10));
    }
}
