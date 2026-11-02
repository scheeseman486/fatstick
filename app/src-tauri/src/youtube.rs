//! YouTube in the app: searching it, fetching a song's audio (never its video:
//! see `fatstick_youtube`) into the cache to hear it, and keeping it, which
//! tags it and copies it into the music folder, into the library. Songs kept
//! are remembered by video, so a song isn't fetched or filed twice.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use fatstick_audio::Tools;
use fatstick_library::folder::inside;
use fatstick_library::{scan, Library, Track};
use fatstick_youtube::setup::{self, Found, Program, SetupProgress, Sources};
use fatstick_youtube::{Fetched, Hit, Progress, Store, Ytdlp};
use serde::Serialize;

use crate::folder::MusicFolder;

/// Which track each video became: { video id: track id }. Kept on this side,
/// like the excluded devices.
pub const KEPT: &str = "youtubeKept";
/// Where kept songs go, relative to the music folder.
const FOLDER: &str = "youtubeFolder";
const DEFAULT_FOLDER: &str = "YouTube";
/// When the app's yt-dlp was last checked for a new version.
const CHECKED: &str = "youtubeChecked";
const CHECK_EVERY_MS: u64 = 24 * 3600 * 1000;
/// Songs heard but not kept, at most this much, least recently used going first.
const STORE_CAP: u64 = 1_000_000_000;
const FETCHES_AT_ONCE: usize = 3;

type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

#[derive(Default)]
struct Flight {
    cancel: AtomicBool,
    result: Mutex<Option<Result<Fetched, String>>>,
    done: Condvar,
}

pub struct Youtube {
    lib: Arc<Mutex<Library>>,
    tools: Option<Tools>,
    music: Arc<MusicFolder>,
    emit: Emit,
    pub store: Store,
    /// The app's own copies of yt-dlp and Deno.
    own: PathBuf,
    ytdlp_cache: PathBuf,
    found: Mutex<Option<Found>>,
    /// Songs being fetched: asking again waits for the same download.
    flights: Mutex<HashMap<String, Arc<Flight>>>,
    running: Mutex<usize>,
    free: Condvar,
    keeping: Mutex<()>,
    setting_up: AtomicBool,
    setup_cancel: AtomicBool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub ytdlp: Option<Program>,
    pub deno: Option<Program>,
    /// A yt-dlp of the person's own, too old to work with YouTube now.
    pub ytdlp_old: bool,
    /// Builds of yt-dlp and Deno exist for this system.
    pub can_get: bool,
    pub setting_up: bool,
    /// Where kept songs go, once there's a music folder.
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitView {
    #[serde(flatten)]
    pub hit: Hit,
    /// The track this video became, if it's in the library.
    pub track_id: Option<String>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Youtube {
    pub fn new(lib: Arc<Mutex<Library>>, tools: Option<Tools>, music: Arc<MusicFolder>, data_dir: &Path, cache_dir: &Path, emit: impl Fn(&str, serde_json::Value) + Send + Sync + 'static) -> Arc<Youtube> {
        Arc::new(Youtube {
            lib,
            tools,
            music,
            emit: Arc::new(emit),
            store: Store::new(cache_dir),
            own: data_dir.join("tools"),
            ytdlp_cache: cache_dir.join("yt-dlp"),
            found: Mutex::new(None),
            flights: Mutex::new(HashMap::new()),
            running: Mutex::new(0),
            free: Condvar::new(),
            keeping: Mutex::new(()),
            setting_up: AtomicBool::new(false),
            setup_cancel: AtomicBool::new(false),
        })
    }

    /// What's installed. The first look runs the programs, so it's never on the
    /// window's thread; the answer is kept until something changes it.
    fn found(&self) -> Found {
        let known = self.found.lock().unwrap().clone();
        known.unwrap_or_else(|| self.look_again())
    }

    fn look_again(&self) -> Found {
        let f = setup::locate(&self.own);
        *self.found.lock().unwrap() = Some(f.clone());
        f
    }

    pub fn status(self: &Arc<Self>) -> Status {
        let f = self.found();
        self.check_for_update(&f);
        Status {
            ytdlp_old: f.ytdlp.as_ref().is_some_and(|y| !y.managed && !fatstick_youtube::is_current(&y.version)),
            ytdlp: f.ytdlp,
            deno: f.deno,
            can_get: setup::assets().is_some(),
            setting_up: self.setting_up.load(Ordering::SeqCst),
            folder: self.folder().map(|p| p.display().to_string()),
        }
    }

    fn ytdlp(&self) -> Result<Ytdlp, String> {
        let f = self.found();
        let y = f.ytdlp.ok_or("yt-dlp isn't set up yet.")?;
        Ok(Ytdlp { path: y.path, version: y.version, deno: f.deno.map(|d| d.path), cache_dir: self.ytdlp_cache.clone() })
    }

    // ---- getting yt-dlp and Deno -------------------------------------------------------

    /// Get whatever is missing (asked for by the person, from the window).
    pub fn set_up(self: &Arc<Self>) -> Result<Status, String> {
        let sources = Sources::official().ok_or("There's no ready-made yt-dlp for this system. Install yt-dlp yourself and put it on the PATH.")?;
        if self.setting_up.swap(true, Ordering::SeqCst) {
            return Err("It's being set up already.".into());
        }
        self.setup_cancel.store(false, Ordering::SeqCst);
        let f = self.look_again();
        let ytdlp = f.ytdlp.as_ref().map_or(true, |y| !y.managed && !fatstick_youtube::is_current(&y.version));
        let deno = f.deno.is_none();
        let emit = self.emit.clone();
        let r = setup::install(&self.own, &sources, ytdlp, deno, &self.setup_cancel, &mut progress_to(emit));
        self.setting_up.store(false, Ordering::SeqCst);
        self.look_again();
        r?;
        Ok(self.status())
    }

    pub fn cancel_setup(&self) {
        self.setup_cancel.store(true, Ordering::SeqCst);
    }

    /// The app's yt-dlp brought up to date, when asked. `true` if it changed.
    pub fn update(self: &Arc<Self>) -> Result<(bool, Status), String> {
        let f = self.look_again();
        if !f.ytdlp.is_some_and(|y| y.managed) {
            return Err("This yt-dlp is one you installed yourself, so update it the way you installed it.".into());
        }
        let sources = Sources::official().ok_or("There's no ready-made yt-dlp for this system.")?;
        if self.setting_up.swap(true, Ordering::SeqCst) {
            return Err("It's being set up already.".into());
        }
        self.setup_cancel.store(false, Ordering::SeqCst);
        let r = setup::update_ytdlp(&self.own, &sources, &self.setup_cancel, &mut progress_to(self.emit.clone()));
        self.setting_up.store(false, Ordering::SeqCst);
        self.look_again();
        let changed = r?;
        let _ = self.lib.lock().unwrap().set_setting(CHECKED, &now_ms().to_string());
        Ok((changed, self.status()))
    }

    /// YouTube changes and yt-dlp follows, so the app's copy is looked at once
    /// a day when YouTube is used: a quiet check, and a new copy if there is one.
    fn check_for_update(self: &Arc<Self>, f: &Found) {
        if !f.ytdlp.as_ref().is_some_and(|y| y.managed) {
            return;
        }
        let last = self.lib.lock().unwrap().setting(CHECKED).ok().flatten().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        if now_ms().saturating_sub(last) < CHECK_EVERY_MS || self.setting_up.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            me.setup_cancel.store(false, Ordering::SeqCst);
            let r = Sources::official().ok_or(String::new()).and_then(|s| setup::update_ytdlp(&me.own, &s, &me.setup_cancel, &mut |_| {}));
            me.setting_up.store(false, Ordering::SeqCst);
            if let Ok(changed) = r {
                let _ = me.lib.lock().unwrap().set_setting(CHECKED, &now_ms().to_string());
                if changed {
                    me.look_again();
                    (me.emit)("youtube-updated", serde_json::to_value(me.status()).unwrap_or_default());
                }
            }
        });
    }

    // ---- searching --------------------------------------------------------------------

    /// A search, or the one video a pasted link is for.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<HitView>, String> {
        let yt = self.ytdlp()?;
        let hits = match fatstick_youtube::video_id(query) {
            Some(id) => vec![yt.look_up(&id)?],
            None => yt.search(query, limit.clamp(5, 100))?,
        };
        Ok(hits.into_iter().map(|hit| HitView { track_id: self.kept_track(&hit.id).map(|t| t.id), hit }).collect())
    }

    // ---- fetching ---------------------------------------------------------------------

    /// A song's audio in the store: from an earlier fetch, from the fetch already
    /// under way, or from a new one when one of the few at a time is free.
    pub fn fetch(self: &Arc<Self>, id: &str) -> Result<Fetched, String> {
        if let Some(f) = self.store.get(id) {
            return Ok(f);
        }
        let (flight, first) = {
            let mut m = self.flights.lock().unwrap();
            match m.get(id) {
                Some(f) => (f.clone(), false),
                None => {
                    let f = Arc::new(Flight::default());
                    m.insert(id.to_string(), f.clone());
                    (f, true)
                }
            }
        };
        if !first {
            let mut r = flight.result.lock().unwrap();
            while r.is_none() {
                r = flight.done.wait(r).unwrap();
            }
            return r.clone().unwrap();
        }
        let r = self.fly(id, &flight);
        *flight.result.lock().unwrap() = Some(r.clone());
        flight.done.notify_all();
        self.flights.lock().unwrap().remove(id);
        // Room for it, among those not being fetched or heard.
        self.store.trim(STORE_CAP, &self.busy());
        r
    }

    fn fly(&self, id: &str, flight: &Flight) -> Result<Fetched, String> {
        let yt = self.ytdlp()?;
        {
            let mut n = self.running.lock().unwrap();
            while *n >= FETCHES_AT_ONCE {
                if flight.cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled.".into());
                }
                n = self.free.wait_timeout(n, std::time::Duration::from_millis(200)).unwrap().0;
            }
            *n += 1;
        }
        let emit = self.emit.clone();
        let id2 = id.to_string();
        let mut on = move |p: Progress| emit("youtube-progress", serde_json::json!({ "id": id2, "done": p.done, "total": p.total }));
        let r = yt.fetch(&self.store, self.tools.as_ref(), id, &flight.cancel, &mut on);
        *self.running.lock().unwrap() -= 1;
        self.free.notify_all();
        r
    }

    fn busy(&self) -> HashSet<String> {
        self.flights.lock().unwrap().keys().cloned().collect()
    }

    pub fn cancel(&self, id: &str) {
        if let Some(f) = self.flights.lock().unwrap().get(id) {
            f.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Everything heard and not kept, gone (the menu's "Clear the cache").
    pub fn clear(&self) {
        self.store.trim(0, &self.busy());
    }

    // ---- keeping ----------------------------------------------------------------------

    fn kept(&self) -> HashMap<String, String> {
        self.lib.lock().unwrap().setting(KEPT).ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
    }

    /// The library's copy of a video, if it's still there.
    fn kept_track(&self, video: &str) -> Option<Track> {
        let tid = self.kept().remove(video)?;
        self.lib.lock().unwrap().track(&tid).ok().flatten().filter(|t| t.exists)
    }

    /// Where kept songs go, once there's a music folder.
    fn folder(&self) -> Option<PathBuf> {
        let root = self.music.root()?;
        let name = self.lib.lock().unwrap().setting(FOLDER).ok().flatten().unwrap_or_else(|| DEFAULT_FOLDER.to_string());
        Some(root.join(name))
    }

    /// Kept songs go here from now on: a folder in the music folder.
    pub fn set_folder(self: &Arc<Self>, path: &Path) -> Result<Status, String> {
        let root = self.music.root().ok_or("Choose your music folder first.")?;
        let p = std::fs::canonicalize(path).map_err(|e| format!("Couldn't use {}: {e}", path.display()))?;
        if !inside(&root, &p) && p != root {
            return Err("Songs from YouTube go in your music folder, so that's a folder in there.".into());
        }
        let rel = p.strip_prefix(&root).map_err(|e| e.to_string())?;
        self.lib.lock().unwrap().set_setting(FOLDER, &rel.display().to_string()).map_err(|e| e.to_string())?;
        Ok(self.status())
    }

    /// Into the library: tagged and copied into the music folder, once. Fetches
    /// it first if it isn't here.
    pub fn keep(self: &Arc<Self>, id: &str) -> Result<Track, String> {
        if let Some(t) = self.kept_track(id) {
            return Ok(t);
        }
        let dir = self.folder().ok_or("Choose your music folder first.")?;
        let f = self.fetch(id)?;
        let _one_at_a_time = self.keeping.lock().unwrap();
        if let Some(t) = self.kept_track(id) {
            return Ok(t);
        }
        let tools = self.tools.clone().ok_or("ffmpeg isn't available.")?;
        let path = fatstick_youtube::keep(&self.store, &f, &dir)?;
        let tid = scan::read_file(&self.lib, &tools, &path).map_err(|e| e.to_string())?;
        let lib = self.lib.lock().unwrap();
        let mut kept: HashMap<String, String> = lib.setting(KEPT).ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
        kept.insert(id.to_string(), tid.clone());
        lib.set_setting(KEPT, &serde_json::to_string(&kept).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        lib.track(&tid).map_err(|e| e.to_string())?.ok_or_else(|| "The track went missing.".into())
    }
}

fn progress_to(emit: Emit) -> impl FnMut(SetupProgress) {
    move |p| emit("youtube-setup", serde_json::to_value(&p).unwrap_or_default())
}
