// Playlist2USB desktop shell. The UI is plain HTML in ../ui; every operation
// here is a thin wrapper over p2u-core (playlists, copy, FAT reorder) and
// p2u-device (volumes, OS-brokered raw access).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use p2u_core::fat::write::{WriteItem, WriteOptions};
use p2u_core::job::{run_lock, run_write, LockJob, LockOutcome, WriteJob, WriteOutcome};
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
    /// Empty string = top level of the stick.
    folder: String,
    number_prefix: bool,
    ascii_names: bool,
    #[serde(default)]
    replace: bool,
    /// Allow files to be split when no single free block is large enough.
    #[serde(default)]
    allow_fragmented: bool,
    /// Compare every byte on the stick with the source afterwards.
    #[serde(default = "yes")]
    verify_data: bool,
}

fn yes() -> bool {
    true
}

fn build_plan(req: &CopyRequest, mount: &std::path::Path) -> Plan {
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
    make_plan(
        &pl,
        mount,
        &PlanOptions { folder_name: Some(req.folder.clone()), number_prefix: req.number_prefix, ascii_names: req.ascii_names },
    )
}

/// The names and sizes the stick should hold, without copying ("Lock order only").
#[tauri::command]
fn plan_playlist(req: CopyRequest) -> Result<Plan, String> {
    let vol = find_volume(&req.volume_id)?;
    let mount = vol.mount_point.clone().unwrap_or_default();
    Ok(build_plan(&req, &mount))
}

#[derive(Serialize)]
struct WriteResponse {
    outcome: WriteOutcome,
    plan: Plan,
    method: &'static str,
}

/// The main action: write the files straight into the FAT volume (order and
/// placement chosen here), then read everything back from the stick.
#[tauri::command]
async fn write_playlist(app: AppHandle, req: CopyRequest) -> Result<WriteResponse, String> {
    let vol = find_volume(&req.volume_id)?;
    let backups = app.path().app_data_dir().map_err(|e| e.to_string())?.join("backups");
    std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let plan = build_plan(&req, std::path::Path::new("/"));
        let job = WriteJob {
            target_dir: plan.volume_path.clone(),
            items: plan.items.iter().map(|i| WriteItem { source: i.source.clone(), name: i.target_name.clone() }).collect(),
            options: WriteOptions { replace: req.replace, allow_fragmented: req.allow_fragmented, force: false, dry_run: false },
            verify_data: req.verify_data,
            partition: None,
            eject: true,
        };
        match p2u_device::acquire(&vol) {
            Ok(mut raw) => {
                let outcome = run_write(&mut raw, &job, Some(&backups), &mut |p| {
                    let _ = app.emit("write-progress", p);
                });
                let _ = if outcome.ok { raw.eject() } else { Ok(()) };
                drop(raw);
                if !outcome.ok {
                    let _ = p2u_device::remount_after(&vol);
                }
                Ok(WriteResponse { outcome, plan, method: "system" })
            }
            Err(DeviceError::NeedsElevation) | Err(DeviceError::NoBroker(_)) => {
                let outcome = p2u_device::write_via_helper(&vol, &job).map_err(|e| e.to_string())?;
                Ok(WriteResponse { outcome, plan, method: "helper" })
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

#[derive(Deserialize)]
struct LockRequest {
    volume_id: String,
    volume_path: String,
    order: Vec<String>,
    /// Expected sizes, parallel to `order`.
    sizes: Option<Vec<u64>>,
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
        let job = LockJob {
            dir_path: req.volume_path,
            order: req.order,
            expected_sizes: req.sizes,
            options: Default::default(),
            partition: None,
            eject: true,
        };
        match p2u_device::acquire(&vol) {
            Ok(mut raw) => {
                let outcome = run_lock(&mut raw, &job, Some(&backups));
                // On success leave the stick unmounted so nothing writes to it
                // after the verification; on failure mount it again.
                let _ = if outcome.ok { raw.eject() } else { Ok(()) };
                drop(raw);
                let mp = if outcome.ok { None } else { p2u_device::remount_after(&vol).ok().flatten() };
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            load_playlist,
            startup_playlist,
            list_volumes,
            plan_playlist,
            write_playlist,
            lock_order
        ])
        .run(tauri::generate_context!())
        .expect("error while running Playlist2USB");
}
