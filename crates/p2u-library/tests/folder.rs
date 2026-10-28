//! The music folder against real files: a scan finds what's there, rescans
//! read only what changed, a moved or renamed file keeps its id, a retagged
//! one is read again, and a deleted one is missing. Fixtures are made with the
//! ffmpeg on the PATH; skipped without it.

use std::process::Command;
use std::sync::Mutex;

use p2u_audio::tagwrite::{write_tags, TagEdit};
use p2u_audio::Tools;
use p2u_library::folder::scan_folder;
use p2u_library::Library;

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

    // Deleted: missing. Added: read.
    std::fs::remove_file(album.join("01 a.mp3")).unwrap();
    ff(&["-f", "lavfi", "-i", tone, "-c:a", "libopus", &d(&album.join("03 c.opus"))]);
    let s5 = scan_folder(&lib, &tools, &root, &quiet).unwrap();
    assert_eq!((s5.files, s5.added, s5.missing), (2, 1, 1), "{s5:?}");
    assert!(scan_folder(&lib, &tools, &root.join("nope"), &quiet).is_err());
}
