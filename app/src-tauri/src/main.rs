// Playlist2USB desktop shell. The UI is plain HTML in ../ui; every operation
// here is a thin wrapper over p2u-core (playlists, the FAT32 stick writer) and
// p2u-device (finding sticks, OS-brokered raw access).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use p2u_core::fat::stick::{plan_stick, StickOptions, WriteItem};
use p2u_core::job::{run_stick, StickJob, StickOutcome};
use p2u_core::plan::{make_plan, Plan, PlanOptions};
use p2u_core::playlist::{self, Playlist, PlaylistEntry, Source};
use p2u_device::{DeviceError, StickInfo};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

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
fn list_sticks() -> Result<Vec<StickInfo>, String> {
    p2u_device::list_sticks().map_err(|e| e.to_string())
}

fn find_stick(id: &str) -> Result<StickInfo, String> {
    p2u_device::list_sticks()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "The stick was removed or changed. Plug it in and pick it again.".to_string())
}

#[derive(Deserialize)]
struct TrackIn {
    path: String,
    title: Option<String>,
}

#[derive(Deserialize)]
struct WriteRequest {
    name: String,
    tracks: Vec<TrackIn>,
    stick_id: String,
    /// Empty string = top level of the stick.
    folder: String,
    number_prefix: bool,
    ascii_names: bool,
    /// Compare every byte on the stick with the source afterwards.
    #[serde(default = "yes")]
    verify_data: bool,
}

fn yes() -> bool {
    true
}

fn build_plan(req: &WriteRequest) -> Plan {
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
        &PlanOptions { folder_name: Some(req.folder.clone()), number_prefix: req.number_prefix, ascii_names: req.ascii_names },
    )
}

#[derive(Serialize)]
struct WriteResponse {
    outcome: StickOutcome,
    plan: Plan,
    method: &'static str,
    /// Set when the stick was ejected (powered off) after the check.
    ejected: bool,
}

/// The one action: erase the stick, write a fresh FAT32 file system with the
/// playlist in FAT order and in one unbroken block per file, read it all back.
#[tauri::command]
async fn write_playlist(app: AppHandle, req: WriteRequest) -> Result<WriteResponse, String> {
    let stick = find_stick(&req.stick_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let plan = build_plan(&req);
        let job = StickJob {
            items: plan.items.iter().map(|i| WriteItem { source: i.source.clone(), name: i.target_name.clone() }).collect(),
            options: StickOptions { folder: plan.folder_name.clone(), label: String::new() },
            verify_data: req.verify_data,
            eject: true,
        };
        // Catch "doesn't fit" and bad names before the stick is touched.
        plan_stick(stick.size_bytes, 512, &job.items, &job.options).map_err(|e| format!("Nothing was written: {e}."))?;
        match p2u_device::acquire(&stick) {
            Ok(mut raw) => {
                let mut outcome = run_stick(&mut raw, &job, &mut |p| {
                    let _ = app.emit("write-progress", p);
                });
                let mut ejected = false;
                if outcome.ok {
                    match raw.finish() {
                        Ok(()) => ejected = true,
                        Err(e) => outcome.error = Some(e.to_string()),
                    }
                }
                Ok(WriteResponse { outcome, plan, method: "system", ejected })
            }
            Err(DeviceError::NeedsElevation) | Err(DeviceError::NoBroker(_)) => {
                let outcome = p2u_device::write_via_helper(&stick, &job).map_err(|e| e.to_string())?;
                let ejected = outcome.ok && outcome.error.is_none();
                Ok(WriteResponse { outcome, plan, method: "helper", ejected })
            }
            Err(DeviceError::NotAuthorized) => Err("Authorization was cancelled. Nothing was written.".into()),
            Err(e) => Err(format!("Nothing was written: {e}")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![load_playlist, startup_playlist, list_sticks, write_playlist])
        .run(tauri::generate_context!())
        .expect("error while running Playlist2USB");
}
