// Playlist2USB desktop shell. The UI is plain HTML in ../ui; every operation
// here is a thin wrapper over p2u-core (playlists, copy, FAT reorder) and
// p2u-device (volumes, OS-brokered raw access).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use p2u_core::copy::{execute_copy, readdir_order, CopyOptions, CopyReport};
use p2u_core::fat::{check_order, OrderCheck};
use p2u_core::job::{run_lock, LockJob, LockOutcome};
use p2u_core::plan::{make_plan, Plan, PlanOptions};
use p2u_core::playlist::{self, Playlist, PlaylistEntry, Source};
use p2u_device::{DeviceError, VolumeInfo};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Serialize)]
struct TrackView {
    path: String,
    title: String,
    exists: bool,
    size: u64,
    duration_secs: Option<f64>,
}

#[derive(Serialize)]
struct PlaylistView {
    name: String,
    path: String,
    tracks: Vec<TrackView>,
    skipped: Vec<String>,
    warnings: Vec<String>,
}

#[tauri::command]
fn load_playlist(path: String) -> Result<PlaylistView, String> {
    let pl = playlist::load(PathBuf::from(&path).as_path()).map_err(|e| e.to_string())?;
    let mut tracks = Vec::new();
    let mut skipped = Vec::new();
    for e in &pl.entries {
        match &e.source {
            Source::File(p) => {
                let meta = std::fs::metadata(p).ok().filter(|m| m.is_file());
                let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                tracks.push(TrackView {
                    path: p.display().to_string(),
                    title: e.title.clone().unwrap_or(stem),
                    exists: meta.is_some(),
                    size: meta.map(|m| m.len()).unwrap_or(0),
                    duration_secs: e.duration_secs,
                });
            }
            Source::Url(u) => skipped.push(u.clone()),
        }
    }
    Ok(PlaylistView { name: pl.name, path, tracks, skipped, warnings: pl.warnings })
}

/// A playlist passed on the command line ("Open with", file associations).
#[tauri::command]
fn startup_playlist() -> Option<String> {
    std::env::args().skip(1).find(|a| !a.starts_with('-') && std::path::Path::new(a).is_file())
}

#[tauri::command]
fn list_volumes() -> Result<Vec<VolumeInfo>, String> {
    p2u_device::list_volumes().map_err(|e| e.to_string())
}

fn find_volume(id: &str) -> Result<VolumeInfo, String> {
    p2u_device::list_volumes()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|v| v.id == id)
        .ok_or_else(|| "The stick was removed or changed. Plug it in and pick it again.".to_string())
}

#[derive(Deserialize)]
struct TrackIn {
    path: String,
    title: Option<String>,
}

#[derive(Deserialize)]
struct CopyRequest {
    name: String,
    tracks: Vec<TrackIn>,
    volume_id: String,
    folder: String,
    number_prefix: bool,
    ascii_names: bool,
    replace: bool,
}

#[derive(Serialize)]
struct CopyResult {
    report: CopyReport,
    plan: Plan,
}

#[tauri::command]
async fn copy_playlist(app: AppHandle, req: CopyRequest) -> Result<CopyResult, String> {
    let vol = find_volume(&req.volume_id)?;
    let mount = vol.mount_point.clone().ok_or("The stick is not mounted. Unplug it and plug it in again.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let pl = Playlist {
            name: req.name.clone(),
            path: PathBuf::new(),
            entries: req
                .tracks
                .iter()
                .map(|t| PlaylistEntry { source: Source::File(PathBuf::from(&t.path)), title: t.title.clone(), duration_secs: None })
                .collect(),
            warnings: vec![],
        };
        let plan = make_plan(
            &pl,
            &mount,
            &PlanOptions { folder_name: Some(req.folder.clone()), number_prefix: req.number_prefix, ascii_names: req.ascii_names },
        );
        let opts = CopyOptions { replace_existing: req.replace, stamp_times: true, write_m3u: false, sync_each_file: false };
        let report = execute_copy(&plan, &opts, None, |p| {
            let _ = app.emit("copy-progress", p);
            true
        })
        .map_err(|e| e.to_string())?;
        Ok(CopyResult { report, plan })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Deserialize)]
struct LockRequest {
    volume_id: String,
    volume_path: String,
    order: Vec<String>,
}

#[derive(Serialize)]
struct LockResult {
    outcome: LockOutcome,
    mount_point: Option<String>,
    method: &'static str,
}

#[tauri::command]
async fn lock_order(app: AppHandle, req: LockRequest) -> Result<LockResult, String> {
    let vol = find_volume(&req.volume_id)?;
    let backups = app.path().app_data_dir().map_err(|e| e.to_string())?.join("backups");
    std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let job = LockJob { dir_path: req.volume_path, order: req.order, options: Default::default(), partition: None };
        match p2u_device::acquire(&vol) {
            Ok(raw) => {
                let outcome = run_lock(raw, &job, Some(&backups));
                let mp = p2u_device::remount_after(&vol).ok().flatten();
                Ok(LockResult { outcome, mount_point: mp.map(|m| m.display().to_string()), method: "system" })
            }
            Err(DeviceError::NeedsElevation) | Err(DeviceError::NoBroker(_)) => {
                let outcome = p2u_device::lock_via_helper(&vol, &job).map_err(|e| e.to_string())?;
                Ok(LockResult { outcome, mount_point: None, method: "helper" })
            }
            Err(DeviceError::NotAuthorized) => {
                let _ = p2u_device::remount_after(&vol);
                Err("Authorization was cancelled. The stick was not changed.".into())
            }
            Err(e) => {
                let _ = p2u_device::remount_after(&vol);
                Err(e.to_string())
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Unprivileged check: FAT drivers list a folder in on-disk order.
#[tauri::command]
fn check_mounted_order(volume_id: String, volume_path: String, order: Vec<String>) -> Result<OrderCheck, String> {
    let vol = find_volume(&volume_id)?;
    let mount = vol.mount_point.ok_or("The stick is not mounted.")?;
    let dir = mount.join(volume_path.trim_start_matches(['/', '\\']));
    Ok(check_order(&readdir_order(&dir).map_err(|e| e.to_string())?, &order))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            load_playlist,
            startup_playlist,
            list_volumes,
            copy_playlist,
            lock_order,
            check_mounted_order
        ])
        .run(tauri::generate_context!())
        .expect("error while running Playlist2USB");
}
