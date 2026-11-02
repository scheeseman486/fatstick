//! Search, fetch and keep with `tests/fake_ytdlp.py` in yt-dlp's place, and the
//! real ffmpeg: what arrives, and what's kept, is audio alone.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fatstick_audio::Tools;
use fatstick_youtube::{audio_alone, keep, Store, Ytdlp};

fn fake() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fake_ytdlp.py")
}

fn setup() -> (tempfile::TempDir, Ytdlp, Store, Tools) {
    let d = tempfile::tempdir().unwrap();
    let yt = Ytdlp { path: fake(), version: "2026.10.26".into(), deno: None, cache_dir: d.path().join("yc") };
    let store = Store::new(d.path());
    (d, yt, store, Tools::locate().expect("ffmpeg"))
}

fn tag(path: &Path, name: &str) -> String {
    let o = Command::new("ffprobe").args(["-v", "error", "-show_entries", &format!("format_tags={name}:stream_tags={name}"), "-of", "default=nw=1:nk=1"]).arg(path).output().unwrap();
    String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string()
}

#[test]
fn searches() {
    let (_d, yt, ..) = setup();
    let h = yt.search("anything", 2).unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].id, "aaaaaaaaaa1");
    assert_eq!(h[0].channel, "The Band VEVO");
    assert!(yt.search("nothing", 5).unwrap().is_empty());
    assert_eq!(yt.look_up("aaaaaaaaaa2").unwrap().title, "Second Song (Lyrics)");
    assert!(yt.look_up("zzzzzzzzzzz").is_err());
    assert!(yt.look_up("not an id").is_err());
}

#[test]
fn fetches_audio_into_the_store_and_keeps_it_tagged() {
    let (d, yt, store, tools) = setup();
    let no = AtomicBool::new(false);
    let mut seen = Vec::new();
    let f = yt.fetch(&store, Some(&tools), "aaaaaaaaaa1", &no, &mut |p| seen.push(p.done)).unwrap();
    assert!(!seen.is_empty(), "progress arrives");
    let path = store.path_of(&f);
    assert_eq!(path.extension().unwrap(), if tools.can_mux("opus") { "opus" } else { "m4a" });
    audio_alone(&tools, &path).unwrap();
    assert_eq!((f.tags.artist.as_str(), f.tags.title.as_str(), f.tags.year), ("The Band", "First Song", None));
    assert_eq!(std::fs::read_dir(&store.dir).unwrap().count(), 2, "the song and what it is, nothing else");

    // A second time it's there already: yt-dlp isn't asked.
    let broken = Ytdlp { path: "/nonexistent".into(), ..yt.clone() };
    assert_eq!(broken.fetch(&store, Some(&tools), "aaaaaaaaaa1", &no, &mut |_| {}).unwrap(), f);

    // Kept: under a name from its tags, tagged, the source noted.
    let music = d.path().join("Music/YouTube");
    let kept = keep(&store, &f, &music).unwrap();
    assert_eq!(kept.file_stem().unwrap(), "The Band - First Song");
    assert_eq!(tag(&kept, "title"), "First Song");
    assert_eq!(tag(&kept, "artist"), "The Band");
    assert_eq!(tag(&kept, "comment"), "https://www.youtube.com/watch?v=aaaaaaaaaa1");
    audio_alone(&tools, &kept).unwrap();
    assert_eq!(std::fs::read_dir(&music).unwrap().count(), 1, "no working file left behind");

    // Never over another file.
    let again = keep(&store, &f, &music).unwrap();
    assert_ne!(again, kept);
    assert_eq!(again.file_stem().unwrap(), "The Band - First Song [aaaaaaaaaa1]");
    assert!(kept.is_file());
}

#[test]
fn an_ffmpeg_without_ogg_gets_aac_in_a_plain_m4a() {
    let (d, yt, store, tools) = setup();
    let no = AtomicBool::new(false);
    let f = yt.fetch_as(&store, Some(&tools), "aaaaaaaaaa2", false, &no, &mut |_| {}).unwrap();
    assert_eq!(f.file, "aaaaaaaaaa2.m4a");
    audio_alone(&tools, &store.path_of(&f)).unwrap();
    let kept = keep(&store, &f, &d.path().join("m")).unwrap();
    assert_eq!(kept.extension().unwrap(), "m4a");
    assert_eq!(tag(&kept, "title"), "Second Song");
    assert_eq!(tag(&kept, "artist"), "Some Channel");
    let o = Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=codec_name:format=duration", "-of", "default=nw=1:nk=1"]).arg(&kept).output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("aac"), "{text}");
    assert!(text.lines().last().unwrap().parse::<f64>().unwrap() > 4.0, "{text}");
}

#[test]
fn what_youtube_knows_goes_in_the_tags() {
    let (d, yt, store, tools) = setup();
    let no = AtomicBool::new(false);
    let f = yt.fetch(&store, Some(&tools), "aaaaaaaaaa3", &no, &mut |_| {}).unwrap();
    let kept = keep(&store, &f, &d.path().join("m")).unwrap();
    assert_eq!(tag(&kept, "title"), "Third Song Proper");
    assert_eq!(tag(&kept, "album"), "The Album");
    assert!(tag(&kept, "date").starts_with("1975"));
}

#[test]
fn a_video_with_no_audio_stream_is_a_failure_not_a_video() {
    let (_d, yt, store, tools) = setup();
    let no = AtomicBool::new(false);
    let e = yt.fetch(&store, Some(&tools), "noaudiostrm", &no, &mut |_| {}).unwrap_err();
    assert!(e.contains("no audio-only stream"), "{e}");
    assert!(store.get("noaudiostrm").is_none());
    let e = yt.fetch(&store, Some(&tools), "blockedsong", &no, &mut |_| {}).unwrap_err();
    assert!(e.contains("Updating yt-dlp"), "{e}");
}

#[test]
fn a_file_with_video_in_it_is_thrown_away_or_stripped() {
    let (_d, yt, store, tools) = setup();
    let no = AtomicBool::new(false);
    // Whatever arrives, only its audio can stay.
    if let Ok(f) = yt.fetch(&store, Some(&tools), "hasvideo000", &no, &mut |_| {}) {
        audio_alone(&tools, &store.path_of(&f)).unwrap();
    } else {
        assert!(store.get("hasvideo000").is_none());
        assert!(store.fetched_file("hasvideo000").is_none());
    }
}

#[test]
fn audio_alone_sees_video() {
    let d = tempfile::tempdir().unwrap();
    let tools = Tools::locate().expect("ffmpeg");
    let p = d.path().join("v.webm");
    let ok = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=duration=1", "-f", "lavfi", "-i", "color=c=black:s=64x64:d=1", "-c:a", "libopus", "-c:v", "libvpx-vp9", "-shortest"]).arg(&p).status().unwrap();
    assert!(ok.success());
    // The bundled ffmpeg is audio-only, and can't read a video stream it has no
    // decoder for; ffprobe still lists it.
    let e = audio_alone(&tools, &p).unwrap_err();
    assert!(e.contains("video"), "{e}");
}

#[test]
fn cancelling_stops_the_download() {
    let (_d, yt, store, tools) = setup();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let t0 = std::time::Instant::now();
    let e = yt.fetch(&store, Some(&tools), "slowsongslo", &cancel, &mut |_| c2.store(true, Ordering::SeqCst)).unwrap_err();
    assert_eq!(e, "Cancelled.");
    assert!(t0.elapsed().as_secs() < 8);
    assert!(store.get("slowsongslo").is_none());
}
