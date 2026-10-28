// Fatstick desktop app. The window (../ui) owns playlists, profiles and
// sticks and plans each sync; this side reads files, keeps the library,
// measures, converts and caches audio, plays previews, and writes sticks.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod folder;
#[cfg(target_os = "linux")]
mod graphics;
mod player;
mod sticks;
mod sync;
mod work;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fatstick_audio::prepare::Recipe;
use fatstick_audio::{Cache, Tools};
use fatstick_library::{scan, Library, Track};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    lib: Arc<Mutex<Library>>,
    tools: Option<Tools>,
    tools_error: Option<String>,
    cache: Cache,
    work: Arc<work::Work>,
    player: player::Player,
    cancel: Arc<AtomicBool>,
    syncing: AtomicBool,
    data_dir: PathBuf,
    music: Arc<folder::MusicFolder>,
}

type Res<T> = Result<T, String>;

fn err(e: impl ToString) -> String {
    e.to_string()
}

const CACHE_CAP_DEFAULT: u64 = 10_000_000_000;

fn cache_cap(lib: &Library) -> u64 {
    lib.setting("cacheCapBytes").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(CACHE_CAP_DEFAULT)
}

// ---- start-up ---------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitView {
    tracks: Vec<Track>,
    playlists: Vec<serde_json::Value>,
    profiles: Vec<serde_json::Value>,
    sets: Vec<serde_json::Value>,
    syncs: Vec<serde_json::Value>,
    settings: serde_json::Value,
    ffmpeg: Option<String>,
    ffmpeg_error: Option<String>,
    cache: fatstick_audio::cache::CacheStats,
    data_dir: String,
    version: String,
    /// This library's id, which every stick it writes carries.
    library_id: String,
    /// The music folder and its last scan.
    music: folder::FolderInfo,
    /// Files passed on the command line ("Open with").
    open_paths: Vec<String>,
}

#[tauri::command]
fn init(st: State<AppState>) -> Res<InitView> {
    let lib = st.lib.lock().unwrap();
    let mut settings = serde_json::Map::new();
    for k in ["theme", "cacheCapBytes", "currentPlaylist", "currentSet", "columns", "volume"] {
        if let Some(v) = lib.setting(k).map_err(err)? {
            settings.insert(k.into(), v.into());
        }
    }
    Ok(InitView {
        tracks: lib.tracks().map_err(err)?,
        playlists: lib.docs("playlist").map_err(err)?,
        profiles: lib.docs("profile").map_err(err)?,
        sets: lib.docs("set").map_err(err)?,
        syncs: lib.last_syncs().map_err(err)?,
        settings: settings.into(),
        ffmpeg: st.tools.as_ref().map(|t| t.version()),
        ffmpeg_error: st.tools_error.clone(),
        cache: st.cache.stats(),
        data_dir: st.data_dir.display().to_string(),
        version: env!("CARGO_PKG_VERSION").into(),
        library_id: lib.library_id().map_err(err)?,
        music: st.music.info(),
        open_paths: std::env::args().skip(1).filter(|a| !a.starts_with('-') && Path::new(a).exists()).collect(),
    })
}

#[tauri::command]
fn save_docs(st: State<AppState>, kind: String, put: Vec<serde_json::Value>, delete: Vec<String>, order: Vec<String>) -> Res<()> {
    if !["playlist", "profile", "set"].contains(&kind.as_str()) {
        return Err(format!("unknown kind {kind}"));
    }
    st.lib.lock().unwrap().save_docs(&kind, &put, &delete, &order).map_err(err)
}

#[tauri::command]
fn set_setting(st: State<AppState>, key: String, value: String) -> Res<()> {
    st.lib.lock().unwrap().set_setting(&key, &value).map_err(err)
}

// ---- files --------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Added {
    /// Audio files, in the order they should go into a playlist.
    track_ids: Vec<String>,
    tracks: Vec<Track>,
    /// Playlist files among the paths, imported.
    playlists: Vec<scan::Imported>,
    failed: Vec<String>,
}

fn need_tools(st: &AppState) -> Res<Tools> {
    st.tools.clone().ok_or_else(|| format!("ffmpeg isn't available: {}", st.tools_error.clone().unwrap_or_default()))
}

#[tauri::command]
async fn add_paths(app: AppHandle, paths: Vec<String>) -> Res<Added> {
    let st = app.state::<AppState>();
    let tools = need_tools(&st)?;
    let lib = st.lib.clone();
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |p: scan::ScanProgress| {
            let _ = app2.emit("scan-progress", p);
        };
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let (audio, lists) = scan::expand(&paths);
        let mut failed = Vec::new();
        let ids = scan::read_files(&lib, &tools, &audio, &progress);
        let mut tracks = Vec::new();
        for (r, p) in ids.into_iter().zip(&audio) {
            match r.and_then(|id| lib.lock().unwrap().track(&id)) {
                Ok(Some(t)) => tracks.push(t),
                _ => failed.push(p.display().to_string()),
            }
        }
        // Files dropped one by one keep their order; folders go album by album.
        if paths.iter().any(|p| p.is_dir()) {
            scan::album_order(&mut tracks);
        }
        let mut playlists = Vec::new();
        for l in lists {
            match scan::import(&lib, &tools, &l, &progress) {
                Ok(imp) => playlists.push(imp),
                Err(e) => failed.push(format!("{}: {e}", l.display())),
            }
        }
        let mut all = tracks.clone();
        {
            let l = lib.lock().unwrap();
            for imp in &playlists {
                for id in &imp.track_ids {
                    if let Ok(Some(t)) = l.track(id) {
                        all.push(t);
                    }
                }
            }
        }
        Ok(Added { track_ids: tracks.iter().map(|t| t.id.clone()).collect(), tracks: all, playlists, failed })
    })
    .await
    .map_err(err)?
}

#[tauri::command]
fn export_playlist(st: State<AppState>, track_ids: Vec<String>, name: String, path: String, relative: bool) -> Res<()> {
    let lib = st.lib.lock().unwrap();
    let tracks: Vec<Track> = track_ids.iter().filter_map(|id| lib.track(id).ok().flatten()).collect();
    scan::export(&tracks, &name, Path::new(&path), relative).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Located {
    /// The track now at that path (another one, if the library had it already).
    track: Track,
    /// Other missing tracks whose files are in the same new folder.
    matches: Vec<(String, String)>,
}

#[tauri::command]
fn locate(st: State<AppState>, track_id: String, path: String) -> Res<Located> {
    let tools = need_tools(&st)?;
    let p = PathBuf::from(&path);
    let id = st.lib.lock().unwrap().move_track(&track_id, &p).map_err(err)?;
    scan::read_file(&st.lib, &tools, &p).map_err(err)?;
    let lib = st.lib.lock().unwrap();
    let track = lib.track(&id).map_err(err)?.ok_or("not found")?;
    let missing: Vec<Track> = lib.tracks().map_err(err)?.into_iter().filter(|t| !t.exists).collect();
    let dir = p.parent().unwrap_or(Path::new("."));
    let matches = scan::same_folder_matches(&missing, dir).into_iter().map(|(id, p)| (id, p.display().to_string())).collect();
    Ok(Located { track, matches })
}

/// Re-read files whose size or modified time changed; report every track.
#[tauri::command]
async fn refresh_tracks(app: AppHandle) -> Res<Vec<Track>> {
    let st = app.state::<AppState>();
    let lib = st.lib.clone();
    let tools = st.tools.clone();
    let root = st.music.root();
    tauri::async_runtime::spawn_blocking(move || {
        // The music folder's own scan looks after its files.
        let stale: Vec<PathBuf> = {
            let l = lib.lock().unwrap();
            l.tracks()
                .map_err(err)?
                .into_iter()
                .filter(|t| !root.as_deref().is_some_and(|r| fatstick_library::folder::inside(r, Path::new(&t.path))))
                .filter(|t| t.exists && !scan::is_current(&l, t))
                .map(|t| PathBuf::from(t.path))
                .collect()
        };
        if let Some(tools) = tools {
            scan::read_files(&lib, &tools, &stale, &|_| {});
        }
        let l = lib.lock().unwrap();
        l.tracks().map_err(err)
    })
    .await
    .map_err(err)?
}

// ---- background work -------------------------------------------------------------

#[tauri::command]
fn analyze(st: State<AppState>, track_ids: Vec<String>) {
    st.work.analyze(track_ids);
}

/// Read tracks in full (every MP3 frame) that the music folder's scan only
/// read quickly, once they're in a playlist.
#[tauri::command]
fn read_tracks(st: State<AppState>, track_ids: Vec<String>) {
    st.work.read(track_ids);
}

#[tauri::command]
fn prepare_queue(st: State<AppState>, jobs: Vec<work::PrepJob>) -> Vec<String> {
    st.work.set_queue(&st.lib, &st.cache, jobs)
}

#[tauri::command]
fn cache_info(st: State<AppState>) -> fatstick_audio::cache::CacheStats {
    st.cache.stats()
}

#[tauri::command]
fn clear_cache(st: State<AppState>) -> fatstick_audio::cache::CacheStats {
    st.cache.clear();
    st.cache.stats()
}

// ---- sticks and syncing -------------------------------------------------------------

#[derive(Serialize)]
struct SticksView {
    sticks: Vec<sticks::StickView>,
    error: Option<String>,
}

#[tauri::command]
async fn list_sticks() -> Res<SticksView> {
    tauri::async_runtime::spawn_blocking(|| {
        let (sticks, error) = sticks::list();
        SticksView { sticks, error }
    })
    .await
    .map_err(err)
}

#[tauri::command]
async fn sync(app: AppHandle, plan: sync::SyncPlan) -> Res<sync::SyncResult> {
    let st = app.state::<AppState>();
    if st.syncing.swap(true, Ordering::SeqCst) {
        return Err("Another sync is running.".into());
    }
    st.cancel.store(false, Ordering::SeqCst);
    st.work.paused.store(true, Ordering::SeqCst);
    let (lib, tools, cache, cancel) = (st.lib.clone(), st.tools.clone(), st.cache.clone(), st.cancel.clone());
    let app2 = app.clone();
    let r = tauri::async_runtime::spawn_blocking(move || {
        let progress = |p: sync::SyncProgress| {
            let _ = app2.emit("sync-progress", p);
        };
        let r = sync::run(&lib, tools.as_ref(), &cache, &plan, &cancel, &progress);
        let cap = cache_cap(&lib.lock().unwrap());
        cache.trim(cap, &[]);
        r
    })
    .await
    .map_err(err);
    st.work.paused.store(false, Ordering::SeqCst);
    st.syncing.store(false, Ordering::SeqCst);
    r
}

#[tauri::command]
fn cancel_sync(st: State<AppState>) {
    st.cancel.store(true, Ordering::SeqCst);
}

// ---- player ----------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayArgs {
    track_id: String,
    token: String,
    #[serde(default)]
    from: f64,
    /// "As on the stick": the gain to apply, and the recipe whose cached
    /// result plays instead of the source when it's ready.
    #[serde(default)]
    gain_db: f64,
    #[serde(default)]
    recipe: Option<Recipe>,
    /// The entry's key: semitones up or down, shifted live (unless the
    /// prepared file plays, which has it already).
    #[serde(default)]
    pitch: f64,
    #[serde(default = "yes")]
    keep_formants: bool,
    /// The entry's cuts, fades and added silences: it plays as the stick
    /// will have it (unless the prepared file plays, which has them).
    #[serde(flatten)]
    edges: Edges,
}

fn yes() -> bool {
    true
}

/// An entry's ends, in seconds: cut from the file, faded, and silence added.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase", default)]
struct Edges {
    cut_start: f64,
    cut_end: f64,
    fade_in: f64,
    fade_out: f64,
    pad_start: f64,
    pad_end: f64,
}

fn shape_of(duration: f64, e: &Edges) -> player::Shape {
    let ok = |x: f64| if x.is_finite() && x > 0.0 { x } else { 0.0 };
    let end = if ok(e.cut_end) > 0.0 && duration > 0.0 { (duration - ok(e.cut_end)).max(ok(e.cut_start)) } else { f64::INFINITY };
    // A fade out needs to know where the end is: the file's length.
    let end = if !end.is_finite() && ok(e.fade_out) > 0.0 && duration > 0.0 { duration } else { end };
    player::Shape { start: ok(e.cut_start), end, fade_in: ok(e.fade_in), fade_out: ok(e.fade_out), pad_start: ok(e.pad_start).min(3600.0), pad_end: ok(e.pad_end).min(3600.0) }
}

fn key_of(pitch: f64, keep_formants: bool) -> Option<fatstick_audio::stretch::Key> {
    (pitch != 0.0 && pitch.is_finite()).then(|| fatstick_audio::stretch::Key { semitones: pitch.clamp(-24.0, 24.0) as f32, keep_formants })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Playing {
    /// "cache" when the prepared file plays, "source" otherwise.
    from: String,
}

#[tauri::command]
fn player_play(st: State<AppState>, args: PlayArgs) -> Res<Playing> {
    let (path, dur) = {
        let lib = st.lib.lock().unwrap();
        let t = lib.track(&args.track_id).map_err(err)?.ok_or("That track isn't in the library.")?;
        if !t.exists {
            return Err(format!("{} isn't there any more.", t.path));
        }
        (PathBuf::from(&t.path), t.duration)
    };
    let cached = args.recipe.as_ref().and_then(|r| work::cached_for(&st.lib, &st.cache, &args.track_id, r));
    let (path, gain, from, dur, key, shape) = match cached {
        // A prepared file can be longer or shorter than its source (cut, padded).
        Some(p) => {
            let d = fatstick_audio::mp3::scan(&p).ok().flatten().map(|i| i.duration).unwrap_or(dur);
            (p, 0.0, "cache", d, None, player::Shape::default())
        }
        None => (path, args.gain_db, "source", dur, key_of(args.pitch, args.keep_formants), shape_of(dur, &args.edges)),
    };
    st.player.send(player::Cmd::Play { path, from: args.from, gain_db: gain, duration: dur, token: args.token, key, shape });
    Ok(Playing { from: from.into() })
}

/// The start and end, and fade sliders, live: other cuts, fades and silences
/// for what's playing. The window passes the track's length.
#[tauri::command]
fn player_shape(st: State<AppState>, duration: f64, edges: Edges) {
    st.player.send(player::Cmd::Shape(shape_of(duration, &edges)));
}

#[derive(Serialize)]
struct Peaks {
    /// Seconds per peak.
    step: f64,
    peaks: Vec<u8>,
}

/// A track's waveform for the start and end and fade dialogs: the peak of
/// every 50 ms. Async: decoding a long file takes a moment.
#[tauri::command]
async fn track_peaks(st: State<'_, AppState>, track_id: String) -> Res<Peaks> {
    let path = {
        let lib = st.lib.lock().unwrap();
        let t = lib.track(&track_id).map_err(err)?.ok_or("That track isn't in the library.")?;
        PathBuf::from(t.path)
    };
    let tools = st.tools.clone().ok_or("ffmpeg isn't available.")?;
    const STEP: f64 = 0.05;
    let peaks = tauri::async_runtime::spawn_blocking(move || fatstick_audio::decode::peaks(&tools, &path, STEP)).await.map_err(err)?.map_err(err)?;
    Ok(Peaks { step: STEP, peaks })
}

/// The key slider, live: another key for what's playing (0 for its own).
#[tauri::command]
fn player_key(st: State<AppState>, pitch: f64, keep_formants: bool) {
    st.player.send(player::Cmd::Key(key_of(pitch, keep_formants).or_else(|| (pitch == 0.0).then_some(fatstick_audio::stretch::Key { semitones: 0.0, keep_formants }))));
}

#[tauri::command]
fn player_pause(st: State<AppState>) {
    st.player.send(player::Cmd::Pause);
}
#[tauri::command]
fn player_resume(st: State<AppState>) {
    st.player.send(player::Cmd::Resume);
}
#[tauri::command]
fn player_seek(st: State<AppState>, pos: f64) {
    st.player.send(player::Cmd::Seek(pos));
}
#[tauri::command]
fn player_stop(st: State<AppState>) {
    st.player.send(player::Cmd::Stop);
}
#[tauri::command]
fn player_volume(st: State<AppState>, volume: f32) {
    st.player.send(player::Cmd::Volume(volume));
}

// ---- the music folder ----------------------------------------------------------------------

#[tauri::command]
fn library_info(st: State<AppState>) -> folder::FolderInfo {
    st.music.info()
}

/// Async: looking at a folder on a network drive can take a moment.
#[tauri::command]
async fn library_set_root(st: State<'_, AppState>, path: String) -> Res<folder::FolderInfo> {
    let music = st.music.clone();
    tauri::async_runtime::spawn_blocking(move || music.set_root(Path::new(&path))).await.map_err(err)?
}

#[derive(Serialize)]
struct Forgot {
    /// Taken out of the library: dropped, or kept for a playlist (in `kept`).
    removed: Vec<String>,
    kept: Vec<Track>,
}

/// Tracks whose files have gone, out of the library (the window says which
/// a playlist still refers to: those stay, unlisted).
#[tauri::command]
fn library_forget(st: State<AppState>, delete: Vec<String>, hide: Vec<String>) -> Res<Forgot> {
    let lib = st.lib.lock().unwrap();
    let removed = lib.forget(&delete, &hide).map_err(err)?;
    let kept = hide.iter().filter(|id| removed.contains(id)).filter_map(|id| lib.track(id).ok().flatten()).collect();
    Ok(Forgot { removed, kept })
}

#[tauri::command]
fn library_scan(st: State<AppState>) {
    st.music.scan();
}

/// Tag edits written into files in the music folder (each one read again after).
#[tauri::command]
async fn library_write_tags(st: State<'_, AppState>, jobs: Vec<folder::TagJob>) -> Res<folder::FileOps> {
    let music = st.music.clone();
    tauri::async_runtime::spawn_blocking(move || folder::write(&music, &jobs)).await.map_err(err)
}

/// Files in the music folder renamed within their folders.
#[tauri::command]
async fn library_rename(st: State<'_, AppState>, jobs: Vec<folder::RenameJob>) -> Res<folder::FileOps> {
    let music = st.music.clone();
    tauri::async_runtime::spawn_blocking(move || folder::rename(&music, &jobs)).await.map_err(err)
}

// ---- stick layout windows -------------------------------------------------------------------

/// Printing (a setlist): through the webview, as window.print() does nothing
/// in macOS's web view. The window calls window.print() on other systems.
#[tauri::command]
fn print_window(window: tauri::WebviewWindow) -> Res<()> {
    window.print().map_err(err)
}

// ---- about ---------------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct About {
    version: String,
    ffmpeg: Option<String>,
    ffmpeg_path: Option<String>,
    data_dir: String,
    cache: fatstick_audio::cache::CacheStats,
}

#[tauri::command]
fn about(st: State<AppState>) -> About {
    About {
        version: env!("CARGO_PKG_VERSION").into(),
        ffmpeg: st.tools.as_ref().map(|t| t.version()),
        ffmpeg_path: st.tools.as_ref().map(|t| t.ffmpeg.display().to_string()),
        data_dir: st.data_dir.display().to_string(),
        cache: st.cache.stats(),
    }
}

// ---- self-test -------------------------------------------------------------------------------
// With FATSTICK_SELFTEST=<script.js>, the window runs that script once the page has
// loaded (FATSTICK_SELFTEST_CONFIG is handed to it as JSON), and the script's report
// ends the app: tests/app_e2e.py drives the real app this way. Nothing happens
// without the variable.

#[tauri::command]
fn selftest_report(app: AppHandle, ok: bool, report: String) {
    if std::env::var_os("FATSTICK_SELFTEST").is_none() {
        return;
    }
    println!("{report}");
    println!("SELFTEST {}", if ok { "PASSED" } else { "FAILED" });
    app.exit(if ok { 0 } else { 1 });
}

fn selftest_script() -> Option<String> {
    let path = std::env::var_os("FATSTICK_SELFTEST")?;
    let body = std::fs::read_to_string(&path).ok()?;
    let config = std::env::var("FATSTICK_SELFTEST_CONFIG").unwrap_or_else(|_| "{}".into());
    Some(format!("window.__FATSTICK_SELFTEST_CONFIG = {config};\n{body}"))
}

fn main() {
    #[cfg(target_os = "linux")]
    graphics::apply();
    let data_dir = std::env::var_os("FATSTICK_DATA_DIR").map(PathBuf::from).unwrap_or_else(fatstick_library::default_dir);
    let cache_dir = std::env::var_os("FATSTICK_CACHE_DIR").map(PathBuf::from).unwrap_or_else(Cache::default_dir);
    let lib = Library::open(&data_dir.join("library.db")).expect("opening the library database");
    let cache = Cache::open(&cache_dir).expect("opening the cache folder");
    let (tools, tools_error) = match Tools::locate() {
        Ok(t) => (Some(t), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let lib = Arc::new(Mutex::new(lib));

    let selftest = selftest_script();
    let ran = AtomicBool::new(false);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_page_load(move |webview, payload| {
            if let (Some(js), tauri::webview::PageLoadEvent::Finished) = (&selftest, payload.event()) {
                if !ran.swap(true, Ordering::SeqCst) {
                    let _ = webview.eval(js);
                }
            }
        })
        .setup(move |app| {
            let h = app.handle().clone();
            let work = work::Work::start(lib.clone(), tools.clone(), cache.clone(), move |e| {
                let _ = match e {
                    work::Event::Prepared(p) => h.emit("prepared", p),
                    work::Event::Analyzed(a) => h.emit("analyzed", a),
                    work::Event::Read(r) => h.emit("track-read", r),
                };
            });
            let h = app.handle().clone();
            let player = player::Player::start(tools.clone(), move |ev| {
                let _ = h.emit("player", ev);
            });
            let h = app.handle().clone();
            let music = folder::MusicFolder::start(lib.clone(), tools.clone(), move |name, v| {
                let _ = h.emit(name, v);
            });
            app.manage(AppState {
                lib: lib.clone(),
                tools: tools.clone(),
                tools_error: tools_error.clone(),
                cache: cache.clone(),
                work,
                player,
                cancel: Arc::new(AtomicBool::new(false)),
                syncing: AtomicBool::new(false),
                data_dir: data_dir.clone(),
                music,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            init,
            library_info,
            library_set_root,
            library_scan,
            library_forget,
            library_write_tags,
            library_rename,
            read_tracks,
            save_docs,
            set_setting,
            add_paths,
            export_playlist,
            locate,
            refresh_tracks,
            analyze,
            prepare_queue,
            cache_info,
            clear_cache,
            list_sticks,
            sync,
            cancel_sync,
            player_play,
            player_pause,
            player_key,
            player_shape,
            track_peaks,
            print_window,
            player_resume,
            player_seek,
            player_stop,
            player_volume,
            about,
            selftest_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Fatstick");
}
