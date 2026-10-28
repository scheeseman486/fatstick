//! A sync, end to end. The window sends what each stick should hold (folders,
//! names, and what to do with each track); this side:
//!
//! 1. prepares every file in the cache (or finds it prepared), re-reading any
//!    source that changed, and never copying an MP3 the profile can't take;
//! 2. builds each file's tags and cover;
//! 3. lays out the whole stick exactly, so "fits" means fits;
//! 4. gets the stick from the OS (one prompt), erases it, writes it in one
//!    pass and reads every byte back;
//! 5. ejects it and records what was written.
//!
//! Steps 1 to 3 can't change the stick: a missing file, a decode error or a
//! stick that's too small stops the sync with the stick as it was. Cancelling
//! during the write leaves a blank stick.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use p2u_audio::encode::{output_rate, EncodeSpec};
use p2u_audio::prepare::{copy_allowed, prepare, Prepared, Recipe};
use p2u_audio::render::{self, RenderSpec};
use p2u_audio::tags::{id3v1, id3v2, TagSpec};
use p2u_audio::{cover, Cache, Probe, Tags, Tools};
use p2u_core::fat::stick::{plan_content, Part, StickContent, StickFolder, WriteItem, WriteProgress};
use p2u_core::fat::Device;
use p2u_core::job::{run_stick, StickJob, StickOutcome};
use p2u_library::Library;
use serde::{Deserialize, Serialize};

use crate::sticks::{self, Target};
use crate::work::{current_probe, render_sources};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePlan {
    pub bitrate_cap: u32,
    #[serde(default)]
    pub mode: String,
    pub sample_rates: Vec<u32>,
    #[serde(default)]
    pub tags: String,
    #[serde(default)]
    pub id3v1: bool,
    #[serde(default)]
    pub cover: String,
    #[serde(default = "yes")]
    pub track_tag_is_position: bool,
    #[serde(default)]
    pub number_in_title: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePlan {
    pub name: String,
    /// The track whose tags the file gets; empty for a joined playlist or
    /// silence, which get `title`, `artist` and `album` instead.
    #[serde(default)]
    pub track_id: String,
    pub recipe: Recipe,
    pub position: u32,
    pub total: u32,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPlan {
    /// Empty: tracks at the top level.
    pub name: String,
    pub files: Vec<FilePlan>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    pub set_id: String,
    pub set_name: String,
    pub stick_id: String,
    /// The size the window saw when the sync was confirmed. A different size
    /// at the same device means another stick was plugged in since.
    #[serde(default)]
    pub stick_size: Option<u64>,
    #[serde(default)]
    pub label: String,
    pub profile: ProfilePlan,
    pub folders: Vec<FolderPlan>,
    #[serde(default = "yes")]
    pub verify: bool,
    /// The window's record of what this sync holds, stored as is.
    #[serde(default)]
    pub record: serde_json::Value,
    /// What the window says about the stick for its note on the stick:
    /// `{ profile: {id, name}, playlists: [{id, name, tracks}] }`.
    #[serde(default)]
    pub note: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    /// prepare, tags, check, authorize, erase, write, verify, eject
    pub stage: String,
    pub done: u64,
    pub total: u64,
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub ok: bool,
    pub cancelled: bool,
    /// The stick's old contents are gone (it was erased, even if the write failed).
    pub erased: bool,
    pub error: Option<String>,
    pub notes: Vec<String>,
    pub files: usize,
    pub bytes: u64,
    pub ejected: bool,
    pub record: Option<serde_json::Value>,
    /// What the read-back found, for the details.
    pub outcome: Option<StickOutcome>,
}

impl SyncResult {
    fn fail(e: impl Into<String>) -> Self {
        SyncResult { error: Some(e.into()), ..Default::default() }
    }
}

/// What the stick will actually get: an MP3 the profile can't take is
/// encoded even if the window asked for a copy (and the result says so).
fn effective(probe: &Probe, recipe: &Recipe, p: &ProfilePlan) -> (Recipe, bool) {
    let rates = if p.sample_rates.is_empty() { vec![44100, 48000] } else { p.sample_rates.clone() };
    let enc = |gain_db: f64| {
        Recipe::Encode(EncodeSpec { cap: p.bitrate_cap, vbr: p.mode == "vbr", sample_rate: output_rate(probe.sample_rate, &rates), gain_db })
    };
    match recipe {
        Recipe::Copy if !copy_allowed(probe, p.bitrate_cap, &rates) => (enc(0.0), true),
        &Recipe::Gain { steps } if !copy_allowed(probe, p.bitrate_cap, &rates) => (enc(steps as f64 * 1.5), true),
        Recipe::Encode(s) => {
            let mut s = *s;
            s.cap = s.cap.min(p.bitrate_cap);
            if !rates.contains(&s.sample_rate) {
                s.sample_rate = output_rate(probe.sample_rate, &rates);
            }
            (Recipe::Encode(s), false)
        }
        r => (r.clone(), false),
    }
}

/// A render within the profile's limits.
fn effective_render(spec: &RenderSpec, p: &ProfilePlan) -> RenderSpec {
    let rates = if p.sample_rates.is_empty() { vec![44100, 48000] } else { p.sample_rates.clone() };
    let mut s = spec.clone();
    s.cap = s.cap.min(p.bitrate_cap);
    if !rates.contains(&s.sample_rate) {
        s.sample_rate = output_rate(44100, &rates);
    }
    s
}

/// The front cover for the stick, made once per source content and kept.
fn cover_for(tools: &Tools, cache: &Cache, src: &Path, content_key: &str) -> Option<Vec<u8>> {
    let dir = cache.dir().join("covers");
    let _ = std::fs::create_dir_all(&dir);
    let have = dir.join(format!("{content_key}.jpg"));
    let none = dir.join(format!("{content_key}.none"));
    if let Ok(b) = std::fs::read(&have) {
        return Some(b);
    }
    if none.exists() {
        return None;
    }
    let made = cover::extract(tools, src, &cache.temp("cover")).ok().flatten().and_then(|pic| cover::for_stick(&pic));
    match &made {
        Some(b) => {
            let _ = std::fs::write(&have, b);
        }
        None => {
            let _ = std::fs::write(&none, b"");
        }
    }
    made
}

struct Ready {
    /// The file whose tags (and cover) it gets; none for silence.
    probe: Option<Probe>,
    content_key: String,
    path: PathBuf,
    audio: Prepared,
    switched: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    lib: &Mutex<Library>,
    tools: Option<&Tools>,
    cache: &Cache,
    plan: &SyncPlan,
    cancel: &AtomicBool,
    progress: &(dyn Fn(SyncProgress) + Sync),
) -> SyncResult {
    let Some(tools) = tools else { return SyncResult::fail("ffmpeg isn't available, so nothing can be prepared. Nothing was written.") };
    let target = match sticks::find(&plan.stick_id) {
        Ok(t) => t,
        Err(e) => return SyncResult::fail(e),
    };
    let size_now = match &target {
        Target::Image(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        Target::Stick(s) => s.size_bytes,
    };
    if plan.stick_size.is_some_and(|want| want != size_now) {
        return SyncResult::fail("A different stick is plugged in now. Pick it again to sync to it. Nothing was written.");
    }
    let files: Vec<&FilePlan> = plan.folders.iter().flat_map(|f| f.files.iter()).collect();
    if files.is_empty() {
        return SyncResult::fail("There's nothing to write. Nothing was written.");
    }
    let say = |stage: &str, done: u64, total: u64, name: &str, message: String| {
        progress(SyncProgress { stage: stage.into(), done, total, name: name.into(), message })
    };

    // 1. Prepare, on all cores but one.
    let n = files.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<Ready, String>>>> = Mutex::new((0..n).map(|_| None).collect());
    let workers = std::thread::available_parallelism().map(|x| x.get().saturating_sub(1).clamp(1, 6)).unwrap_or(2);
    say("prepare", 0, n as u64, "", format!("Preparing {n} files."));
    std::thread::scope(|s| {
        for _ in 0..workers.min(n) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let f = files[i];
                let r = match &f.recipe {
                    Recipe::Render(spec) => (|| {
                        let spec = effective_render(spec, &plan.profile);
                        let srcs = render_sources(lib, Some(tools), &spec)?;
                        let audio = render::prepare(tools, cache, &spec, &srcs).map_err(|e| e.to_string())?;
                        // Tags from the file's own track, else the first part's (cover art).
                        let tagged = if f.track_id.is_empty() { spec.parts.iter().find_map(|p| p.track_id.clone()) } else { Some(f.track_id.clone()) };
                        let (path, content_key, probe) = match tagged {
                            Some(id) => {
                                let (path, key, probe) = current_probe(lib, tools, &id)?;
                                (path, key, Some(probe))
                            }
                            None => (PathBuf::new(), String::new(), None),
                        };
                        Ok(Ready { probe, content_key, path, audio, switched: false })
                    })(),
                    _ => current_probe(lib, tools, &f.track_id).and_then(|(path, key, probe)| {
                        if probe.drm || probe.codec.is_empty() {
                            return Err(format!("{} can't be decoded", path.display()));
                        }
                        let (recipe, switched) = effective(&probe, &f.recipe, &plan.profile);
                        let audio = prepare(tools, cache, &path, &probe, &key, &recipe).map_err(|e| format!("{}: {e}", path.display()))?;
                        Ok(Ready { probe: Some(probe), content_key: key, path, audio, switched })
                    }),
                };
                results.lock().unwrap()[i] = Some(r);
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                say("prepare", d as u64, n as u64, &f.name, format!("Prepared {d} of {n}: {}", f.name));
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return SyncResult { cancelled: true, error: Some("Cancelled. Nothing was written; the stick is as it was.".into()), ..Default::default() };
    }
    let mut ready = Vec::with_capacity(n);
    for (r, f) in results.into_inner().unwrap().into_iter().zip(&files) {
        match r {
            Some(Ok(x)) => ready.push(x),
            Some(Err(e)) => return SyncResult::fail(format!("{} couldn't be prepared: {e}. Nothing was written.", f.name)),
            None => return SyncResult::fail("Preparing stopped early. Nothing was written."),
        }
    }
    let mut notes = Vec::new();
    let switched: Vec<&str> = files.iter().zip(&ready).filter(|(_, r)| r.switched).map(|(f, _)| f.name.as_str()).collect();
    if !switched.is_empty() {
        notes.push(format!("{} encoded rather than copied, because the profile can't play them as they are.", switched.len()));
    }

    // 2. Tags (and covers), all in one scratch file the parts point into.
    say("tags", 0, n as u64, "", "Writing tags.".into());
    let scratch = cache.temp("sync");
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        return SyncResult::fail(format!("Couldn't make a scratch folder: {e}. Nothing was written."));
    }
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&scratch);
    };
    let tag_path = scratch.join("tags.bin");
    let mut tag_file = match std::fs::File::create(&tag_path) {
        Ok(f) => f,
        Err(e) => {
            cleanup();
            return SyncResult::fail(format!("Couldn't write tags: {e}. Nothing was written."));
        }
    };
    let mut offset = 0u64;
    let mut content = StickContent { folders: Vec::new(), label: plan.label.clone(), hidden: Vec::new() };
    let mut k = 0;
    let p = &plan.profile;
    for folder in &plan.folders {
        let mut items = Vec::with_capacity(folder.files.len());
        for f in &folder.files {
            let r = &ready[k];
            k += 1;
            // A joined playlist or silence has tags of its own; a track has its source's.
            let own = f.title.is_some();
            let blank = Tags::default();
            let t = if own { &blank } else { r.probe.as_ref().map(|x| &x.tags).unwrap_or(&blank) };
            let base = f.title.clone().unwrap_or_else(|| t.title.clone());
            let title = if p.number_in_title && !base.is_empty() {
                format!("{:0w$} {}", f.position, base, w = f.total.to_string().len().max(2))
            } else {
                base
            };
            let has_cover = r.probe.as_ref().is_some_and(|x| x.cover);
            let spec = TagSpec {
                title,
                artist: f.artist.clone().unwrap_or_else(|| t.artist.clone()),
                album: f.album.clone().unwrap_or_else(|| t.album.clone()),
                albumartist: t.albumartist.clone(),
                year: t.year,
                track: if p.track_tag_is_position || own { Some(f.position) } else { t.track },
                track_total: if p.track_tag_is_position || own { Some(f.total) } else { None },
                disc: if p.track_tag_is_position { None } else { t.disc },
                genre: t.genre.clone(),
                cover_jpeg: if p.cover == "none" || !has_cover { None } else { cover_for(tools, cache, &r.path, &r.content_key) },
            };
            let mut parts = Vec::with_capacity(3);
            let v2 = id3v2(&spec, p.tags == "id3v24");
            if !v2.is_empty() {
                if tag_file.write_all(&v2).is_err() {
                    cleanup();
                    return SyncResult::fail("Couldn't write tags. Nothing was written.");
                }
                parts.push(Part::range(&tag_path, offset, v2.len() as u64));
                offset += v2.len() as u64;
            }
            parts.push(Part::range(&r.audio.path, r.audio.offset, r.audio.len));
            if p.id3v1 {
                let v1 = id3v1(&spec);
                if tag_file.write_all(&v1).is_err() {
                    cleanup();
                    return SyncResult::fail("Couldn't write tags. Nothing was written.");
                }
                parts.push(Part::range(&tag_path, offset, 128));
                offset += 128;
            }
            items.push(WriteItem { name: f.name.clone(), parts });
            say("tags", k as u64, n as u64, &f.name, String::new());
        }
        content.folders.push(StickFolder { name: folder.name.clone(), items });
    }
    if tag_file.flush().and_then(|_| tag_file.sync_all()).is_err() {
        cleanup();
        return SyncResult::fail("Couldn't write tags. Nothing was written.");
    }
    drop(tag_file);
    if cancel.load(Ordering::Relaxed) {
        cleanup();
        return SyncResult { cancelled: true, error: Some("Cancelled. Nothing was written; the stick is as it was.".into()), ..Default::default() };
    }

    // The note on the stick: which stick (set) of which library it holds, so
    // it is recognized when it's plugged in again.
    let library_id = lib.lock().unwrap().library_id().unwrap_or_default();
    let note = serde_json::json!({
        "app": "Playlist2USB",
        "format": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "libraryId": library_id,
        "setId": plan.set_id,
        "setName": plan.set_name,
        "profile": plan.note.get("profile").cloned().unwrap_or_default(),
        "playlists": plan.note.get("playlists").cloned().unwrap_or_default(),
        "files": n,
        "syncedAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
    });
    let note_path = scratch.join("note.json");
    if std::fs::write(&note_path, serde_json::to_vec_pretty(&note).unwrap_or_default()).is_err() {
        cleanup();
        return SyncResult::fail("Couldn't write the stick's note. Nothing was written.");
    }
    content.hidden.push(WriteItem::file(&note_path, sticks::NOTE_NAME));

    // 3. The exact layout, before the stick is touched.
    let size = match &target {
        Target::Image(path) => std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        Target::Stick(s) => s.size_bytes,
    };
    let exact = match plan_content(size, 512, &content) {
        Ok(x) => x,
        Err(e) => {
            cleanup();
            return SyncResult { notes, ..SyncResult::fail(format!("Nothing was written: {e}.")) };
        }
    };
    say("check", 1, 1, "", format!("{} files, {:.1} MB, fit on the stick.", exact.file_count(), exact.total_bytes as f64 / 1e6));

    // 4. Erase, write, read back.
    let job = StickJob { content, verify_data: plan.verify, eject: true };
    let mut prog = |w: &WriteProgress| {
        let message = match w.stage.as_str() {
            "erase" => "Erasing the stick.".to_string(),
            "write" => format!("Writing {} of {}: {}", w.index + 1, w.total, w.name),
            _ => format!("Reading back {} of {}: {}", w.index + 1, w.total, w.name),
        };
        progress(SyncProgress { stage: w.stage.clone(), done: w.all_done, total: w.all_total, name: w.name.clone(), message });
    };
    let mut ejected = false;
    let outcome = match &target {
        Target::Image(path) => match std::fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(mut f) => {
                let o = run_stick(&mut f, &job, &mut prog, Some(cancel));
                let _ = f.sync_device();
                o
            }
            Err(e) => StickOutcome::fail(format!("nothing was written: {e}")),
        },
        Target::Stick(info) => {
            say("authorize", 0, 1, "", "Waiting for your password or fingerprint.".into());
            match p2u_device::acquire(info) {
                Ok(mut raw) => {
                    let mut o = run_stick(&mut raw, &job, &mut prog, Some(cancel));
                    if o.ok {
                        say("eject", 0, 1, "", "Ejecting the stick.".into());
                        match raw.finish() {
                            Ok(()) => ejected = true,
                            Err(e) => notes.push(format!("The stick was written and checked, but couldn't be ejected: {e}. Eject it before unplugging.")),
                        }
                    }
                    o.erased |= o.cancelled;
                    o
                }
                Err(p2u_device::DeviceError::NeedsElevation) | Err(p2u_device::DeviceError::NoBroker(_)) => {
                    say("write", 0, exact.total_bytes, "", "Writing with administrator rights (no progress is shown).".into());
                    match p2u_device::write_via_helper(info, &job) {
                        Ok(o) => {
                            ejected = o.ok && o.error.is_none();
                            o
                        }
                        Err(e) => StickOutcome::fail(format!("nothing was written: {e}")),
                    }
                }
                Err(p2u_device::DeviceError::NotAuthorized) => StickOutcome::fail("authorization was cancelled. Nothing was written"),
                Err(e) => StickOutcome::fail(format!("nothing was written: {e}")),
            }
        }
    };
    cleanup();

    let mut res = SyncResult {
        ok: outcome.ok,
        cancelled: outcome.cancelled,
        erased: outcome.erased,
        error: outcome.error.clone().map(|e| {
            let mut c = e.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }),
        notes,
        files: exact.file_count(),
        bytes: exact.total_bytes,
        ejected,
        record: None,
        outcome: Some(outcome),
    };
    if res.ok {
        let mut record = plan.record.clone();
        if !record.is_object() {
            record = serde_json::json!({});
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let (stick_name, stick_size) = match &target {
            Target::Image(p) => (p.display().to_string(), size),
            Target::Stick(s) => (s.name.clone(), s.size_bytes),
        };
        record["setId"] = plan.set_id.clone().into();
        record["setName"] = plan.set_name.clone().into();
        record["at"] = now.into();
        record["stickId"] = plan.stick_id.clone().into();
        record["stickName"] = stick_name.into();
        record["stickSize"] = stick_size.into();
        record["files"] = res.files.into();
        record["bytes"] = res.bytes.into();
        record["label"] = exact.format.label.trim_end().into();
        if let Some(r) = res.outcome.as_ref().and_then(|o| o.result.as_ref()) {
            record["volumeSerial"] = sticks::serial_text(r.plan.format.serial).into();
        }
        let _ = lib.lock().unwrap().add_sync(&plan.set_id, &record);
        res.record = Some(record);
    }
    res
}
