//! Finding yt-dlp and Deno, and getting them when asked to.
//!
//! Neither comes with the app: YouTube changes often, yt-dlp keeps up with it in
//! new releases, and an installer can't. So a person asks for them once, and they
//! come from each project's own GitHub releases into the app's data folder,
//! checked against the checksums published with the release and tried before
//! they're put in place. The copy of yt-dlp there is kept current ([`update_ytdlp`]).
//!
//! Deno is what yt-dlp recommends for running the code YouTube asks to be run,
//! and the one runtime it uses without being told to, as Deno's sandbox keeps that
//! code from touching the computer.
//!
//! Copies the person has installed themselves are found on the PATH and used as
//! they are; the desktop's PATH can be short, so the usual places are looked in too.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const YTDLP_RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
pub const DENO_RELEASES: &str = "https://github.com/denoland/deno/releases/latest/download";

const EXE: &str = std::env::consts::EXE_SUFFIX;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    pub path: PathBuf,
    pub version: String,
    /// In the app's own folder, so the app keeps it current.
    pub managed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub ytdlp: Option<Program>,
    pub deno: Option<Program>,
}

/// A program's version, if it runs at all.
fn version_of(path: &Path, flag: &str) -> Option<String> {
    let mut c = Command::new(path);
    c.arg(flag).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    super::quiet_child(&mut c);
    let o = c.output().ok()?;
    if !o.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    // "deno 2.5.0 (stable, ...)" or "2026.10.26".
    let first = text.lines().next().unwrap_or("").trim();
    let v = first.strip_prefix("deno ").map_or(first, |x| x.split_whitespace().next().unwrap_or(x));
    Some(v.to_string())
}

/// Where a person's own copy might be, for a desktop whose PATH is short.
fn usual_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    if let Some(h) = &home {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".deno/bin"));
    }
    if cfg!(target_os = "macos") {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
    }
    dirs
}

fn find(name: &str, flag: &str, own: &Path, var: &str) -> Option<Program> {
    if let Some(p) = std::env::var_os(var) {
        let path = PathBuf::from(p);
        return version_of(&path, flag).map(|version| Program { path, version, managed: false });
    }
    let mine = own.join(format!("{name}{EXE}"));
    if mine.is_file() {
        if let Some(version) = version_of(&mine, flag) {
            return Some(Program { path: mine, version, managed: true });
        }
    }
    usual_dirs().into_iter().map(|d| d.join(format!("{name}{EXE}"))).filter(|p| p.is_file()).find_map(|path| version_of(&path, flag).map(|version| Program { path, version, managed: false }))
}

/// yt-dlp and Deno: the app's own copies, else the person's. `FATSTICK_YTDLP`
/// and `FATSTICK_DENO` name particular programs instead.
pub fn locate(own: &Path) -> Found {
    Found { ytdlp: find("yt-dlp", "--version", own, "FATSTICK_YTDLP"), deno: find("deno", "--version", own, "FATSTICK_DENO") }
}

/// What to get on this system, or `None` where no build is published.
#[derive(Debug, Clone, PartialEq)]
pub struct Assets {
    pub ytdlp: &'static str,
    pub deno: &'static str,
}

pub fn assets() -> Option<Assets> {
    let (ytdlp, deno) = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("yt-dlp_linux", "deno-x86_64-unknown-linux-gnu.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("yt-dlp_linux_aarch64", "deno-aarch64-unknown-linux-gnu.zip")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        ("yt-dlp.exe", "deno-x86_64-pc-windows-msvc.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("yt-dlp_macos", "deno-aarch64-apple-darwin.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        ("yt-dlp_macos", "deno-x86_64-apple-darwin.zip")
    } else {
        return None;
    };
    Some(Assets { ytdlp, deno })
}

/// Where the files come from: their releases, or a test's own server.
#[derive(Debug, Clone)]
pub struct Sources {
    pub ytdlp: String,
    pub deno: String,
    pub assets: Assets,
}

impl Sources {
    pub fn official() -> Option<Sources> {
        Some(Sources { ytdlp: YTDLP_RELEASES.into(), deno: DENO_RELEASES.into(), assets: assets()? })
    }
}

/// How far along: `what` is "yt-dlp" or "Deno".
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupProgress {
    pub what: String,
    pub done: u64,
    pub total: u64,
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder().tls_config(tls).timeout_connect(Some(Duration::from_secs(20))).timeout_recv_response(Some(Duration::from_secs(60))).timeout_recv_body(Some(Duration::from_secs(30 * 60))).user_agent(concat!("Fatstick/", env!("CARGO_PKG_VERSION"))).build().into()
}

fn get_text(url: &str) -> Result<String, String> {
    let mut r = agent().get(url).call().map_err(|e| format!("Couldn't get {url}: {e}"))?;
    r.body_mut().read_to_string().map_err(|e| format!("Couldn't read {url}: {e}"))
}

/// Download to a file, hashing as it goes. Returns the file's SHA-256.
fn download(url: &str, to: &Path, what: &str, cancel: &AtomicBool, progress: &mut dyn FnMut(SetupProgress)) -> Result<String, String> {
    let mut r = agent().get(url).call().map_err(|e| format!("Couldn't get {what}: {e}"))?;
    let total = r.body().content_length().unwrap_or(0);
    let mut body = r.body_mut().as_reader();
    let mut f = std::fs::File::create(to).map_err(|e| format!("Couldn't write {}: {e}", to.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled.".into());
        }
        let n = body.read(&mut buf).map_err(|e| format!("The download of {what} stopped: {e}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        f.write_all(&buf[..n]).map_err(|e| format!("Couldn't write {}: {e}", to.display()))?;
        done += n as u64;
        progress(SetupProgress { what: what.into(), done, total });
    }
    f.flush().map_err(|e| e.to_string())?;
    Ok(hex(&hash.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The checksum of `name` in a "hash  name" list (yt-dlp's SHA2-256SUMS).
pub fn sum_for(list: &str, name: &str) -> Option<String> {
    list.lines().find_map(|l| {
        let mut p = l.split_whitespace();
        let (h, n) = (p.next()?, p.next()?);
        (n.trim_start_matches('*') == name && is_sum(h)).then(|| h.to_ascii_lowercase())
    })
}

fn is_sum(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// The one checksum in a release's `.sha256sum` file, however it's laid out
/// (Deno's Windows ones come from PowerShell).
pub fn only_sum(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit()).find(|w| is_sum(w)).map(str::to_ascii_lowercase)
}

#[cfg(unix)]
fn make_runnable(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}
#[cfg(not(unix))]
fn make_runnable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Put a downloaded program in place, once it has been seen to run.
fn install_program(tmp: &Path, dest: &Path, flag: &str, what: &str) -> Result<(), String> {
    make_runnable(tmp)?;
    let Some(_) = version_of(tmp, flag) else {
        let _ = std::fs::remove_file(tmp);
        return Err(format!("{what} was downloaded, but it won't run on this computer."));
    };
    std::fs::rename(tmp, dest).map_err(|e| format!("Couldn't put {what} in place: {e}"))
}

fn get_ytdlp(dir: &Path, s: &Sources, cancel: &AtomicBool, progress: &mut dyn FnMut(SetupProgress)) -> Result<(), String> {
    let want = sum_for(&get_text(&format!("{}/SHA2-256SUMS", s.ytdlp))?, s.assets.ytdlp).ok_or("yt-dlp's release doesn't list a build for this system.")?;
    let tmp = dir.join(format!(".yt-dlp-new{EXE}"));
    let r = (|| {
        let got = download(&format!("{}/{}", s.ytdlp, s.assets.ytdlp), &tmp, "yt-dlp", cancel, progress)?;
        if got != want {
            return Err("The yt-dlp that arrived doesn't match its published checksum, so it wasn't used.".to_string());
        }
        install_program(&tmp, &dir.join(format!("yt-dlp{EXE}")), "--version", "yt-dlp")
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn get_deno(dir: &Path, s: &Sources, cancel: &AtomicBool, progress: &mut dyn FnMut(SetupProgress)) -> Result<(), String> {
    let want = only_sum(&get_text(&format!("{}/{}.sha256sum", s.deno, s.assets.deno))?).ok_or("Deno's release doesn't give a checksum for this system.")?;
    let zip_path = dir.join(".deno-new.zip");
    let tmp = dir.join(format!(".deno-new{EXE}"));
    let r = (|| {
        let got = download(&format!("{}/{}", s.deno, s.assets.deno), &zip_path, "Deno", cancel, progress)?;
        if got != want {
            return Err("The Deno that arrived doesn't match its published checksum, so it wasn't used.".to_string());
        }
        let f = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| format!("Deno's archive couldn't be opened: {e}"))?;
        let mut inner = z.by_name(&format!("deno{EXE}")).map_err(|e| format!("Deno's archive doesn't hold the program: {e}"))?;
        let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        std::io::copy(&mut inner, &mut out).map_err(|e| format!("Deno's archive couldn't be unpacked: {e}"))?;
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        install_program(&tmp, &dir.join(format!("deno{EXE}")), "--version", "Deno")
    })();
    let _ = std::fs::remove_file(&zip_path);
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// Get what's asked for into `dir`. Nothing replaces a working copy until the
/// new one has passed its checksum and been seen to run.
pub fn install(dir: &Path, s: &Sources, ytdlp: bool, deno: bool, cancel: &AtomicBool, progress: &mut dyn FnMut(SetupProgress)) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}", dir.display()))?;
    if ytdlp {
        get_ytdlp(dir, s, cancel, progress)?;
    }
    if deno {
        get_deno(dir, s, cancel, progress)?;
    }
    Ok(())
}

/// The app's copy of yt-dlp, brought up to date. `true` if it changed; costs one
/// small download when it's current already.
pub fn update_ytdlp(dir: &Path, s: &Sources, cancel: &AtomicBool, progress: &mut dyn FnMut(SetupProgress)) -> Result<bool, String> {
    let want = sum_for(&get_text(&format!("{}/SHA2-256SUMS", s.ytdlp))?, s.assets.ytdlp).ok_or("yt-dlp's release doesn't list a build for this system.")?;
    let mine = dir.join(format!("yt-dlp{EXE}"));
    if file_sum(&mine).as_deref() == Some(want.as_str()) {
        return Ok(false);
    }
    get_ytdlp(dir, s, cancel, progress)?;
    Ok(true)
}

fn file_sum(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut hash = Sha256::new();
    std::io::copy(&mut f, &mut hash).ok()?;
    Some(hex(&hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::Arc;

    #[test]
    fn reads_checksums() {
        let list = "aaaa  short\n".to_string() + &"e".repeat(64) + "  yt-dlp\n" + &"F".repeat(64) + " *yt-dlp_linux\n";
        assert_eq!(sum_for(&list, "yt-dlp").unwrap(), "e".repeat(64));
        assert_eq!(sum_for(&list, "yt-dlp_linux").unwrap(), "f".repeat(64));
        assert_eq!(sum_for(&list, "yt-dlp.exe"), None);
        assert_eq!(sum_for("aaaa  short", "short"), None);
        let one = "a".repeat(64);
        assert_eq!(only_sum(&format!("{one}  deno.zip\n")).unwrap(), one);
        assert_eq!(only_sum(&format!("Algorithm : SHA256\nHash      : {}\nPath      : x\n", one.to_uppercase())).unwrap(), one);
        assert_eq!(only_sum("no sums here"), None);
    }

    #[test]
    fn there_is_a_build_for_what_the_app_ships_on() {
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) && cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            let a = assets().expect("a build for this system");
            assert!(a.ytdlp.starts_with("yt-dlp") && a.deno.starts_with("deno-") && a.deno.ends_with(".zip"));
        }
    }

    /// A release server on this computer, serving given files.
    fn serve(files: HashMap<String, Vec<u8>>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        let files = Arc::new(files);
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let files = files.clone();
                std::thread::spawn(move || {
                    let mut s = s;
                    let mut r = std::io::BufReader::new(s.try_clone().unwrap());
                    let mut first = String::new();
                    r.read_line(&mut first).unwrap();
                    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                    let mut l = String::new();
                    while r.read_line(&mut l).is_ok_and(|n| n > 2) {
                        l.clear();
                    }
                    let (head, body): (String, &[u8]) = match files.get(&path) {
                        Some(b) => (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()), b),
                        None => ("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), b""),
                    };
                    s.write_all(head.as_bytes()).unwrap();
                    s.write_all(body).unwrap();
                });
            }
        });
        at
    }

    fn sum(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    fn deno_zip(program: &[u8]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut out);
            z.start_file(format!("deno{EXE}"), zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(program).unwrap();
            z.finish().unwrap();
        }
        out.into_inner()
    }

    fn sources(at: &str) -> Sources {
        Sources { ytdlp: format!("{at}/y"), deno: format!("{at}/d"), assets: Assets { ytdlp: "yt-dlp_test", deno: "deno-test.zip" } }
    }

    #[cfg(unix)]
    #[test]
    fn gets_them_checks_them_and_keeps_them_current() {
        let ytdlp = b"#!/bin/sh\necho 2026.10.26\n".to_vec();
        let newer = b"#!/bin/sh\necho 2026.11.01\n".to_vec();
        let deno = b"#!/bin/sh\necho 'deno 2.9.6 (stable, release, x86_64)'\n".to_vec();
        let zip = deno_zip(&deno);
        let files = |y: &[u8]| {
            HashMap::from([
                ("/y/SHA2-256SUMS".to_string(), format!("{}  yt-dlp_test\n{}  other\n", sum(y), "0".repeat(64)).into_bytes()),
                ("/y/yt-dlp_test".to_string(), y.to_vec()),
                ("/d/deno-test.zip.sha256sum".to_string(), format!("{}  deno-test.zip\n", sum(&zip)).into_bytes()),
                ("/d/deno-test.zip".to_string(), zip.clone()),
            ])
        };
        let s = sources(&serve(files(&ytdlp)));
        let d = tempfile::tempdir().unwrap();
        let no = AtomicBool::new(false);
        let mut seen = Vec::new();
        install(d.path(), &s, true, true, &no, &mut |p| seen.push(p.what)).unwrap();
        assert!(seen.iter().any(|w| w == "yt-dlp") && seen.iter().any(|w| w == "Deno"));
        let names: Vec<String> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "nothing left behind: {names:?}");
        assert_eq!(version_of(&d.path().join("yt-dlp"), "--version").unwrap(), "2026.10.26");
        assert_eq!(version_of(&d.path().join("deno"), "--version").unwrap(), "2.9.6");
        assert!(!update_ytdlp(d.path(), &s, &no, &mut |_| {}).unwrap(), "current already");
        let s2 = sources(&serve(files(&newer)));
        assert!(update_ytdlp(d.path(), &s2, &no, &mut |_| {}).unwrap());
        assert_eq!(version_of(&d.path().join("yt-dlp"), "--version").unwrap(), "2026.11.01");
    }

    #[cfg(unix)]
    #[test]
    fn a_wrong_checksum_is_refused_and_the_old_copy_stays() {
        let good = b"#!/bin/sh\necho 2026.01.01\n".to_vec();
        let evil = b"#!/bin/sh\necho 6666.66.66\n".to_vec();
        let at = serve(HashMap::from([("/y/SHA2-256SUMS".to_string(), format!("{}  yt-dlp_test\n", sum(&good)).into_bytes()), ("/y/yt-dlp_test".to_string(), evil)]));
        let d = tempfile::tempdir().unwrap();
        let mine = d.path().join("yt-dlp");
        std::fs::write(&mine, b"#!/bin/sh\necho 2025.01.01\n").unwrap();
        make_runnable(&mine).unwrap();
        let no = AtomicBool::new(false);
        let e = install(d.path(), &sources(&at), true, false, &no, &mut |_| {}).unwrap_err();
        assert!(e.contains("checksum"), "{e}");
        assert_eq!(version_of(&mine, "--version").unwrap(), "2025.01.01");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "nothing left behind");
    }

    #[cfg(unix)]
    #[test]
    fn one_that_wont_run_is_refused() {
        let broken = b"#!/bin/sh\nexit 3\n".to_vec();
        let at = serve(HashMap::from([("/y/SHA2-256SUMS".to_string(), format!("{}  yt-dlp_test\n", sum(&broken)).into_bytes()), ("/y/yt-dlp_test".to_string(), broken)]));
        let d = tempfile::tempdir().unwrap();
        let no = AtomicBool::new(false);
        let e = install(d.path(), &sources(&at), true, false, &no, &mut |_| {}).unwrap_err();
        assert!(e.contains("won't run"), "{e}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn cancelling_leaves_nothing() {
        let body = vec![7u8; 4 * 1024 * 1024];
        let at = serve(HashMap::from([("/y/SHA2-256SUMS".to_string(), format!("{}  yt-dlp_test\n", sum(&body)).into_bytes()), ("/y/yt-dlp_test".to_string(), body)]));
        let d = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(true);
        let e = install(d.path(), &sources(&at), true, false, &stop, &mut |_| {}).unwrap_err();
        assert_eq!(e, "Cancelled.");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn big_files_come_down_whole() {
        // More than the limit the HTTP library puts on text.
        let body = vec![9u8; 12 * 1024 * 1024];
        let want = sum(&body);
        let at = serve(HashMap::from([("/big".to_string(), body)]));
        let d = tempfile::tempdir().unwrap();
        let to = d.path().join("big");
        let no = AtomicBool::new(false);
        let mut last = SetupProgress::default();
        assert_eq!(download(&format!("{at}/big"), &to, "big", &no, &mut |p| last = p).unwrap(), want);
        assert_eq!(std::fs::metadata(&to).unwrap().len(), 12 * 1024 * 1024);
        assert_eq!((last.done, last.total), (12 * 1024 * 1024, 12 * 1024 * 1024));
    }

    /// The real releases name what `assets` expects, for every system. Needs the
    /// network, and fetches only their checksum lists:
    /// `cargo test -p fatstick-youtube -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_releases_have_what_is_expected() {
        let list = get_text(&format!("{YTDLP_RELEASES}/SHA2-256SUMS")).unwrap();
        for name in ["yt-dlp_linux", "yt-dlp_linux_aarch64", "yt-dlp.exe", "yt-dlp_macos"] {
            assert!(sum_for(&list, name).is_some(), "{name}");
        }
        for name in ["deno-x86_64-unknown-linux-gnu.zip", "deno-aarch64-unknown-linux-gnu.zip", "deno-x86_64-pc-windows-msvc.zip", "deno-aarch64-apple-darwin.zip", "deno-x86_64-apple-darwin.zip"] {
            let text = get_text(&format!("{DENO_RELEASES}/{name}.sha256sum")).unwrap();
            assert!(only_sum(&text).is_some(), "{name}: {text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn finds_an_own_copy_first() {
        let d = tempfile::tempdir().unwrap();
        let mine = d.path().join("yt-dlp");
        std::fs::write(&mine, b"#!/bin/sh\necho 2026.10.26\n").unwrap();
        make_runnable(&mine).unwrap();
        let f = find("yt-dlp", "--version", d.path(), "FATSTICK_TEST_NOT_SET");
        assert_eq!(f, Some(Program { path: mine, version: "2026.10.26".into(), managed: true }));
        // A copy that won't run doesn't count.
        let e = tempfile::tempdir().unwrap();
        std::fs::write(e.path().join("nothing-by-this-name"), b"x").unwrap();
        assert_eq!(find("nothing-by-this-name", "--version", e.path(), "FATSTICK_TEST_NOT_SET"), None);
    }
}
