//! YouTube, through yt-dlp: searching it, and fetching a song's audio.
//!
//! **Audio only.** Every download asks yt-dlp for an audio-only format by name
//! ([`AUDIO_ANY`], [`AUDIO_AAC`]) with nothing to fall back to, so a video with
//! no audio-only stream fails instead of fetching the video. yt-dlp's own config
//! files are ignored, so nothing on the computer can change that, and the file
//! that arrives is checked with ffprobe and thrown away if it holds anything
//! but audio.
//!
//! The audio stream is kept as YouTube has it, never re-encoded: Opus goes from
//! its WebM into an Ogg file (`.opus`), AAC into a plain `.m4a`. Both can carry
//! tags and both are formats the library reads. An ffmpeg without the Ogg
//! muxer (a build from before this was added) gets AAC instead.
//!
//! A song is fetched into a [`Store`] in the cache folder first, so hearing one
//! leaves nothing in the music folder and needs nothing fetched again when it's
//! kept. [`keep`] tags it and copies it into the music folder.
//!
//! Only YouTube video ids reach yt-dlp as links (see [`video_id`]), rebuilt into
//! a watch URL, after a `--`: nothing typed can become an option or send yt-dlp
//! to another site.

pub mod meta;
pub mod setup;

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use fatstick_audio::Tools;
use serde::{Deserialize, Serialize};

pub use meta::SongTags;

/// The best audio-only stream, Opus first as it's the better sound at the
/// bitrates YouTube offers. Nothing here can select a video format.
pub const AUDIO_ANY: &str = "bestaudio[acodec^=opus]/bestaudio[acodec^=mp4a]";
/// The same without Opus, for an ffmpeg that can't write Ogg.
pub const AUDIO_AAC: &str = "bestaudio[acodec^=mp4a]";

const SEARCH_LIMIT: Duration = Duration::from_secs(60);
const FETCH_LIMIT: Duration = Duration::from_secs(30 * 60);

/// `--ignore-config` and the rest of what every run gets is in [`Ytdlp::common`].
#[derive(Debug, Clone)]
pub struct Ytdlp {
    pub path: PathBuf,
    pub version: String,
    /// Runs the code YouTube asks to be run. Without it yt-dlp still works, but
    /// some songs fail.
    pub deno: Option<PathBuf>,
    /// yt-dlp's own cache: what it has worked out about YouTube's player.
    pub cache_dir: PathBuf,
}

/// One result of a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration: f64,
    pub views: Option<u64>,
}

/// A download under way.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// A song in the [`Store`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fetched {
    pub id: String,
    /// The file's name in the store.
    pub file: String,
    pub duration: f64,
    pub tags: SongTags,
}

/// Whether this is the shape of a YouTube video id.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

/// The video a pasted link is for, when it's a link to a YouTube video.
pub fn video_id(input: &str) -> Option<String> {
    let s = input.trim();
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).or_else(|| host.strip_prefix("music.")).unwrap_or(&host);
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let route = route.split('#').next().unwrap_or("").trim_end_matches('/');
    let id = match host {
        "youtu.be" => route.to_string(),
        "youtube.com" | "youtube-nocookie.com" => {
            if route == "watch" {
                query.split(['&', '#']).find_map(|kv| kv.strip_prefix("v=")).unwrap_or("").to_string()
            } else {
                let (kind, id) = route.split_once('/')?;
                if !["shorts", "live", "embed", "v"].contains(&kind) {
                    return None;
                }
                id.to_string()
            }
        }
        _ => return None,
    };
    is_video_id(&id).then_some(id)
}

/// A date version ("2026.10.26") as numbers.
fn version_of(v: &str) -> Option<(u32, u32, u32)> {
    let mut p = v.trim().split('.').map(|x| x.parse::<u32>().ok());
    Some((p.next()??, p.next()??, p.next()??))
}

/// Whether this yt-dlp is one that knows about JavaScript runtimes (newer than
/// November 2025). An older one has lost touch with YouTube anyway.
pub fn is_current(version: &str) -> bool {
    version_of(version).is_some_and(|v| v >= (2025, 11, 12))
}

impl Ytdlp {
    /// What every run gets. The user's own yt-dlp config is ignored, so it can't
    /// ask for video, or for anything else this module doesn't expect.
    fn common(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--ignore-config".into(), "--no-warnings".into(), "--cache-dir".into(), self.cache_dir.clone().into()];
        if let (Some(deno), true) = (&self.deno, is_current(&self.version)) {
            let mut v = OsString::from("deno:");
            v.push(deno);
            a.extend(["--js-runtimes".into(), v]);
        }
        a
    }

    pub fn search_args(&self, query: &str, limit: usize) -> Vec<OsString> {
        let clean: String = query.chars().filter(|c| !c.is_control()).take(200).collect();
        let mut a = self.common();
        a.extend(["--flat-playlist".into(), "--dump-single-json".into(), "--".into()]);
        a.push(format!("ytsearch{limit}:{}", clean.trim()).into());
        a
    }

    fn link_args(&self, id: &str) -> Vec<OsString> {
        let mut a = self.common();
        a.extend(["--no-playlist".into(), "--dump-single-json".into(), "-f".into(), AUDIO_ANY.into(), "--".into(), watch_url(id).into()]);
        a
    }

    /// `opus`: whether Opus can be made into a file here.
    pub fn fetch_args(&self, id: &str, into: &Path, opus: bool) -> Vec<OsString> {
        let mut a = self.common();
        a.extend([
            "--no-playlist".into(),
            "--no-simulate".into(),
            "--dump-json".into(),
            "--format".into(),
            (if opus { AUDIO_ANY } else { AUDIO_AAC }).into(),
            // The fetched file is finished here, by the app's own ffmpeg.
            "--fixup".into(),
            "never".into(),
            "--no-mtime".into(),
            "--newline".into(),
            "--progress".into(),
            "--progress-template".into(),
            "download:fatstick %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s".into(),
            "--output".into(),
        ]);
        a.push(into.join(format!("{id}.%(ext)s")).into());
        a.extend(["--".into(), watch_url(id).into()]);
        a
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
        let out = run(self.command(self.search_args(query, limit)), None, SEARCH_LIMIT, &mut |_| {})?;
        parse_search(&out.stdout)
    }

    /// The one video a pasted link is for.
    pub fn look_up(&self, id: &str) -> Result<Hit, String> {
        if !is_video_id(id) {
            return Err("That isn't a YouTube video.".into());
        }
        let out = run(self.command(self.link_args(id)), None, SEARCH_LIMIT, &mut |_| {})?;
        let v: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|e| format!("yt-dlp's answer couldn't be read: {e}"))?;
        hit_of(&v).ok_or_else(|| "That video can't be fetched (it may be a live stream).".to_string())
    }

    /// Fetch a song's audio into the store; `Ok` as soon as it's there.
    pub fn fetch(&self, store: &Store, tools: Option<&Tools>, id: &str, cancel: &AtomicBool, progress: &mut dyn FnMut(Progress)) -> Result<Fetched, String> {
        let opus = tools.is_some_and(|t| t.can_mux("opus"));
        self.fetch_as(store, tools, id, opus, cancel, progress)
    }

    /// [`Ytdlp::fetch`], saying whether Opus can be made into a file here.
    pub fn fetch_as(&self, store: &Store, tools: Option<&Tools>, id: &str, opus: bool, cancel: &AtomicBool, progress: &mut dyn FnMut(Progress)) -> Result<Fetched, String> {
        if !is_video_id(id) {
            return Err("That isn't a YouTube video.".into());
        }
        if let Some(f) = store.get(id) {
            return Ok(f);
        }
        let tools = tools.ok_or("ffmpeg isn't available, so YouTube's audio can't be read.")?;
        std::fs::create_dir_all(&store.dir).map_err(|e| format!("Couldn't make {}: {e}", store.dir.display()))?;
        let mut info = None;
        let mut on_line = |line: &str| {
            if line.starts_with('{') {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    info = Some(v);
                }
            } else if let Some(p) = parse_progress(line) {
                progress(p);
            }
        };
        let cmd = self.command(self.fetch_args(id, &store.dir, opus));
        run(cmd, Some(cancel), FETCH_LIMIT, &mut on_line)?;
        let info = info.ok_or("yt-dlp finished without saying what it fetched.")?;
        let got = store.fetched_file(id).ok_or("yt-dlp finished, but the file isn't there.")?;
        let file = match finish(tools, &got, id) {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_file(&got);
                return Err(e);
            }
        };
        let duration = info.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
        let f = Fetched { id: id.to_string(), file: file.file_name().unwrap_or_default().to_string_lossy().into_owned(), duration, tags: meta::tags_of(&info) };
        store.put(&f)?;
        Ok(f)
    }

    fn command(&self, args: Vec<OsString>) -> Command {
        let mut c = Command::new(&self.path);
        c.args(args).stdin(Stdio::null());
        quiet_child(&mut c);
        c
    }
}

/// The downloaded stream, made into the file the library keeps: the same audio,
/// in a container that takes tags. Refuses anything that isn't audio alone.
fn finish(tools: &Tools, got: &Path, id: &str) -> Result<PathBuf, String> {
    let ext = got.extension().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
    // (the muxer and the extension, or nothing if the file is fine as it is)
    let plan = match ext.as_str() {
        "webm" | "mka" | "ogg" | "opus" => Some(("opus", "opus")),
        "m4a" | "mp4" if tools.can_mux("ipod") => Some(("ipod", "m4a")),
        "m4a" => None,
        other => return Err(format!("YouTube sent a .{other} file, which the library can't keep.")),
    };
    let done = match plan {
        None => got.to_path_buf(),
        Some((muxer, new_ext)) => {
            let dir = got.parent().unwrap_or(Path::new("."));
            let tmp = dir.join(format!("{id}.remux.{new_ext}"));
            let mut c = tools.ffmpeg();
            c.args(["-y", "-loglevel", "error", "-i"]).arg(got).args(["-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", "copy", "-f", muxer]);
            if muxer == "ipod" {
                c.args(["-movflags", "+faststart"]);
            }
            c.arg(&tmp);
            if let Err(e) = fatstick_audio::tools::run(&mut c) {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("The audio couldn't be unpacked: {e}"));
            }
            let dst = dir.join(format!("{id}.{new_ext}"));
            let _ = std::fs::remove_file(got);
            std::fs::rename(&tmp, &dst).map_err(|e| format!("Couldn't save the audio: {e}"))?;
            dst
        }
    };
    if let Err(e) = audio_alone(tools, &done) {
        let _ = std::fs::remove_file(&done);
        return Err(e);
    }
    Ok(done)
}

/// Whether a file holds audio and nothing else, by ffprobe.
pub fn audio_alone(tools: &Tools, path: &Path) -> Result<(), String> {
    let mut c = tools.ffprobe();
    c.args(["-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0"]).arg(path);
    let o = fatstick_audio::tools::run(&mut c).map_err(|e| format!("The file couldn't be checked: {e}"))?;
    let text = String::from_utf8_lossy(&o.stdout);
    let kinds: Vec<&str> = text.lines().map(|l| l.trim().trim_end_matches(',')).filter(|l| !l.is_empty()).collect();
    if kinds.is_empty() || kinds.iter().any(|k| *k != "audio") {
        return Err(format!("That file holds more than audio ({}), so it wasn't kept.", kinds.join(", ")));
    }
    Ok(())
}

// ---- reading yt-dlp's answers --------------------------------------------------------

fn hit_of(v: &serde_json::Value) -> Option<Hit> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let id = s("id");
    if !is_video_id(&id) {
        return None;
    }
    // Live streams and premieres have no file to fetch.
    if matches!(v.get("live_status").and_then(|x| x.as_str()), Some("is_live" | "is_upcoming" | "post_live")) || v.get("is_live").and_then(|x| x.as_bool()) == Some(true) {
        return None;
    }
    let duration = v.get("duration").and_then(|d| d.as_f64()).filter(|d| *d > 0.0)?;
    let channel = [s("channel"), s("uploader")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
    Some(Hit { title: s("title"), channel, duration, views: v.get("view_count").and_then(|x| x.as_u64()), id })
}

/// The results of `--flat-playlist --dump-single-json ytsearchN:...`.
pub fn parse_search(stdout: &str) -> Result<Vec<Hit>, String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| format!("yt-dlp's answer couldn't be read: {e}"))?;
    let mut seen = HashSet::new();
    Ok(v.get("entries").and_then(|e| e.as_array()).map(|e| e.iter().filter_map(hit_of).filter(|h| seen.insert(h.id.clone())).collect()).unwrap_or_default())
}

/// A line from the progress template in [`Ytdlp::fetch_args`].
pub fn parse_progress(line: &str) -> Option<Progress> {
    let mut p = line.strip_prefix("fatstick ")?.split_whitespace();
    let num = |x: Option<&str>| x.and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0);
    let done = num(p.next())?;
    let (exact, estimate) = (num(p.next()), num(p.next()));
    Some(Progress { done: done as u64, total: exact.or(estimate).unwrap_or(0.0) as u64 })
}

/// The reason in what yt-dlp said on stderr, in a sentence someone can act on.
pub fn explain(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| l.starts_with("ERROR:")).or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("");
    let said = line.trim_start_matches("ERROR:").trim();
    // "[youtube] abc: the reason" is the reason.
    let said = said.strip_prefix('[').and_then(|s| s.split_once("] ")).map_or(said, |x| x.1);
    let said = match said.split_once(": ") {
        Some((a, b)) if is_video_id(a) => b,
        _ => said,
    };
    let low = said.to_lowercase();
    if low.contains("requested format is not available") {
        "That one has no audio-only stream here, so nothing was fetched.".into()
    } else if low.contains("not a bot") || low.contains("sign in to confirm") {
        "YouTube wants a sign-in from this connection before it gives this up. Try again later.".into()
    } else if low.contains("http error 403") || low.contains("unable to extract") || low.contains("nsig") || low.contains("signature") {
        "YouTube refused the download. Updating yt-dlp usually fixes that.".into()
    } else if low.contains("getaddrinfo") || low.contains("name resolution") || low.contains("network is unreachable") || low.contains("timed out") || low.contains("connection") {
        "Couldn't reach YouTube. Check the connection and try again.".into()
    } else if said.is_empty() {
        "yt-dlp stopped without saying why.".into()
    } else {
        said.to_string()
    }
}

// ---- running it ------------------------------------------------------------------------

#[cfg(windows)]
fn quiet_child(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
fn quiet_child(c: &mut Command) {
    // Its own process group: the standalone yt-dlp is a small program that
    // starts the real one, and stopping a download has to stop them all.
    use std::os::unix::process::CommandExt;
    c.process_group(0);
}

#[cfg(windows)]
fn stop_all(child: &mut Child) {
    let mut c = Command::new("taskkill");
    c.args(["/T", "/F", "/PID", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
    quiet_child(&mut c);
    let _ = c.status();
    let _ = child.kill();
}
#[cfg(not(windows))]
fn stop_all(child: &mut Child) {
    let _ = Command::new("kill").args(["-TERM", "--", &format!("-{}", child.id())]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = child.kill();
}

struct Ran {
    stdout: String,
}

/// Run to the end, line by line. Stopped by `cancel` or the time limit; either
/// way the program and anything it started are gone when this returns.
fn run(mut cmd: Command, cancel: Option<&AtomicBool>, limit: Duration, on_line: &mut dyn FnMut(&str)) -> Result<Ran, String> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Couldn't start yt-dlp: {e}"))?;
    let stdout = child.stdout.take().ok_or("yt-dlp has no output")?;
    let mut stderr = child.stderr.take().ok_or("yt-dlp has no output")?;
    let said = Arc::new(Mutex::new(String::new()));
    let said2 = said.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        *said2.lock().unwrap() = String::from_utf8_lossy(&buf).into_owned();
    });
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            if tx.send(String::from_utf8_lossy(&line).trim_end().to_string()).is_err() {
                break;
            }
        }
    });
    let t0 = Instant::now();
    let mut text = String::new();
    let mut stopped: Option<(&str, Instant)> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                on_line(&line);
                text.push_str(&line);
                text.push('\n');
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if stopped.is_none() {
            let why = if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                Some("Cancelled.")
            } else if t0.elapsed() > limit {
                Some("yt-dlp took too long, so it was stopped.")
            } else {
                None
            };
            if let Some(why) = why {
                stop_all(&mut child);
                stopped = Some((why, Instant::now()));
            }
        }
        // Whatever it left running holds the output open: don't wait on that for long.
        if stopped.is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2)) {
            break;
        }
    }
    if let Some((why, _)) = stopped {
        let _ = child.wait();
        return Err(why.to_string());
    }
    let status = child.wait().map_err(|e| format!("yt-dlp: {e}"))?;
    if !status.success() {
        // Give the thread reading stderr its moment.
        for _ in 0..20 {
            if !said.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        return Err(explain(&said.lock().unwrap()));
    }
    Ok(Ran { stdout: text })
}

// ---- the store ---------------------------------------------------------------------------

/// Songs fetched from YouTube, in the cache folder: `<id>.opus` or `<id>.m4a`,
/// and `<id>.json` saying what it is. Deleting the folder is always safe.
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(cache_dir: &Path) -> Store {
        Store { dir: cache_dir.join("youtube") }
    }

    fn info_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// The song, if it's whole; marks it as just used.
    pub fn get(&self, id: &str) -> Option<Fetched> {
        if !is_video_id(id) {
            return None;
        }
        let info = self.info_path(id);
        let f: Fetched = serde_json::from_slice(&std::fs::read(&info).ok()?).ok()?;
        if !self.path_of(&f).is_file() {
            return None;
        }
        let _ = std::fs::OpenOptions::new().write(true).open(&info).and_then(|x| x.set_modified(SystemTime::now()));
        Some(f)
    }

    pub fn path_of(&self, f: &Fetched) -> PathBuf {
        self.dir.join(&f.file)
    }

    fn put(&self, f: &Fetched) -> Result<(), String> {
        let tmp = self.dir.join(format!("{}.json.tmp", f.id));
        std::fs::write(&tmp, serde_json::to_vec(f).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.info_path(&f.id)).map_err(|e| e.to_string())
    }

    /// What yt-dlp left for this song: anything but its working files.
    pub fn fetched_file(&self, id: &str) -> Option<PathBuf> {
        let head = format!("{id}.");
        std::fs::read_dir(&self.dir).ok()?.flatten().map(|e| e.path()).find(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            n.starts_with(&head) && !n.contains(".remux.") && ![".json", ".part", ".ytdl", ".tmp", ".temp"].iter().any(|x| n.ends_with(x)) && !n.contains(".part-")
        })
    }

    /// Delete the songs used least recently until the store is at most `cap`
    /// bytes, leaving `busy` ids alone, and working files older than a day.
    pub fn trim(&self, cap: u64, busy: &HashSet<String>) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let old = SystemTime::now() - Duration::from_secs(24 * 3600);
        let mut songs: Vec<(String, u64, SystemTime)> = Vec::new();
        let mut files: Vec<(PathBuf, String, u64, SystemTime)> = Vec::new();
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            let id = name.split('.').next().unwrap_or("").to_string();
            let when = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if name.ends_with(".json") {
                songs.push((id.clone(), 0, when));
            }
            files.push((e.path(), id, m.len(), when));
        }
        let known: HashSet<String> = songs.iter().map(|s| s.0.clone()).collect();
        for (path, id, _, when) in &files {
            if !known.contains(id) && !busy.contains(id) && *when < old {
                let _ = std::fs::remove_file(path);
            }
        }
        for s in &mut songs {
            s.1 = files.iter().filter(|f| f.1 == s.0).map(|f| f.2).sum();
        }
        let mut total: u64 = songs.iter().map(|s| s.1).sum();
        songs.sort_by_key(|s| s.2);
        for (id, len, _) in songs {
            if total <= cap {
                break;
            }
            if busy.contains(&id) {
                continue;
            }
            for f in files.iter().filter(|f| f.1 == id) {
                let _ = std::fs::remove_file(&f.0);
            }
            total -= len;
        }
    }
}

/// Tag a fetched song and copy it into `dir`, under a name from its tags. The
/// file appears whole, under its final name, or not at all; nothing in `dir` is
/// ever replaced. Returns where it went.
pub fn keep(store: &Store, f: &Fetched, dir: &Path) -> Result<PathBuf, String> {
    let src = store.path_of(f);
    let ext = src.extension().unwrap_or_default().to_string_lossy().to_string();
    // The tags go in here, so a slow network drive only sees the finished file.
    let edit = meta::tag_edit(&f.tags, &watch_url(&f.id));
    fatstick_audio::tagwrite::write_tags(&src, &edit).map_err(|e| format!("Couldn't tag the file: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".fatstick-youtube-{}.{ext}", f.id));
    std::fs::copy(&src, &tmp).map_err(|e| format!("Couldn't copy it into {}: {e}", dir.display()))?;
    let stem = meta::file_stem(&f.tags, &f.id);
    let mut n = 0;
    loop {
        let name = match n {
            0 => format!("{stem}.{ext}"),
            1 => format!("{stem} [{}].{ext}", f.id),
            _ => format!("{stem} [{}] {n}.{ext}", f.id),
        };
        let dst = dir.join(name);
        if !dst.exists() {
            return match std::fs::rename(&tmp, &dst) {
                Ok(()) => Ok(dst),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    Err(format!("Couldn't save it as {}: {e}", dst.display()))
                }
            };
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt() -> Ytdlp {
        Ytdlp { path: "yt-dlp".into(), version: "2026.10.26".into(), deno: Some("/opt/deno".into()), cache_dir: "/c".into() }
    }
    fn text(a: &[OsString]) -> Vec<String> {
        a.iter().map(|x| x.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_fetch_asks_for_audio_alone() {
        for opus in [true, false] {
            let a = text(&yt().fetch_args("abcdefghijk", Path::new("/s"), opus));
            let at = a.iter().position(|x| x == "--format").unwrap();
            let f = &a[at + 1];
            // Every alternative is an audio-only format.
            assert!(f.split('/').all(|x| x.starts_with("bestaudio[")), "{f}");
            assert_eq!(a.iter().filter(|x| *x == "--format" || *x == "-f").count(), 1);
            assert!(a.contains(&"--ignore-config".to_string()));
            for video in ["bestvideo", "--merge-output-format", "--remux-video", "--recode-video", "--embed-thumbnail", "--write-thumbnail"] {
                assert!(!a.iter().any(|x| x.contains(video)), "{video}");
            }
            assert!(a.contains(&"--no-playlist".to_string()));
            assert_eq!(a[a.len() - 2], "--");
            assert_eq!(a[a.len() - 1], "https://www.youtube.com/watch?v=abcdefghijk");
        }
        assert_eq!(AUDIO_AAC, "bestaudio[acodec^=mp4a]");
    }

    #[test]
    fn what_is_typed_stays_a_search() {
        let a = text(&yt().search_args(" --exec rm\n-rf  ", 25));
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(a[a.len() - 1], "ytsearch25:--exec rm-rf");
        assert!(a.contains(&"--ignore-config".to_string()));
    }

    #[test]
    fn deno_only_for_a_yt_dlp_that_knows_it() {
        let a = text(&yt().search_args("x", 5));
        let at = a.iter().position(|x| x == "--js-runtimes").unwrap();
        assert_eq!(a[at + 1], "deno:/opt/deno");
        let mut old = yt();
        old.version = "2025.09.26".into();
        assert!(!text(&old.search_args("x", 5)).contains(&"--js-runtimes".to_string()));
        assert!(is_current("2025.11.12") && is_current("2026.1.29") && !is_current("2025.10.22") && !is_current("nonsense"));
    }

    #[test]
    fn links() {
        let id = "dQw4w9WgXcQ";
        for l in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?list=PL1&v=dQw4w9WgXcQ&t=4",
            "http://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&si=x",
            "https://youtu.be/dQw4w9WgXcQ?t=3",
            "www.youtube.com/shorts/dQw4w9WgXcQ",
            " https://www.youtube.com/live/dQw4w9WgXcQ/ ",
        ] {
            assert_eq!(video_id(l).as_deref(), Some(id), "{l}");
        }
        for l in ["beautifully", "https://evil.example/watch?v=dQw4w9WgXcQ", "https://www.youtube.com/channel/UC1234567890", "https://www.youtube.com/watch?v=short", "https://youtu.be/a b c d e f", "https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"] {
            assert_eq!(video_id(l), None, "{l}");
        }
    }

    #[test]
    fn reads_a_search() {
        let s = r#"{"_type":"playlist","entries":[
          {"id":"aaaaaaaaaaa","title":"Song","channel":"Chan","duration":200,"view_count":12},
          {"id":"aaaaaaaaaaa","title":"Song again","channel":"Chan","duration":200},
          {"id":"bbbbbbbbbbb","title":"On air","channel":"Chan","duration":null,"live_status":"is_live"},
          {"id":"ccccccccccc","title":"Soon","duration":100,"live_status":"is_upcoming"},
          {"id":"not an id","title":"Channel","duration":100},
          {"id":"ddddddddddd","title":"No channel","uploader":"Up","duration":90.5}
        ]}"#;
        let h = parse_search(s).unwrap();
        assert_eq!(h.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["aaaaaaaaaaa", "ddddddddddd"]);
        assert_eq!(h[0].views, Some(12));
        assert_eq!(h[1].channel, "Up");
        assert!(parse_search("nonsense").is_err());
        assert_eq!(parse_search(r#"{"entries":[]}"#).unwrap(), []);
    }

    #[test]
    fn reads_progress() {
        let p = parse_progress("fatstick 1024 4096 NA").unwrap();
        assert_eq!((p.done, p.total), (1024, 4096));
        let p = parse_progress("fatstick 1024 NA 8192.5").unwrap();
        assert_eq!((p.done, p.total), (1024, 8192));
        assert!(parse_progress("fatstick NA NA NA").is_none());
        assert!(parse_progress("[download] 10%").is_none());
    }

    #[test]
    fn explains_failures() {
        assert!(explain("WARNING: x\nERROR: [youtube] dQw4w9WgXcQ: Requested format is not available. Use --list-formats").contains("no audio-only stream"));
        assert!(explain("ERROR: unable to download video data: HTTP Error 403: Forbidden").contains("Updating yt-dlp"));
        assert!(explain("ERROR: [youtube] dQw4w9WgXcQ: Sign in to confirm you're not a bot").contains("sign-in"));
        assert_eq!(explain("ERROR: [youtube] dQw4w9WgXcQ: Private video"), "Private video");
        assert_eq!(explain(""), "yt-dlp stopped without saying why.");
    }

    #[cfg(unix)]
    #[test]
    fn stopping_stops_everything_it_started() {
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("still-running");
        // A launcher with a child, as the standalone yt-dlp is.
        let script = format!("#!/bin/sh\necho started\n(sleep 2; touch '{}') &\nwait\n", marker.display());
        let p = d.path().join("slow");
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let mut cmd = Command::new(&p);
        cmd.stdin(Stdio::null());
        quiet_child(&mut cmd);
        let t0 = Instant::now();
        let r = run(cmd, Some(&cancel), Duration::from_secs(60), &mut |line| {
            if line == "started" {
                c2.store(true, Ordering::SeqCst);
            }
        });
        assert_eq!(r.err().as_deref(), Some("Cancelled."));
        assert!(t0.elapsed() < Duration::from_secs(4));
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "the child kept running");
    }

    #[test]
    fn store_trims_oldest_and_spares_what_is_busy() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        std::fs::create_dir_all(&s.dir).unwrap();
        let put = |id: &str, age: u64| {
            let f = Fetched { id: id.into(), file: format!("{id}.opus"), duration: 1.0, tags: SongTags::default() };
            std::fs::write(s.dir.join(&f.file), vec![0u8; 1000]).unwrap();
            s.put(&f).unwrap();
            let when = SystemTime::now() - Duration::from_secs(age);
            std::fs::OpenOptions::new().write(true).open(s.info_path(id)).unwrap().set_modified(when).unwrap();
        };
        put("aaaaaaaaaaa", 300);
        put("bbbbbbbbbbb", 200);
        put("ccccccccccc", 100);
        s.trim(2500, &HashSet::from(["aaaaaaaaaaa".to_string()]));
        assert!(s.get("aaaaaaaaaaa").is_some(), "busy");
        assert!(s.get("bbbbbbbbbbb").is_none(), "the oldest that's free");
        assert!(s.get("ccccccccccc").is_some());
        assert!(s.fetched_file("bbbbbbbbbbb").is_none());
        assert_eq!(s.fetched_file("ccccccccccc").unwrap().extension().unwrap(), "opus");
    }

    #[test]
    fn working_files_are_not_the_song() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        std::fs::create_dir_all(&s.dir).unwrap();
        std::fs::write(s.dir.join("aaaaaaaaaaa.webm.part"), b"x").unwrap();
        std::fs::write(s.dir.join("aaaaaaaaaaa.json.tmp"), b"x").unwrap();
        assert!(s.fetched_file("aaaaaaaaaaa").is_none());
        std::fs::write(s.dir.join("aaaaaaaaaaa.webm"), b"x").unwrap();
        assert_eq!(s.fetched_file("aaaaaaaaaaa").unwrap().file_name().unwrap(), "aaaaaaaaaaa.webm");
    }
}
