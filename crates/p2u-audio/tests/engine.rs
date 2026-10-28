//! The engine against a real ffmpeg. Fixtures are made with the `ffmpeg` on
//! the PATH (which needs FLAC, Vorbis, Opus and PNG encoders, unlike the
//! bundled build); the engine itself uses whatever `Tools::locate` finds.
//! Skipped when no ffmpeg is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use p2u_audio::encode::EncodeSpec;
use p2u_audio::prepare::{copy_allowed, prepare, Recipe};
use p2u_audio::tags::{id3v1, id3v2, TagSpec};
use p2u_audio::{cover, loudness, mp3, probe, Cache, Tools};

fn ff(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

fn tools() -> Option<Tools> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipped: no ffmpeg on the PATH");
        return None;
    }
    Some(Tools::locate().expect("ffmpeg and ffprobe"))
}

/// Independent stereo noise: hard on a VBR encoder.
const NOISE: &str = "aevalsrc=0.8*random(0)-0.4|0.8*random(1)-0.4:d=6:s=44100";

fn fixtures(dir: &Path) -> Vec<PathBuf> {
    let p = |n: &str| dir.join(n).display().to_string();
    ff(&["-f", "lavfi", "-i", "testsrc2=size=800x800:duration=1", "-frames:v", "1", &p("cover.png")]);
    ff(&["-f", "lavfi", "-i", NOISE, "-i", &p("cover.png"), "-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-b:a", "320k",
        "-c:v", "png", "-disposition:v", "attached_pic", "-id3v2_version", "3", "-metadata", "title=Röad Tést", "-metadata", "artist=Fixture Band",
        "-metadata", "album=Fixtures", "-metadata", "track=3/12", "-metadata", "date=1999", &p("cbr320.mp3")]);
    ff(&["-f", "lavfi", "-i", NOISE, "-c:a", "libmp3lame", "-q:a", "0", &p("vbr.mp3")]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=96000:duration=4", "-i", &p("cover.png"), "-map", "0:a", "-map", "1:v",
        "-ac", "2", "-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24", "-c:v", "png", "-disposition:v", "attached_pic",
        "-metadata", "TITLE=Lossless", "-metadata", "ALBUMARTIST=Various", &p("hires.flac")]);
    ff(&["-f", "lavfi", "-i", NOISE, "-ac", "2", "-c:a", "libopus", "-b:a", "128k", "-metadata", "title=Opus One", &p("one.opus")]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=330:duration=3", "-ac", "6", "-c:a", "pcm_s16le", &p("surround.wav")]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=550:sample_rate=22050:duration=3", "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k", &p("mpeg2.mp3")]);
    ["cbr320.mp3", "vbr.mp3", "hires.flac", "one.opus", "surround.wav", "mpeg2.mp3"].iter().map(|n| dir.join(n)).collect()
}

/// Decoded samples (f32, stereo, 44.1 kHz).
fn pcm(path: &Path) -> Vec<f32> {
    let o = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-f", "f32le", "-ac", "2", "-ar", "44100", "-"]).output().unwrap();
    o.stdout.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

#[test]
fn engine_end_to_end() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let files = fixtures(dir.path());
    let cache = Cache::open(dir.path().join("cache")).unwrap();
    let probes: Vec<_> = files.iter().map(|f| probe(&t, f).unwrap()).collect();
    let [cbr, vbr, flac, opus, wav, mpeg2] = &probes[..] else { unreachable!() };

    // Probing.
    assert_eq!((cbr.codec.as_str(), cbr.bitrate, cbr.peak_bitrate, cbr.vbr, cbr.sample_rate), ("mp3", 320, 320, false, 44100));
    assert_eq!((cbr.tags.title.as_str(), cbr.tags.artist.as_str(), cbr.tags.track, cbr.tags.year), ("Röad Tést", "Fixture Band", Some(3), Some(1999)));
    assert!(cbr.cover && flac.cover && !opus.cover);
    assert!(vbr.vbr && vbr.peak_bitrate > 192 && vbr.bitrate < vbr.peak_bitrate, "V0 frames vary, and peak past 192 kbps: {vbr:?}");
    assert_eq!((flac.codec.as_str(), flac.sample_rate, flac.bits, flac.tags.albumartist.as_str()), ("flac", 96000, 24, "Various"));
    assert_eq!((opus.codec.as_str(), opus.sample_rate, opus.tags.title.as_str()), ("opus", 48000, "Opus One"));
    assert_eq!((wav.codec.as_str(), wav.channels), ("wav", 6));
    assert_eq!((mpeg2.codec.as_str(), mpeg2.sample_rate, mpeg2.mp3.as_ref().unwrap().version), ("mp3", 22050, 2));

    // What may be copied as it is.
    let rates = [44100, 48000];
    assert!(copy_allowed(cbr, 320, &rates));
    assert!(!copy_allowed(cbr, 256, &rates), "320 frames on a 256 stick");
    assert!(!copy_allowed(vbr, 192, &rates), "VBR peaks over the cap");
    assert!(copy_allowed(vbr, 320, &rates));
    assert!(!copy_allowed(mpeg2, 320, &rates), "22.05 kHz isn't in the generic profile");
    assert!(!copy_allowed(flac, 320, &rates));

    // A plain copy is the source's audio frames, without its tags.
    let ck: Vec<String> = files.iter().map(|f| p2u_audio::probe::content_key(f).unwrap()).collect();
    let c = prepare(&t, &cache, &files[0], cbr, &ck[0], &Recipe::Copy).unwrap();
    assert_eq!(c.path, files[0]);
    let m = cbr.mp3.as_ref().unwrap();
    assert!(c.offset > 1000, "starts after the ID3v2 tag with its cover");
    assert_eq!((c.offset, c.len), (m.audio_start, m.audio_len));

    // Encodes: CBR at the cap, every frame; 44.1 or 48 kHz; stereo.
    for (i, cap, want_rate) in [(2usize, 256u32, 44100u32), (3, 320, 48000), (4, 256, 44100), (5, 320, 44100)] {
        let spec = EncodeSpec { cap, vbr: false, sample_rate: p2u_audio::encode::output_rate(probes[i].sample_rate, &rates), gain_db: 0.0 };
        let p = prepare(&t, &cache, &files[i], &probes[i], &ck[i], &Recipe::Encode(spec)).unwrap();
        let info = mp3::scan(&p.path).unwrap().unwrap();
        assert_eq!((info.peak_bitrate, info.vbr, info.sample_rate, info.channels), (cap, false, want_rate, 2), "{}", files[i].display());
        assert_eq!(p.offset, 0);
        assert!(std::fs::read(&p.path).unwrap().starts_with(&[0xFF]), "no ID3 tag in the cache");
    }
    // VBR asked for, with a cap its peaks would break: done again as CBR.
    let spec = EncodeSpec { cap: 192, vbr: true, sample_rate: 44100, gain_db: 0.0 };
    let p = prepare(&t, &cache, &files[1], vbr, &ck[1], &Recipe::Encode(spec)).unwrap();
    let again = mp3::scan(&p.path).unwrap().unwrap();
    assert_eq!((again.peak_bitrate, again.vbr), (192, false));

    // Lossless gain: +2 steps scale every decoded sample by 2^(2/4) = √2.
    let quiet = dir.path().join("quiet.mp3");
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-af", "volume=-12dB", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "192k", &quiet.display().to_string()]);
    let qp = probe(&t, &quiet).unwrap();
    let qk = p2u_audio::probe::content_key(&quiet).unwrap();
    let g = prepare(&t, &cache, &quiet, &qp, &qk, &Recipe::Gain { steps: 2 }).unwrap();
    let (a, b) = (pcm(&quiet), pcm(&g.path));
    assert_eq!(a.len(), b.len());
    let rms = |v: &[f32]| (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
    let ratio = rms(&b) / rms(&a);
    assert!((ratio - 2f64.sqrt()).abs() < 0.002, "ratio {ratio}");
    assert_eq!(mp3::scan(&g.path).unwrap().unwrap().frames, qp.mp3.unwrap().frames);

    // Exact gain on an encode, and loudness that moves with it.
    let l0 = loudness::measure(&t, &files[2]).unwrap();
    let spec = EncodeSpec { cap: 320, vbr: false, sample_rate: 44100, gain_db: -6.0 };
    let e = prepare(&t, &cache, &files[2], flac, &ck[2], &Recipe::Encode(spec)).unwrap();
    let l1 = loudness::measure(&t, &e.path).unwrap();
    assert!((l0.integrated - 6.0 - l1.integrated).abs() < 0.3, "{l0:?} {l1:?}");

    // Covers: the 800 px PNG becomes a baseline JPEG within the limits.
    let pic = cover::extract(&t, &files[0], &cache.temp("cover")).unwrap().expect("embedded picture");
    assert!(pic.starts_with(b"\x89PNG"));
    let jpeg = cover::for_stick(&pic).unwrap();
    assert!(jpeg.len() <= cover::MAX_BYTES && jpeg.starts_with(&[0xFF, 0xD8]));
    assert!(cover::extract(&t, &files[3], &cache.temp("cover")).unwrap().is_none());

    // A whole file as the stick gets it: ID3v2.3 + prepared audio + ID3v1.
    let spec = TagSpec { title: "Röad Tést".into(), artist: "Fixture Band".into(), track: Some(7), track_total: Some(21), cover_jpeg: Some(jpeg), ..Default::default() };
    let mut whole = id3v2(&spec, false);
    let audio = std::fs::read(&c.path).unwrap();
    whole.extend_from_slice(&audio[c.offset as usize..(c.offset + c.len) as usize]);
    whole.extend_from_slice(&id3v1(&spec));
    let out = dir.path().join("assembled.mp3");
    std::fs::write(&out, &whole).unwrap();
    let back = probe(&t, &out).unwrap();
    assert_eq!((back.tags.title.as_str(), back.tags.track, back.cover, back.peak_bitrate), ("Röad Tést", Some(7), true, 320));
    assert_eq!(back.mp3.unwrap().frames, m.frames);

    // Six encodes and one gain copy in the cache, which clears on request.
    assert_eq!(cache.stats().files, 7);
    cache.clear();
    assert_eq!(cache.stats().files, 0);
}
