// Playlist2USB desktop app. The window (../ui) owns playlists, profiles and
// sticks and plans each sync; this side reads files, keeps the library,
// measures, converts and caches audio, plays previews, and writes sticks.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

use p2u_audio::prepare::Recipe;
use p2u_audio::{Cache, Tools};
use p2u_library::{scan, Library, Track};
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
    cache: p2u_audio::cache::CacheStats,
    data_dir: String,
    version: String,
    /// This library's id, which every stick it writes carries.
    library_id: String,
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
    tauri::async_runtime::spawn_blocking(move || {
        let stale: Vec<PathBuf> = {
            let l = lib.lock().unwrap();
            l.tracks().map_err(err)?.into_iter().filter(|t| t.exists && !scan::is_current(&l, t)).map(|t| PathBuf::from(t.path)).collect()
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

#[tauri::command]
fn prepare_queue(st: State<AppState>, jobs: Vec<work::PrepJob>) -> Vec<String> {
    st.work.set_queue(&st.lib, &st.cache, jobs)
}

#[tauri::command]
fn cache_info(st: State<AppState>) -> p2u_audio::cache::CacheStats {
    st.cache.stats()
}

#[tauri::command]
fn clear_cache(st: State<AppState>) -> p2u_audio::cache::CacheStats {
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
    let (path, gain, from) = match cached {
        Some(p) => (p, 0.0, "cache"),
        None => (path, args.gain_db, "source"),
    };
    st.player.send(player::Cmd::Play { path, from: args.from, gain_db: gain, duration: dur, token: args.token });
    Ok(Playing { from: from.into() })
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

// ---- about ---------------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct About {
    version: String,
    ffmpeg: Option<String>,
    ffmpeg_path: Option<String>,
    data_dir: String,
    cache: p2u_audio::cache::CacheStats,
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
// With P2U_SELFTEST=<script.js>, the window runs that script once the page has
// loaded (P2U_SELFTEST_CONFIG is handed to it as JSON), and the script's report
// ends the app: tests/app_e2e.py drives the real app this way. Nothing happens
// without the variable.

#[tauri::command]
fn selftest_report(app: AppHandle, ok: bool, report: String) {
    if std::env::var_os("P2U_SELFTEST").is_none() {
        return;
    }
    println!("{report}");
    println!("SELFTEST {}", if ok { "PASSED" } else { "FAILED" });
    app.exit(if ok { 0 } else { 1 });
}

fn selftest_script() -> Option<String> {
    let path = std::env::var_os("P2U_SELFTEST")?;
    let body = std::fs::read_to_string(&path).ok()?;
    let config = std::env::var("P2U_SELFTEST_CONFIG").unwrap_or_else(|_| "{}".into());
    Some(format!("window.__P2U_SELFTEST_CONFIG = {config};\n{body}"))
}

fn main() {
    #[cfg(target_os = "linux")]
    graphics::apply();
    let data_dir = std::env::var_os("P2U_DATA_DIR").map(PathBuf::from).unwrap_or_else(p2u_library::default_dir);
    let cache_dir = std::env::var_os("P2U_CACHE_DIR").map(PathBuf::from).unwrap_or_else(Cache::default_dir);
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
                };
            });
            let h = app.handle().clone();
            let player = player::Player::start(tools.clone(), move |ev| {
                let _ = h.emit("player", ev);
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
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            init,
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
            player_resume,
            player_seek,
            player_stop,
            player_volume,
            about,
            selftest_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Playlist2USB");
}
