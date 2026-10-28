//! The sync, end to end, on a disk image standing in for a stick: real audio
//! made with the `ffmpeg` on the PATH, read into a library, prepared, tagged,
//! written in two folders and read back the way a player reads it.
//! Skipped when ffmpeg isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use p2u_audio::encode::EncodeSpec;
use p2u_audio::prepare::Recipe;
use p2u_audio::{mp3, probe, Cache, Tools};
use p2u_core::fat::{DirRef, EntryKind, Volume};
use p2u_library::{scan, Library};

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
    std::env::set_var("P2U_TEST_STICKS", &img);
    let enc = |gain_db: f64, rate: u32| Recipe::Encode(EncodeSpec { cap: 320, vbr: false, sample_rate: rate, gain_db });
    let file = |name: &str, i: usize, recipe: Recipe, position: u32, total: u32| FilePlan { name: name.into(), track_id: ids[i].clone(), recipe, position, total };
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
    std::env::set_var("P2U_TEST_STICKS", std::env::join_paths([&img, &small]).unwrap());
    let mut p4 = plan.clone();
    p4.stick_id = format!("{}{}", crate::sticks::IMAGE_PREFIX, small.display());
    p4.stick_size = Some(40 << 20);
    p4.folders = vec![FolderPlan { name: String::new(), files: vec![FilePlan { name: "big.mp3".into(), track_id: big_id, recipe: enc(0.0, 44100), position: 1, total: 1 }] }];
    let r4 = run(&lib, Some(&tools), &cache, &p4, &AtomicBool::new(false), &|_| {});
    assert!(!r4.ok && !r4.erased && r4.error.as_deref().unwrap_or("").contains("needs"), "{r4:?}");
    assert!(std::fs::read(&small).unwrap().iter().all(|&b| b == 0x5A), "untouched");

    // Another stick where the chosen one was (same device, other size): refused, untouched.
    let mut p5 = plan.clone();
    p5.stick_id = p4.stick_id.clone();
    let r5 = run(&lib, Some(&tools), &cache, &p5, &AtomicBool::new(false), &|_| {});
    assert!(!r5.ok && !r5.erased && r5.error.as_deref().unwrap_or("").contains("different stick"), "{r5:?}");
    assert!(std::fs::read(&small).unwrap().iter().all(|&b| b == 0x5A), "untouched");
}

#[test]
fn player_plays_seeks_and_ends() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    std::env::set_var("P2U_NULL_AUDIO", "1");
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("two.flac");
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-ac", "2", "-c:a", "flac", &f.display().to_string()]);
    let (tx, rx) = std::sync::mpsc::channel();
    let p = crate::player::Player::start(Some(Tools::locate().unwrap()), move |e| {
        let _ = tx.send(e);
    });
    p.send(crate::player::Cmd::Play { path: f.clone(), from: 0.0, gain_db: 0.0, duration: 2.0, token: "e1".into() });
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
    p.send(crate::player::Cmd::Play { path: f, from: 0.0, gain_db: -6.0, duration: 2.0, token: "e2".into() });
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
}
