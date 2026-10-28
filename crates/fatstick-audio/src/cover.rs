//! Front covers for the stick: one baseline JPEG, at most 500 × 500 px and
//! 100 KB. Owners report head units and Walkmans that show nothing for
//! progressive JPEGs, large images or large files.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageReader};

use crate::tools::{run, Tools};
use crate::Result;

pub const MAX_SIDE: u32 = 500;
pub const MAX_BYTES: usize = 100 * 1024;

/// The first embedded picture's bytes, as stored (JPEG, PNG, ...), if any.
/// ffmpeg exposes embedded pictures (ID3 APIC, FLAC and Ogg pictures, MP4
/// covr, WM/Picture) as an attached-picture stream. ffprobe dumps that
/// stream's one packet in hex without decoding it, so no image decoder is
/// needed in the bundled build. (`_tmp` is unused; kept for the API.)
pub fn extract(tools: &Tools, src: &Path, _tmp: &Path) -> Result<Option<Vec<u8>>> {
    let r = run(tools
        .ffprobe()
        .args(["-v", "error", "-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "packet=data", "-show_data", "-of", "json"])
        .arg(src));
    let Ok(o) = r else { return Ok(None) };
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_default();
    let Some(dump) = j["packets"][0]["data"].as_str() else { return Ok(None) };
    let bytes = parse_hexdump(dump);
    Ok((!bytes.is_empty()).then_some(bytes))
}

/// ffprobe's `-show_data` format: "00000000: 8950 4e47 0d0a ...  .PNG....".
fn parse_hexdump(dump: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in dump.lines() {
        let Some((_, rest)) = line.trim_start().split_once(": ") else { continue };
        let hex = rest.split("  ").next().unwrap_or("");
        let digits: Vec<u8> = hex.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
        for pair in digits.chunks_exact(2) {
            let v = |c: u8| (c as char).to_digit(16).unwrap() as u8;
            out.push(v(pair[0]) << 4 | v(pair[1]));
        }
    }
    out
}

/// Baseline (SOF0/SOF1) rather than progressive (SOF2) JPEG.
fn is_baseline_jpeg(b: &[u8]) -> bool {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return false;
    }
    let mut p = 2;
    while p + 4 <= b.len() {
        if b[p] != 0xFF {
            return false;
        }
        let m = b[p + 1];
        match m {
            0xC0 | 0xC1 => return true,
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return false,
            0xD8 | 0x01 | 0xD0..=0xD7 => p += 2,
            _ => p += 2 + u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize,
        }
    }
    false
}

/// Make a picture fit the rules. A baseline JPEG already within them is kept
/// byte for byte; anything else is decoded, scaled down and re-encoded.
pub fn for_stick(picture: &[u8]) -> Option<Vec<u8>> {
    let img = ImageReader::new(Cursor::new(picture)).with_guessed_format().ok()?.decode().ok()?;
    let (w, h) = img.dimensions();
    if is_baseline_jpeg(picture) && w <= MAX_SIDE && h <= MAX_SIDE && picture.len() <= MAX_BYTES {
        return Some(picture.to_vec());
    }
    let mut side = MAX_SIDE;
    loop {
        let scaled = if w > side || h > side { img.resize(side, side, FilterType::Lanczos3) } else { img.clone() };
        let rgb = DynamicImage::ImageRgb8(scaled.to_rgb8());
        for q in [90u8, 84, 78, 70, 62, 54, 46, 40] {
            let mut out = Vec::new();
            if JpegEncoder::new_with_quality(&mut out, q).encode_image(&rgb).is_err() {
                return None;
            }
            if out.len() <= MAX_BYTES {
                return Some(out);
            }
        }
        if side <= 200 {
            return None;
        }
        side -= 100;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn noisy_png(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| Rgb([(x * 7 + y * 13) as u8, (x ^ y) as u8, ((x * y) % 251) as u8]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn big_png_becomes_a_small_baseline_jpeg() {
        let out = for_stick(&noisy_png(1400, 1000)).unwrap();
        assert!(is_baseline_jpeg(&out));
        assert!(out.len() <= MAX_BYTES);
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!(img.dimensions(), (500, 357));
    }

    #[test]
    fn hexdump() {
        let d = "\n00000000: 8950 4e47 0d0a 1a0a 0000 000d 4948 4452  .PNG........IHDR\n00000010: 0000 02                                ...\n";
        assert_eq!(parse_hexdump(d), [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 2]);
    }

    #[test]
    fn a_fitting_baseline_jpeg_is_kept() {
        let small = for_stick(&noisy_png(300, 300)).unwrap();
        assert_eq!(for_stick(&small).unwrap(), small);
    }
}
