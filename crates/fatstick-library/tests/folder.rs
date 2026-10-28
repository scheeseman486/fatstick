//! The music folder against real files: a scan finds what's there (MP3s read
//! quickly, in full only when asked), rescans read only what changed, a moved
//! or renamed file keeps its id, a retagged one is read again, and a deleted
//! one is missing, as the library tells the window without looking at every
//! file. Fixtures are made with the ffmpeg on the PATH; skipped without it.

use std::process::Command;
use std::sync::Mutex;

use fatstick_audio::tagwrite::{write_tags, TagEdit};
use fatstick_audio::Tools;
use fatstick_library::folder::scan_folder;
use fatstick_library::{scan, Library};

fn ff(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

#[test]
fn the_music_folder_kept_up_to_date() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipped: no ffmpeg on the PATH");
        return;
    }
    let tools = Tools::locate().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let album = root.join("Artist").join("Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();
    let tone = "sine=frequency=440:duration=1";
    let d = |p: &std::path::Path| p.display().to_string();
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "libmp3lame", "-metadata", "title=First", &d(&album.join("01 a.mp3"))]);
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "flac", "-metadata", "title=Second", &d(&album.join("02 b.flac"))]);
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "libmp3lame", &d(&root.join(".hidden").join("x.mp3"))]);
    std::fs::write(root.join("notes.txt"), "not audio").unwrap();
    let lib = Mutex::new(Library::open_in_memory().unwrap());
    let quiet = |_| {};

    let s1 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert_eq!((s1.files, s1.added, s1.moved, s1.missing, s1.failed), (2, 2, 0, 0, 0), "{s1:?}");
    let s2 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert!(s2.changed.is_empty(), "nothing read again: {s2:?}");

    // The MP3 was read quickly; reading it in full (before a sync) fills in its frames.
    let mp3 = std::fs::canonicalize(album.join("01 a.mp3")).unwrap();
    let a0 = lib.lock().unwrap().track_by_path(&mp3).unwrap().unwrap();
    assert!(a0.quick && a0.title == "First" && a0.bitrate > 0, "{a0:?}");
    assert!(lib.lock().unwrap().probe_of(&a0.id).unwrap().unwrap().2.mp3.is_none());
    assert_eq!(scan::read_file(&lib, &tools, &mp3).unwrap(), a0.id);
    let a1 = lib.lock().unwrap().track(&a0.id).unwrap().unwrap();
    assert!(!a1.quick && lib.lock().unwrap().probe_of(&a0.id).unwrap().unwrap().2.mp3.is_some(), "{a1:?}");
    assert!(scan_folder(&lib, &tools, &root, &quiet).unwrap().changed.is_empty(), "a scan leaves a full read alone");
    let flac = lib.lock().unwrap().track_by_path(&std::fs::canonicalize(album.join("02 b.flac")).unwrap()).unwrap().unwrap();
    assert!(!flac.quick, "only MP3s have frames to read later");

    // Moved and renamed: the same track.
    let b = lib.lock().unwrap().track_by_path(&std::fs::canonicalize(album.join("02 b.flac")).unwrap()).unwrap().unwrap();
    std::fs::create_dir_all(root.join("Other")).unwrap();
    std::fs::rename(album.join("02 b.flac"), root.join("Other").join("renamed.flac")).unwrap();
    let s3 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert_eq!((s3.moved, s3.added, s3.missing), (1, 0, 0), "{s3:?}");
    let b2 = lib.lock().unwrap().track(&b.id).unwrap().unwrap();
    assert!(b2.path.ends_with("renamed.flac") && b2.exists && b2.title == "Second", "{b2:?}");

    // Retagged: read again.
    write_tags(&album.join("01 a.mp3"), &TagEdit { title: Some("Renamed Title".into()), labels: Some(vec!["chill".into()]), ..Default::default() }).unwrap();
    let s4 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert_eq!(s4.changed.len(), 1, "{s4:?}");
    let a = lib.lock().unwrap().track(&s4.changed[0]).unwrap().unwrap();
    assert_eq!((a.title.as_str(), a.labels.clone()), ("Renamed Title", vec!["chill".to_string()]));

    // Deleted: missing, and the scan says so (the window was told it was there).
    // Added: read.
    lib.lock().unwrap().set_music_root(Some(&std::fs::canonicalize(&root).unwrap()));
    let gone = a.id.clone();
    std::fs::remove_file(album.join("01 a.mp3")).unwrap();
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "libopus", &d(&album.join("03 c.opus"))]);
    let s5 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert_eq!((s5.files, s5.added, s5.missing), (2, 1, 1), "{s5:?}");
    assert_eq!(s5.presence, vec![gone.clone()], "{s5:?}");
    assert!(!lib.lock().unwrap().track(&gone).unwrap().unwrap().exists);
    let s6 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert!(s6.presence.is_empty() && s6.changed.is_empty(), "told once: {s6:?}");
    // Back again, unchanged: there, and the window is told.
    std::fs::rename(root.join("Other").join("renamed.flac"), dir.path().join("away.flac")).unwrap();
    let s7 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert_eq!((s7.presence.clone(), s7.missing), (vec![b.id.clone()], 2), "{s7:?}");
    std::fs::rename(dir.path().join("away.flac"), root.join("Other").join("renamed.flac")).unwrap();
    let s8 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert!(s8.presence == vec![b.id.clone()] && s8.changed.is_empty() && lib.lock().unwrap().track(&b.id).unwrap().unwrap().exists, "{s8:?}");
    assert!(scan_folder(&lib, &tools, &root.join("nope"), &quiet).is_err());
}
