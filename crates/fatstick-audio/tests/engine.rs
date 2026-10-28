//! The engine against a real ffmpeg. Fixtures are made with the `ffmpeg` on
//! the PATH (which needs FLAC, Vorbis, Opus and PNG encoders, unlike the
//! bundled build); the engine itself uses whatever `Tools::locate` finds.
//! Skipped when no ffmpeg is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use fatstick_audio::encode::EncodeSpec;
use fatstick_audio::prepare::{copy_allowed, prepare, Recipe};
use fatstick_audio::render::{self, RenderPart, RenderSpec, Source};
use fatstick_audio::tagwrite::{write_tags, TagEdit};
use fatstick_audio::tags::{id3v1, id3v2, TagSpec};
use fatstick_audio::{cover, loudness, mp3, probe, Cache, Tools};

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
    let ck: Vec<String> = files.iter().map(|f| fatstick_audio::probe::content_key(f).unwrap()).collect();
    let c = prepare(&t, &cache, &files[0], cbr, &ck[0], &Recipe::Copy).unwrap();
    assert_eq!(c.path, files[0]);
    let m = cbr.mp3.as_ref().unwrap();
    assert!(c.offset > 1000, "starts after the ID3v2 tag with its cover");
    assert_eq!((c.offset, c.len), (m.audio_start, m.audio_len));

    // Encodes: CBR at the cap, every frame; 44.1 or 48 kHz; stereo.
    for (i, cap, want_rate) in [(2usize, 256u32, 44100u32), (3, 320, 48000), (4, 256, 44100), (5, 320, 44100)] {
        let spec = EncodeSpec { cap, vbr: false, sample_rate: fatstick_audio::encode::output_rate(probes[i].sample_rate, &rates), gain_db: 0.0 };
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
    let qk = fatstick_audio::probe::content_key(&quiet).unwrap();
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

/// RMS of interleaved stereo samples between two times.
fn rms(s: &[f32], from: f64, to: f64) -> f64 {
    let a = ((from * 44100.0) as usize * 2).min(s.len());
    let b = ((to * 44100.0) as usize * 2).min(s.len());
    if b <= a {
        return 0.0;
    }
    (s[a..b].iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / (b - a) as f64).sqrt()
}

#[test]
fn render_cuts_pads_and_joins() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-ac", "2", "-c:a", "flac", &p("a.flac").display().to_string()]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=660:duration=2", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "192k", &p("b.mp3").display().to_string()]);
    let src = |n: &str| Some(Source { path: p(n), content_key: probe::content_key(&p(n)).unwrap() });
    let cache = Cache::open(dir.path().join("cache")).unwrap();
    let part = |id: Option<&str>| RenderPart { track_id: id.map(String::from), ..Default::default() };

    // One track, cut at both ends and padded: 3 - 0.5 - 1.0 + 1.0 + 0.5 = 3 s.
    let one = RenderSpec {
        cap: 320,
        vbr: false,
        sample_rate: 44100,
        parts: vec![RenderPart { cut_start: 0.5, cut_end: 1.0, pad_start: 1.0, pad_end: 0.5, ..part(Some("a")) }],
    };
    let out = render::prepare(&t, &cache, &one, &[src("a.flac")]).unwrap();
    let info = mp3::scan(&out.path).unwrap().unwrap();
    assert!((info.duration - 3.0).abs() < 0.08, "3 s: {}", info.duration);
    assert_eq!((info.peak_bitrate, info.vbr), (320, false));
    let s = pcm(&out.path);
    assert!((s.len() as f64 / 88200.0 - 3.0).abs() < 0.08, "decoded length {}", s.len() as f64 / 88200.0);
    assert!(rms(&s, 0.05, 0.9) < 1e-3, "silence first");
    let tone = rms(&s, 1.2, 2.3);
    assert!(tone > 0.05, "then the tone: {tone}");
    assert!(rms(&s, 2.62, 3.0) < 1e-3, "silence last");
    // Done once: asking again finds it in the cache.
    assert_eq!(render::prepare(&t, &cache, &one, &[src("a.flac")]).unwrap().path, out.path);

    // Two tracks joined, with a second of silence between and a gain on the
    // first: 3 + 1 + 2 = 6 s.
    let joined = RenderSpec {
        cap: 256,
        vbr: false,
        sample_rate: 48000,
        parts: vec![RenderPart { gain_db: -6.0, ..part(Some("a")) }, RenderPart { pad_start: 1.0, ..part(None) }, part(Some("b"))],
    };
    let srcs = [src("a.flac"), None, src("b.mp3")];
    let out2 = render::prepare(&t, &cache, &joined, &srcs).unwrap();
    let info2 = mp3::scan(&out2.path).unwrap().unwrap();
    assert!((info2.duration - 6.0).abs() < 0.08, "6 s: {}", info2.duration);
    assert_eq!((info2.sample_rate, info2.peak_bitrate), (48000, 256));
    let s2 = pcm(&out2.path);
    let a = rms(&s2, 0.3, 2.7);
    assert!((a / tone - 0.5).abs() < 0.05, "-6 dB on the first track: {a} vs {tone}");
    assert!(rms(&s2, 3.1, 3.9) < 1e-3, "the silence between");
    assert!(rms(&s2, 4.2, 5.8) > 0.05, "then the second track");
    // The cache key follows the content and the settings.
    assert_ne!(render::cache_key(&one, &[src("a.flac")]), render::cache_key(&RenderSpec { cap: 256, ..one.clone() }, &[src("a.flac")]));
    // Silence alone.
    let gap = RenderSpec { cap: 320, vbr: false, sample_rate: 44100, parts: vec![RenderPart { pad_start: 2.5, ..part(None) }] };
    let g = render::prepare(&t, &cache, &gap, &[None]).unwrap();
    let gi = mp3::scan(&g.path).unwrap().unwrap();
    assert!((gi.duration - 2.5).abs() < 0.08, "2.5 s of silence: {}", gi.duration);
    assert!(rms(&pcm(&g.path), 0.0, 2.5) < 1e-4);
    // A part whose track has no source is refused.
    assert!(render::prepare(&t, &cache, &one, &[None]).is_err());
}

#[test]
fn tags_written_into_library_files() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    let d = |n: &str| p(n).display().to_string();
    ff(&["-f", "lavfi", "-i", "testsrc2=size=300x300:duration=1", "-frames:v", "1", &d("cover.png")]);
    let tone = "sine=frequency=440:duration=2";
    ff(&["-f", "lavfi", "-i", tone, "-i", &d("cover.png"), "-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-b:a", "192k", "-c:v", "png",
        "-disposition:v", "attached_pic", "-id3v2_version", "3", "-write_id3v1", "1", "-metadata", "title=Old Title", "-metadata", "artist=Old Artist",
        "-metadata", "comment=keep me", &d("a.mp3")]);
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "flac", "-metadata", "TITLE=Old", &d("b.flac")]);
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "libopus", "-b:a", "96k", &d("c.opus")]);
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "aac", "-b:a", "128k", &d("d.m4a")]);
    let edit = TagEdit {
        title: Some("Röad Söng".into()),
        artist: Some("Néw Artist".into()),
        album: Some("The Album".into()),
        albumartist: Some("Various".into()),
        genre: Some("Krautrock".into()),
        composer: Some("Someone".into()),
        year: Some("1977".into()),
        track: Some("4/12".into()),
        disc: Some("2".into()),
        labels: Some(vec!["chill".into(), "road trip".into()]),
    };
    for n in ["a.mp3", "b.flac", "c.opus", "d.m4a"] {
        let before = probe(&t, &p(n)).unwrap();
        write_tags(&p(n), &edit).unwrap();
        let after = probe(&t, &p(n)).unwrap();
        let tg = &after.tags;
        assert_eq!((tg.title.as_str(), tg.artist.as_str(), tg.album.as_str(), tg.albumartist.as_str()), ("Röad Söng", "Néw Artist", "The Album", "Various"), "{n}: {tg:?}");
        assert_eq!((tg.genre.as_str(), tg.composer.as_str(), tg.year, tg.track, tg.disc), ("Krautrock", "Someone", Some(1977), Some(4), Some(2)), "{n}: {tg:?}");
        assert_eq!(tg.labels, ["chill", "road trip"], "{n}");
        assert_eq!((after.codec.as_str(), after.cover), (before.codec.as_str(), before.cover), "{n}: same audio, same cover");
        assert!((after.duration - before.duration).abs() < 0.05, "{n}");
    }
    // The MP3: ID3v2.3, the audio frames untouched, the ID3v1 tag updated, other fields kept.
    let mp3 = std::fs::read(p("a.mp3")).unwrap();
    assert!(mp3.starts_with(b"ID3\x03"), "ID3v2.3");
    assert_eq!(&mp3[mp3.len() - 128..mp3.len() - 125], b"TAG");
    assert!(mp3[mp3.len() - 125..mp3.len() - 95].starts_with(b"R"), "ID3v1 title updated");
    let j = String::from_utf8(Command::new("ffprobe").args(["-v", "error", "-show_entries", "format_tags=comment", "-of", "json"]).arg(p("a.mp3")).output().unwrap().stdout).unwrap();
    assert!(j.contains("keep me"), "other fields are kept: {j}");
    // Clearing a field, and leaving the rest.
    write_tags(&p("b.flac"), &TagEdit { genre: Some(String::new()), year: Some(String::new()), labels: Some(vec![]), ..Default::default() }).unwrap();
    let b = probe(&t, &p("b.flac")).unwrap().tags;
    assert_eq!((b.genre.as_str(), b.year, b.labels.len(), b.title.as_str()), ("", None, 0, "Röad Söng"));
    // Nonsense numbers are refused before anything is written.
    assert!(write_tags(&p("b.flac"), &TagEdit { track: Some("zero".into()), ..Default::default() }).is_err());

    // An MP3 whose labels another app kept elsewhere: ffmpeg in TXXX:grouping,
    // Apple in GRP1. They all get the new labels, so no player shows old ones.
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "libmp3lame", "-b:a", "128k", "-id3v2_version", "3", "-metadata", "grouping=chill; road trip", &d("e.mp3")]);
    {
        use lofty::config::{ParseOptions, WriteOptions};
        use lofty::file::AudioFile;
        use lofty::id3::v2::{Frame, FrameId, TextInformationFrame};
        use lofty::tag::TagExt;
        let mut f = std::fs::File::open(p("e.mp3")).unwrap();
        let mut tag = lofty::mpeg::MpegFile::read_from(&mut f, ParseOptions::new()).unwrap().id3v2().cloned().unwrap();
        drop(f);
        tag.insert(Frame::Text(TextInformationFrame::new(FrameId::Valid("GRP1".into()), lofty::TextEncoding::UTF16, "chill; road trip".to_string())));
        tag.save_to_path(p("e.mp3"), WriteOptions::default()).unwrap();
    }
    assert_eq!(probe(&t, &p("e.mp3")).unwrap().tags.labels, ["chill", "road trip"]);
    write_tags(&p("e.mp3"), &TagEdit { labels: Some(vec!["chill".into(), "kitchen".into()]), ..Default::default() }).unwrap();
    assert_eq!(probe(&t, &p("e.mp3")).unwrap().tags.labels, ["chill", "kitchen"]);
    let frames = || {
        use lofty::config::ParseOptions;
        use lofty::file::AudioFile;
        let mut f = std::fs::File::open(p("e.mp3")).unwrap();
        let tag = lofty::mpeg::MpegFile::read_from(&mut f, ParseOptions::new()).unwrap().id3v2().cloned().unwrap();
        let text = |id: &str| tag.get_text(&lofty::id3::v2::FrameId::Valid(id.to_string().into())).map(str::to_string);
        (text("TIT1"), text("GRP1"), tag.get_user_text("grouping").map(str::to_string))
    };
    let want = Some("chill; kitchen".to_string());
    assert_eq!(frames(), (want.clone(), want.clone(), want));
    write_tags(&p("e.mp3"), &TagEdit { labels: Some(vec![]), ..Default::default() }).unwrap();
    assert_eq!(frames(), (None, None, None), "no labels: every Grouping frame goes");
    assert!(probe(&t, &p("e.mp3")).unwrap().tags.labels.is_empty());
}

/// The music folder's quick read against the full one: the same tags and
/// stream facts, a VBR file's peak at the most its frames could be, and no
/// frame list (the full read before a sync makes one).
#[test]
fn quick_probe_matches_the_full_one() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    let d = |n: &str| p(n).display().to_string();
    ff(&["-f", "lavfi", "-i", NOISE, "-c:a", "libmp3lame", "-q:a", "2", "-id3v2_version", "3", "-metadata", "title=Vee", "-metadata", "grouping=a; b", &d("vbr.mp3")]);
    ff(&["-f", "lavfi", "-i", NOISE, "-c:a", "libmp3lame", "-b:a", "192k", "-metadata", "title=Cee", &d("cbr.mp3")]);
    ff(&["-f", "lavfi", "-i", NOISE, "-c:a", "flac", "-metadata", "TITLE=Eff", &d("x.flac")]);
    for n in ["vbr.mp3", "cbr.mp3", "x.flac"] {
        let full = probe::probe(&t, &p(n)).unwrap();
        let quick = probe::probe_quick(&t, &p(n)).unwrap();
        assert_eq!((&quick.codec, &quick.tags, quick.sample_rate, quick.channels, quick.vbr), (&full.codec, &full.tags, full.sample_rate, full.channels, full.vbr), "{n}");
        assert!((quick.duration - full.duration).abs() < 0.1, "{n}: {} vs {}", quick.duration, full.duration);
        assert!(quick.bitrate.abs_diff(full.bitrate) <= 2, "{n}: {} vs {}", quick.bitrate, full.bitrate);
        assert!(quick.peak_bitrate >= full.peak_bitrate, "{n}: never under the real peak");
        if n.ends_with(".mp3") {
            assert!(quick.quick && quick.mp3.is_none() && !full.quick && full.mp3.is_some(), "{n}");
        } else {
            assert!(!quick.quick, "{n}: only MP3s have frames to leave for later");
        }
    }
    let quick = probe::probe_quick(&t, &p("cbr.mp3")).unwrap();
    assert_eq!((quick.vbr, quick.peak_bitrate, quick.bitrate), (false, 192, 192));
    assert!(probe::probe_quick(&t, &p("vbr.mp3")).unwrap().vbr);
}

/// Upward zero crossings per second of the left channel, from `from` to `to` seconds.
fn hz(s: &[f32], from: f64, to: f64) -> f64 {
    let left: Vec<f32> = s.iter().step_by(2).copied().collect();
    let (a, b) = ((from * 44100.0) as usize, ((to * 44100.0) as usize).min(left.len()));
    let ups = (a.max(1)..b).filter(|&i| left[i - 1] < 0.0 && left[i] >= 0.0).count();
    ups as f64 / ((b - a) as f64 / 44100.0)
}

/// A key shift on the stick: the tone an octave up (and a fifth down), the
/// same length, cut first, and its own cache entry.
#[test]
fn render_shifts_the_key() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=6", "-ac", "2", "-c:a", "flac", &p("a.flac").display().to_string()]);
    let src = [Some(Source { path: p("a.flac"), content_key: probe::content_key(&p("a.flac")).unwrap() })];
    let cache = Cache::open(dir.path().join("cache")).unwrap();
    let with = |pitch: f64, keep_formants: bool| RenderSpec {
        cap: 320,
        vbr: false,
        sample_rate: 44100,
        parts: vec![RenderPart { track_id: Some("a".into()), cut_start: 1.0, pad_end: 0.5, pitch, keep_formants, ..Default::default() }],
    };
    let spec = |pitch: f64| with(pitch, false);
    for (pitch, want, formants) in [(12.0, 880.0, false), (-7.0, 440.0 * 2f64.powf(-7.0 / 12.0), false), (3.0, 440.0 * 2f64.powf(3.0 / 12.0), true)] {
        let out = render::prepare(&t, &cache, &with(pitch, formants), &src).unwrap();
        let info = mp3::scan(&out.path).unwrap().unwrap();
        assert!((info.duration - 5.5).abs() < 0.08, "{pitch:+}: 6 - 1 + 0.5 = 5.5 s: {}", info.duration);
        let s = pcm(&out.path);
        let f = hz(&s, 1.0, 4.0);
        assert!((f - want).abs() / want < 0.01, "{pitch:+} semitones: {f} Hz, wanted {want}");
        // A lone sine is where keeping formants changes the level most: checked without.
        let (was, now) = (rms(&pcm(&p("a.flac")), 2.0, 5.0), rms(&s, 1.0, 4.0));
        assert!(formants || (now / was - 1.0).abs() < 0.15, "as loud as before: {now} vs {was}");
        assert!(rms(&s, 5.1, 5.45) < 1e-3, "then the silence");
    }
    assert_ne!(render::cache_key(&spec(0.0), &src), render::cache_key(&spec(2.0), &src));
    assert_ne!(render::cache_key(&with(2.0, true), &src), render::cache_key(&with(2.0, false), &src));
    // No shift: the key is what it was before shifting existed.
    assert!(!spec(0.0).recipe(&src).contains("key"));
}

/// Fades on the stick: after the cut, the S curve in and out, the same length,
/// with or without a key shift.
#[test]
fn render_fades_in_and_out() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=8", "-ac", "2", "-c:a", "flac", &p("a.flac").display().to_string()]);
    let src = [Some(Source { path: p("a.flac"), content_key: probe::content_key(&p("a.flac")).unwrap() })];
    let cache = Cache::open(dir.path().join("cache")).unwrap();
    let spec = |pitch: f64| RenderSpec {
        cap: 320,
        vbr: false,
        sample_rate: 44100,
        parts: vec![RenderPart { track_id: Some("a".into()), cut_start: 1.0, pad_end: 0.5, fade_in: 2.0, fade_out: 3.0, pitch, ..Default::default() }],
    };
    let full = rms(&pcm(&p("a.flac")), 3.0, 5.0);
    for pitch in [0.0, 4.0] {
        let out = render::prepare(&t, &cache, &spec(pitch), &src).unwrap();
        let info = mp3::scan(&out.path).unwrap().unwrap();
        assert!((info.duration - 7.5).abs() < 0.08, "8 - 1 + 0.5 = 7.5 s: {}", info.duration);
        let s = pcm(&out.path);
        let at = |x: f64| rms(&s, x - 0.05, x + 0.05) / full;
        // In: silent, half way at 1 s (the S curve's middle), full from 2 s.
        assert!(at(0.06) < 0.1, "{pitch}: starts silent: {}", at(0.06));
        assert!((at(1.0) - 0.5).abs() < 0.12, "{pitch}: half way up at 1 s: {}", at(1.0));
        assert!((at(3.0) - 1.0).abs() < 0.1, "{pitch}: full between: {}", at(3.0));
        // Out over the last 3 s of the cut track (4 s to 7 s): half way at 5.5 s, nothing at 7 s.
        assert!((at(5.5) - 0.5).abs() < 0.12, "{pitch}: half way down at 5.5 s: {}", at(5.5));
        assert!(at(6.95) < 0.1, "{pitch}: ends silent: {}", at(6.95));
        assert!(rms(&s, 7.05, 7.45) < 1e-3, "{pitch}: then the silence");
    }
    let plain = RenderSpec { parts: vec![RenderPart { fade_in: 0.0, fade_out: 0.0, ..spec(0.0).parts[0].clone() }], ..spec(0.0) };
    assert_ne!(render::cache_key(&spec(0.0), &src), render::cache_key(&plain, &src));
    assert!(!plain.recipe(&src).contains("fade"));
    assert!(render::fade_curve(0.0) == 0.0 && (render::fade_curve(0.5) - 0.5).abs() < 1e-12 && render::fade_curve(1.0) == 1.0);
}

#[test]
fn peaks_draw_the_loud_and_the_quiet() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("half.flac");
    // A second of silence, then a second at half scale.
    ff(&["-f", "lavfi", "-i", "aevalsrc=if(lt(t\\,1)\\,0\\,0.5*sin(2*PI*440*t))|if(lt(t\\,1)\\,0\\,0.5*sin(2*PI*440*t)):s=44100:d=2", "-c:a", "flac", &f.display().to_string()]);
    let p = fatstick_audio::decode::peaks(&t, &f, 0.05).unwrap();
    assert!((39..=41).contains(&p.len()), "one per 50 ms: {}", p.len());
    assert!(p[..19].iter().all(|&x| x == 0), "silent first: {:?}", &p[..19]);
    assert!(p[21..].iter().all(|&x| (120..=135).contains(&x)), "then half scale: {:?}", &p[21..]);
}
