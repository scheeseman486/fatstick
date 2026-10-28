//! The sync, end to end, on a disk image standing in for a stick: real audio
//! made with the `ffmpeg` on the PATH, read into a library, prepared, tagged,
//! written in two folders and read back the way a player reads it.
//! Skipped when ffmpeg isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use fatstick_audio::encode::EncodeSpec;
use fatstick_audio::prepare::Recipe;
use fatstick_audio::render::{RenderPart, RenderSpec};
use fatstick_audio::{mp3, probe, Cache, Tools};
use fatstick_core::fat::{DirRef, EntryKind, Volume};
use fatstick_library::{scan, Library};

use crate::sync::{run, FilePlan, FolderPlan, ProfilePlan, SyncPlan, SyncProgress};

fn ff(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

/// Every file on a FAT32 image: folder name ("" at the top), file name, bytes.
fn read_back(img: &Path) -> Vec<(String, String, Vec<u8>)> {
    let f = std::fs::File::open(img).unwrap();
    let mut vol = Volume::open(f).unwrap();
    let root = vol.list_dir(DirRef::Root).unwrap();
    let mut out = Vec::new();
    let mut dirs = vec![(String::new(), root.clone())];
    for g in root.groups.iter().filter(|g| g.kind == EntryKind::Dir) {
        let d = vol.find_dir(g.name()).unwrap();
        dirs.push((g.name().to_string(), vol.list_dir(d).unwrap()));
    }
    let cb = vol.geometry().cluster_bytes();
    for (dir, listing) in dirs {
        for g in listing.groups.iter().filter(|g| g.kind == EntryKind::File && !g.is_hidden()) {
            let mut data = Vec::new();
            if g.first_cluster >= 2 {
                for c in vol.cluster_chain(g.first_cluster).unwrap() {
                    let mut buf = vec![0u8; cb as usize];
                    let off = vol.geometry().cluster_offset(c);
                    vol.read_raw(off, &mut buf).unwrap();
                    data.extend_from_slice(&buf);
                }
            }
            data.truncate(g.size as usize);
            out.push((dir.clone(), g.name().to_string(), data));
        }
    }
    out
}

fn profile(cap: u32) -> ProfilePlan {
    ProfilePlan {
        bitrate_cap: cap,
        mode: "cbr".into(),
        sample_rates: vec![44100, 48000],
        tags: "id3v23".into(),
        id3v1: true,
        cover: "small".into(),
        track_tag_is_position: true,
        number_in_title: false,
    }
}

#[test]
fn sync_to_a_disk_image() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipped: no ffmpeg on the PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = |n: &str| dir.path().join(n).display().to_string();
    // Sources: two tagged MP3s (one with a big PNG cover), a FLAC, an Opus,
    // and a 22.05 kHz MP3 the profile can't take as it is.
    ff(&["-f", "lavfi", "-i", "testsrc2=size=900x900:duration=1", "-frames:v", "1", &d("cover.png")]);
    let tone = |f: u32, secs: u32| format!("sine=frequency={f}:duration={secs}");
    ff(&["-f", "lavfi", "-i", &tone(440, 4), "-i", &d("cover.png"), "-map", "0:a", "-map", "1:v", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "320k",
        "-c:v", "png", "-disposition:v", "attached_pic", "-id3v2_version", "3", "-metadata", "title=Teardrop", "-metadata", "artist=Massive Attack",
        "-metadata", "album=Mezzanine", "-metadata", "track=3/11", &d("teardrop.mp3")]);
    ff(&["-f", "lavfi", "-i", &tone(550, 3), "-af", "volume=-15dB", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "192k", "-metadata", "title=Quiet One",
        "-metadata", "artist=Röyksopp", &d("quiet.mp3")]);
    ff(&["-f", "lavfi", "-i", &tone(330, 5), "-ac", "2", "-c:a", "flac", "-metadata", "TITLE=Dreams", "-metadata", "ARTIST=Fleetwood Mac", &d("dreams.flac")]);
    ff(&["-f", "lavfi", "-i", &tone(660, 3), "-ac", "2", "-c:a", "libopus", "-b:a", "96k", "-metadata", "title=Hoppípolla", &d("hopp.opus")]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=770:sample_rate=22050:duration=3", "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k", "-metadata", "title=Old Rip", &d("rip.mp3")]);

    let tools = Tools::locate().unwrap();
    let lib = Mutex::new(Library::open(&dir.path().join("lib.db")).unwrap());
    let cache = Cache::open(dir.path().join("cache")).unwrap();
    let srcs: Vec<PathBuf> = ["teardrop.mp3", "quiet.mp3", "dreams.flac", "hopp.opus", "rip.mp3"].iter().map(|n| dir.path().join(n)).collect();
    let ids: Vec<String> = scan::read_files(&lib, &tools, &srcs, &|_| {}).into_iter().map(|r| r.unwrap()).collect();

    let img = dir.path().join("stick.img");
    std::fs::File::create(&img).unwrap().set_len(256 << 20).unwrap();
    std::env::set_var("FATSTICK_TEST_STICKS", &img);
    let enc = |gain_db: f64, rate: u32| Recipe::Encode(EncodeSpec { cap: 320, vbr: false, sample_rate: rate, gain_db });
    let file = |name: &str, i: usize, recipe: Recipe, position: u32, total: u32| FilePlan {
        name: name.into(),
        track_id: ids[i].clone(),
        recipe,
        position,
        total,
        title: None,
        artist: None,
        album: None,
    };
    let plan = SyncPlan {
        set_id: "set-car".into(),
        set_name: "Car stick".into(),
        stick_id: format!("{}{}", crate::sticks::IMAGE_PREFIX, img.display()),
        stick_size: Some(256 << 20),
        label: "Car".into(),
        profile: profile(320),
        folders: vec![
            FolderPlan {
                name: "01 Road Trip".into(),
                files: vec![
                    file("01 - Fleetwood Mac - Dreams.mp3", 2, enc(-3.0, 44100), 1, 4),
                    file("02 - Massive Attack - Teardrop.mp3", 0, Recipe::Copy, 2, 4),
                    file("03 - Röyksopp - Quiet One.mp3", 1, Recipe::Gain { steps: 2 }, 3, 4),
                    file("04 - Hoppípolla.mp3", 3, enc(0.0, 48000), 4, 4),
                ],
            },
            FolderPlan { name: "02 Kids' car".into(), files: vec![file("01 - Old Rip.mp3", 4, Recipe::Copy, 1, 1)] },
        ],
        verify: true,
        record: serde_json::json!({ "content": [["a"], ["b"]] }),
        note: serde_json::json!({
            "profile": { "id": "prof-generic", "name": "Generic" },
            "playlists": [{ "id": "pl-road", "name": "Road Trip", "tracks": 4 }, { "id": "pl-kids", "name": "Kids' car", "tracks": 1 }],
        }),
    };
    let stages = Mutex::new(Vec::<String>::new());
    let progress = |p: SyncProgress| {
        let mut s = stages.lock().unwrap();
        if s.last() != Some(&p.stage) {
            s.push(p.stage);
        }
    };
    let r = run(&lib, Some(&tools), &cache, &plan, &AtomicBool::new(false), &progress);
    assert!(r.ok, "{r:?}");
    assert_eq!((r.files, r.notes.len()), (5, 1), "the 22 kHz rip was encoded instead: {:?}", r.notes);
    assert_eq!(*stages.lock().unwrap(), ["prepare", "tags", "check", "erase", "write", "verify"]);
    assert_eq!(r.record.as_ref().unwrap()["files"], 5);
    assert_eq!(lib.lock().unwrap().last_syncs().unwrap().len(), 1);

    // The note on the stick: which stick (set) of which library it holds, read
    // back by the stick list along with the volume serial the record kept.
    let view = crate::sticks::list().0.into_iter().find(|v| v.image).unwrap();
    let note = view.note.as_ref().expect("a note on the stick");
    assert_eq!(note["setId"], "set-car");
    assert_eq!(note["setName"], "Car stick");
    assert_eq!(note["libraryId"], lib.lock().unwrap().library_id().unwrap().as_str());
    assert_eq!(note["playlists"][1]["name"], "Kids' car");
    assert_eq!(note["profile"]["name"], "Generic");
    assert_eq!(view.serials, [r.record.as_ref().unwrap()["volumeSerial"].as_str().unwrap()]);
    assert!(view.volumes[0].ends_with("2 folders"), "the note isn't counted: {:?}", view.volumes);

    // Read it back: folders and files in stick order, as planned.
    let files = read_back(&img);
    let names: Vec<String> = files.iter().map(|(d, n, _)| format!("{d}/{n}")).collect();
    assert_eq!(names, [
        "01 Road Trip/01 - Fleetwood Mac - Dreams.mp3",
        "01 Road Trip/02 - Massive Attack - Teardrop.mp3",
        "01 Road Trip/03 - Röyksopp - Quiet One.mp3",
        "01 Road Trip/04 - Hoppípolla.mp3",
        "02 Kids' car/01 - Old Rip.mp3",
    ]);
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let mut probes = Vec::new();
    for (i, (_, _, data)) in files.iter().enumerate() {
        let p = out.join(format!("{i}.mp3"));
        std::fs::write(&p, data).unwrap();
        probes.push((probe(&tools, &p).unwrap(), data.clone(), p));
    }
    // Tags: playlist position as the track number, the source's other tags,
    // ID3v1 at the end, no ReplayGain.
    for (i, (p, data, _)) in probes.iter().enumerate() {
        let total = if i < 4 { 4 } else { 1 };
        let pos = if i < 4 { i as u32 + 1 } else { 1 };
        assert_eq!(p.codec, "mp3");
        assert_eq!(p.tags.track, Some(pos), "file {i}");
        assert!(data.starts_with(b"ID3\x03"), "ID3v2.3 first");
        assert_eq!(&data[data.len() - 128..data.len() - 125], b"TAG", "ID3v1 last");
        let tag = id3::Tag::read_from2(std::io::Cursor::new(data)).unwrap();
        assert_eq!(id3::TagLike::total_tracks(&tag), Some(total));
        assert!(!data.windows(10).any(|w| w == b"REPLAYGAIN"));
    }
    assert_eq!(probes[0].0.tags.title, "Dreams");
    assert_eq!(probes[3].0.tags.title, "Hoppípolla");
    // The copy is the source's audio frames, byte for byte, with a small
    // baseline JPEG cover in place of the 900 px PNG.
    let src_teardrop = probe(&tools, &srcs[0]).unwrap();
    let m = src_teardrop.mp3.as_ref().unwrap();
    let src = std::fs::read(&srcs[0]).unwrap();
    let audio = &src[m.audio_start as usize..(m.audio_start + m.audio_len) as usize];
    let (p1, d1, _) = &probes[1];
    assert!(d1.windows(audio.len().min(4096)).any(|w| w == &audio[..audio.len().min(4096)]));
    let tag = id3::Tag::read_from2(std::io::Cursor::new(d1)).unwrap();
    let pic = tag.pictures().next().expect("cover");
    assert_eq!(pic.mime_type, "image/jpeg");
    assert!(pic.data.len() <= 100 * 1024);
    assert!(image::load_from_memory(&pic.data).map(|i| image::GenericImageView::dimensions(&i) == (500, 500)).unwrap());
    assert_eq!((p1.peak_bitrate, p1.vbr), (320, false));
    // Lossless gain kept every frame; encodes are CBR at the cap.
    assert_eq!(probes[2].0.mp3.as_ref().unwrap().frames, probe(&tools, &srcs[1]).unwrap().mp3.unwrap().frames);
    for i in [0, 3, 4] {
        let s = mp3::scan(&probes[i].2).unwrap().unwrap();
        assert_eq!((s.peak_bitrate, s.vbr), (320, false), "file {i}");
    }
    assert_eq!(probes[3].0.sample_rate, 48000, "Opus stays at 48 kHz");
    assert_eq!(probes[4].0.sample_rate, 44100, "22.05 kHz goes up to 44.1");

    // A second sync re-uses the cache: nothing is encoded again.
    let before: Vec<_> = std::fs::read_dir(cache.dir().join("audio")).unwrap().map(|e| e.unwrap().path()).collect();
    let r2 = run(&lib, Some(&tools), &cache, &plan, &AtomicBool::new(false), &|_| {});
    assert!(r2.ok);
    let after: Vec<_> = std::fs::read_dir(cache.dir().join("audio")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(before.len(), after.len());

    // Cancelling during the write leaves a blank stick.
    let cancel = AtomicBool::new(false);
    let r3 = run(&lib, Some(&tools), &cache, &plan, &cancel, &|p: SyncProgress| {
        if p.stage == "write" {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(r3.cancelled && !r3.ok && r3.erased, "{r3:?}");
    let raw = std::fs::read(&img).unwrap();
    assert!(raw[..2048 * 512 + 512].iter().all(|&b| b == 0), "no partition table, no boot sector");

    // A stick that's too small: refused before anything is written.
    let small = dir.path().join("small.img");
    std::fs::File::create(&small).unwrap().set_len(40 << 20).unwrap();
    std::fs::write(&small, vec![0x5Au8; 40 << 20]).unwrap();
    let big = dir.path().join("big.flac");
    ff(&["-f", "lavfi", "-i", "sine=frequency=100:duration=1500", "-ac", "2", "-c:a", "flac", &big.display().to_string()]);
    let big_id = scan::read_file(&lib, &tools, &big).unwrap();
    std::env::set_var("FATSTICK_TEST_STICKS", std::env::join_paths([&img, &small]).unwrap());
    let mut p4 = plan.clone();
    p4.stick_id = format!("{}{}", crate::sticks::IMAGE_PREFIX, small.display());
    p4.stick_size = Some(40 << 20);
    p4.folders = vec![FolderPlan { name: String::new(), files: vec![FilePlan { name: "big.mp3".into(), track_id: big_id, recipe: enc(0.0, 44100), position: 1, total: 1, title: None, artist: None, album: None }] }];
    let r4 = run(&lib, Some(&tools), &cache, &p4, &AtomicBool::new(false), &|_| {});
    assert!(!r4.ok && !r4.erased && r4.error.as_deref().unwrap_or("").contains("needs"), "{r4:?}");
    assert!(std::fs::read(&small).unwrap().iter().all(|&b| b == 0x5A), "untouched");

    // Another stick where the chosen one was (same device, other size): refused, untouched.
    let mut p5 = plan.clone();
    p5.stick_id = p4.stick_id.clone();
    let r5 = run(&lib, Some(&tools), &cache, &p5, &AtomicBool::new(false), &|_| {});
    assert!(!r5.ok && !r5.erased && r5.error.as_deref().unwrap_or("").contains("different stick"), "{r5:?}");
    assert!(std::fs::read(&small).unwrap().iter().all(|&b| b == 0x5A), "untouched");

    // Rendered files: a track cut and padded, silence between playlists, and
    // a playlist joined into one track (with tags of its own).
    let part = |i: Option<usize>| RenderPart { track_id: i.map(|i| ids[i].clone()), ..Default::default() };
    let render = |parts: Vec<RenderPart>| Recipe::Render(RenderSpec { cap: 320, vbr: false, sample_rate: 44100, parts });
    let own = |name: &str, recipe: Recipe, title: &str, album: Option<&str>| FilePlan {
        name: name.into(),
        track_id: String::new(),
        recipe,
        position: 1,
        total: 1,
        title: Some(title.into()),
        artist: None,
        album: album.map(String::from),
    };
    let mut p6 = plan.clone();
    p6.folders = vec![
        // Dreams is 5 s: 1 s cut from the start, 0.5 s of silence after.
        FolderPlan { name: "01 Trimmed".into(), files: vec![file("01 - Dreams.mp3", 2, render(vec![RenderPart { cut_start: 1.0, pad_end: 0.5, ..part(Some(2)) }]), 1, 1)] },
        FolderPlan { name: "02 Silence".into(), files: vec![own("01 - Silence.mp3", render(vec![RenderPart { pad_start: 2.0, ..part(None) }]), "Silence", None)] },
        // Teardrop (4 s) and Hoppípolla (3 s), joined: 7 s.
        FolderPlan { name: "03 Joined".into(), files: vec![own("01 - Joined.mp3", render(vec![part(Some(0)), part(Some(3))]), "Joined", Some("Joined"))] },
    ];
    let r6 = run(&lib, Some(&tools), &cache, &p6, &AtomicBool::new(false), &|_| {});
    assert!(r6.ok, "{r6:?}");
    let files = read_back(&img);
    let names: Vec<String> = files.iter().map(|(d, n, _)| format!("{d}/{n}")).collect();
    assert_eq!(names, ["01 Trimmed/01 - Dreams.mp3", "02 Silence/01 - Silence.mp3", "03 Joined/01 - Joined.mp3"]);
    let mut got = Vec::new();
    for (i, (_, _, data)) in files.iter().enumerate() {
        let p = out.join(format!("r{i}.mp3"));
        std::fs::write(&p, data).unwrap();
        got.push(probe(&tools, &p).unwrap());
    }
    let secs: Vec<f64> = got.iter().map(|p| p.duration).collect();
    for (have, want) in secs.iter().zip([4.5, 2.0, 7.0]) {
        assert!((have - want).abs() < 0.1, "lengths {secs:?}");
    }
    assert_eq!(got[0].tags.title, "Dreams", "a trimmed track keeps its tags");
    assert_eq!((got[1].tags.title.as_str(), got[1].tags.artist.as_str(), got[1].cover), ("Silence", "", false));
    assert_eq!((got[2].tags.title.as_str(), got[2].tags.album.as_str(), got[2].tags.artist.as_str()), ("Joined", "Joined", ""));
    assert!(got[2].cover, "the joined track takes the first track's cover");
}

#[test]
fn player_plays_seeks_and_ends() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    std::env::set_var("FATSTICK_NULL_AUDIO", "1");
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("two.flac");
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-ac", "2", "-c:a", "flac", &f.display().to_string()]);
    let (tx, rx) = std::sync::mpsc::channel();
    let p = crate::player::Player::start(Some(Tools::locate().unwrap()), move |e| {
        let _ = tx.send(e);
    });
    p.send(crate::player::Cmd::Play { path: f.clone(), from: 0.0, gain_db: 0.0, duration: 2.0, token: "e1".into(), key: None, shape: Default::default() });
    let mut states = Vec::new();
    let mut max_pos = 0.0f64;
    let start = std::time::Instant::now();
    while let Ok(e) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
        assert_eq!(e.token, "e1");
        max_pos = max_pos.max(e.pos);
        if states.last() != Some(&e.state) {
            states.push(e.state.clone());
        }
        if e.state == "ended" || e.state == "error" {
            break;
        }
    }
    assert_eq!(states, ["playing", "ended"], "{states:?}");
    let took = start.elapsed().as_secs_f64();
    assert!((1.7..3.5).contains(&took), "real time: {took}");
    assert!(max_pos > 1.5);
    // Seeking near the end ends sooner.
    p.send(crate::player::Cmd::Play { path: f.clone(), from: 0.0, gain_db: -6.0, duration: 2.0, token: "e2".into(), key: Some(fatstick_audio::stretch::Key { semitones: 3.0, keep_formants: true }), shape: crate::player::Shape { start: 0.0, end: 1.5, fade_in: 0.5, fade_out: 0.5 } });
    std::thread::sleep(std::time::Duration::from_millis(200));
    p.send(crate::player::Cmd::Seek(1.6));
    let start = std::time::Instant::now();
    loop {
        let e = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        if e.state == "ended" {
            break;
        }
    }
    assert!(start.elapsed().as_secs_f64() < 1.2);
    // A cut start: playing and seeking start there, not before it.
    p.send(crate::player::Cmd::Play { path: f, from: 0.0, gain_db: 0.0, duration: 2.0, token: "e3".into(), key: None, shape: crate::player::Shape { start: 1.0, ..Default::default() } });
    let first = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(first.token == "e3" && first.pos >= 1.0, "{} {}", first.token, first.pos);
    p.send(crate::player::Cmd::Seek(0.2));
    let seeked = loop {
        let e = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        if e.pos < 1.2 || e.state == "ended" {
            break e;
        }
    };
    assert!(seeked.state != "ended" && (1.0..1.2).contains(&seeked.pos), "{} {}", seeked.state, seeked.pos);
    p.send(crate::player::Cmd::Stop);
}

#[test]
fn music_folder_scans_writes_tags_and_renames() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    std::fs::create_dir_all(root.join("Band")).unwrap();
    std::fs::create_dir_all(dir.path().join("Elsewhere")).unwrap();
    let d = |p: PathBuf| p.display().to_string();
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-c:a", "libmp3lame", "-metadata", "title=Song", &d(root.join("Band").join("song.mp3"))]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-c:a", "flac", &d(dir.path().join("Elsewhere").join("outside.flac"))]);
    let lib = std::sync::Arc::new(Mutex::new(Library::open_in_memory().unwrap()));
    let tools = Tools::locate().unwrap();
    let events = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let ev = events.clone();
    let music = crate::folder::MusicFolder::start(lib.clone(), Some(tools.clone()), move |name, _| ev.lock().unwrap().push(name.to_string()));
    assert!(music.info().root.is_none());
    music.set_root(&root).unwrap();
    let t0 = std::time::Instant::now();
    while (music.info().scanning || music.info().last.is_none()) && t0.elapsed().as_secs() < 30 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let info = music.info();
    assert_eq!(info.last.as_ref().map(|s| s.files), Some(1), "{info:?}");
    assert!(events.lock().unwrap().iter().any(|e| e == "library-scanned"));
    let id = info.last.unwrap().changed[0].clone();
    // The scan reads the folder, which the watcher must not take for a change.
    let scans = || events.lock().unwrap().iter().filter(|e| *e == "library-scanned").count();
    assert!(info.watching);
    std::thread::sleep(std::time::Duration::from_millis(3500));
    assert_eq!(scans(), 1, "the scan set off another scan");
    // A file copied in is noticed by itself, two seconds after it stops changing.
    std::fs::copy(root.join("Band").join("song.mp3"), root.join("Band").join("copy.mp3")).unwrap();
    let t1 = std::time::Instant::now();
    while scans() < 2 && t1.elapsed().as_secs() < 15 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    while music.info().scanning && t1.elapsed().as_secs() < 30 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(music.info().last.map(|s| (s.files, s.added)), Some((2, 1)), "the watcher saw the new file");
    std::fs::remove_file(root.join("Band").join("copy.mp3")).unwrap();
    let outside = scan::read_file(&lib, &tools, &dir.path().join("Elsewhere").join("outside.flac")).unwrap();

    // Tags: written into the file inside the folder, refused outside it.
    let edit = fatstick_audio::tagwrite::TagEdit { artist: Some("The Band".into()), labels: Some(vec!["chill".into()]), ..Default::default() };
    let r = crate::folder::write(&music, &[crate::folder::TagJob { track_id: id.clone(), edit: edit.clone() }, crate::folder::TagJob { track_id: outside.clone(), edit }]);
    assert_eq!(r.tracks.len(), 1);
    assert_eq!((r.tracks[0].artist.as_str(), r.tracks[0].labels.clone()), ("The Band", vec!["chill".to_string()]));
    assert!(r.failed.len() == 1 && r.failed[0].error.contains("isn't in your music folder"), "{:?}", r.failed);
    assert!(probe(&tools, &dir.path().join("Elsewhere").join("outside.flac")).unwrap().tags.artist.is_empty(), "untouched");

    // Renames: within the folder, keeping the id; clashes and new types refused.
    let r = crate::folder::rename(&music, &[crate::folder::RenameJob { track_id: id.clone(), name: "01 - The Band - Song.mp3".into() }]);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(r.tracks[0].id, id);
    assert!(root.join("Band").join("01 - The Band - Song.mp3").is_file() && !root.join("Band").join("song.mp3").exists());
    let bad = crate::folder::rename(&music, &[crate::folder::RenameJob { track_id: id.clone(), name: "x.flac".into() }, crate::folder::RenameJob { track_id: outside, name: "y.flac".into() }]);
    assert_eq!(bad.failed.len(), 2, "{:?}", bad.failed);
    // A change of case only.
    let r = crate::folder::rename(&music, &[crate::folder::RenameJob { track_id: id.clone(), name: "01 - the band - song.mp3".into() }]);
    assert!(r.failed.is_empty() && r.tracks[0].path.ends_with("01 - the band - song.mp3"), "{:?}", r);
}

/// A big first scan sends the tracks to the window as it reads them, each
/// once: in batches while reading, the rest when it's done.
#[test]
fn music_folder_sends_tracks_as_it_reads() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let one = dir.path().join("one.mp3");
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=0.3", "-c:a", "libmp3lame", "-b:a", "64k", &one.display().to_string()]);
    for a in 0..35 {
        let album = root.join(format!("Artist {a}")).join("Album");
        std::fs::create_dir_all(&album).unwrap();
        for t in 0..20 {
            std::fs::copy(&one, album.join(format!("{t:02} song.mp3"))).unwrap();
        }
    }
    let lib = std::sync::Arc::new(Mutex::new(Library::open_in_memory().unwrap()));
    let tools = Tools::locate().unwrap();
    let events = std::sync::Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let ev = events.clone();
    let music = crate::folder::MusicFolder::start(lib.clone(), Some(tools), move |name, v| ev.lock().unwrap().push((name.to_string(), v)));
    music.set_root(&root).unwrap();
    let t0 = std::time::Instant::now();
    while !events.lock().unwrap().iter().any(|(n, _)| n == "library-scanned") && t0.elapsed().as_secs() < 120 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let evs = events.lock().unwrap();
    let ids = |name: &str| -> Vec<String> {
        evs.iter().filter(|(n, _)| n == name).flat_map(|(_, v)| v["tracks"].as_array().cloned().unwrap_or_default()).map(|t| t["id"].as_str().unwrap().to_string()).collect()
    };
    let batched = ids("library-tracks");
    let last = ids("library-scanned");
    assert!(!batched.is_empty(), "tracks went out while reading");
    let all: std::collections::HashSet<_> = batched.iter().chain(&last).collect();
    assert_eq!((all.len(), batched.len() + last.len()), (700, 700), "every track once");
    let progress = evs.iter().filter(|(n, _)| n == "library-progress").count();
    assert!(progress < 200, "progress is sent ten times a second, not for every file: {progress}");
}
