//! Background work while you edit: measuring loudness for playlists that even
//! out the volume, and preparing the files the next sync will need (encodes
//! and gain-adjusted copies) in the cache, so pressing Sync is mostly writing.
//!
//! The window hands over the whole preparation queue whenever a stick's
//! contents change, current stick first; analysis requests accumulate.
//! Workers use all cores but one and stand aside while a sync runs.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use p2u_audio::prepare::{prepare, ready, Recipe};
use p2u_audio::{loudness, Cache, Tools};
use p2u_library::{scan, Library};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepJob {
    /// The window's own name for this job, echoed back in events.
    pub key: String,
    pub track_id: String,
    pub recipe: Recipe,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub key: String,
    pub ok: bool,
    pub error: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analyzed {
    pub track_id: String,
    pub lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Queues {
    prepare: VecDeque<PrepJob>,
    analyze: VecDeque<String>,
    busy: HashSet<String>,
}

pub struct Work {
    q: Mutex<Queues>,
    cv: Condvar,
    pub paused: AtomicBool,
}

pub enum Event {
    Prepared(Prepared),
    Analyzed(Analyzed),
}

impl Work {
    pub fn start(lib: Arc<Mutex<Library>>, tools: Option<Tools>, cache: Cache, emit: impl Fn(Event) + Send + Sync + 'static) -> Arc<Work> {
        let w = Arc::new(Work { q: Mutex::new(Queues::default()), cv: Condvar::new(), paused: AtomicBool::new(false) });
        let Some(tools) = tools else { return w };
        let emit = Arc::new(emit);
        let n = std::thread::available_parallelism().map(|n| n.get().saturating_sub(1).clamp(1, 4)).unwrap_or(2);
        for _ in 0..n {
            let (w, lib, tools, cache, emit) = (w.clone(), lib.clone(), tools.clone(), cache.clone(), emit.clone());
            std::thread::spawn(move || loop {
                let job = {
                    let mut q = w.q.lock().unwrap();
                    loop {
                        if !w.paused.load(Ordering::Relaxed) {
                            if let Some(i) = q.prepare.iter().position(|j| !q.busy.contains(&j.key)) {
                                let j = q.prepare.remove(i).unwrap();
                                q.busy.insert(j.key.clone());
                                break Some(Ok(j));
                            }
                            if let Some(id) = q.analyze.pop_front() {
                                break Some(Err(id));
                            }
                        }
                        q = w.cv.wait_timeout(q, Duration::from_secs(1)).unwrap().0;
                    }
                };
                match job {
                    Some(Ok(j)) => {
                        let r = prepare_one(&lib, &tools, &cache, &j);
                        w.q.lock().unwrap().busy.remove(&j.key);
                        emit(Event::Prepared(r));
                    }
                    Some(Err(id)) => emit(Event::Analyzed(analyze_one(&lib, &tools, &id))),
                    None => {}
                }
            });
        }
        w
    }

    /// Replace the preparation queue. Returns the keys that are ready already.
    pub fn set_queue(&self, lib: &Mutex<Library>, cache: &Cache, jobs: Vec<PrepJob>) -> Vec<String> {
        let mut done = Vec::new();
        let mut todo = VecDeque::new();
        {
            let l = lib.lock().unwrap();
            for j in jobs {
                let r = l.probe_of(&j.track_id).ok().flatten();
                match r {
                    Some((path, key, probe)) if ready(cache, &path, &probe, &key, &j.recipe).is_some() => done.push(j.key),
                    _ => todo.push_back(j),
                }
            }
        }
        self.q.lock().unwrap().prepare = todo;
        self.cv.notify_all();
        done
    }

    pub fn analyze(&self, ids: Vec<String>) {
        let mut q = self.q.lock().unwrap();
        for id in ids {
            if !q.analyze.contains(&id) {
                q.analyze.push_back(id);
            }
        }
        self.cv.notify_all();
    }
}

/// The track's file as last read, re-read first if it changed on disk.
pub fn current_probe(lib: &Mutex<Library>, tools: &Tools, track_id: &str) -> Result<(std::path::PathBuf, String, p2u_audio::Probe), String> {
    let (path, _, _) = lib.lock().unwrap().probe_of(track_id).map_err(|e| e.to_string())?.ok_or_else(|| format!("{track_id} hasn't been read yet"))?;
    let stale = {
        let l = lib.lock().unwrap();
        let t = l.track(track_id).map_err(|e| e.to_string())?;
        t.map(|t| !scan::is_current(&l, &t)).unwrap_or(true)
    };
    if !Path::new(&path).is_file() {
        return Err(format!("{} isn't there any more", path.display()));
    }
    if stale {
        scan::read_file(lib, tools, &path).map_err(|e| e.to_string())?;
    }
    lib.lock().unwrap().probe_of(track_id).map_err(|e| e.to_string())?.ok_or_else(|| format!("{} couldn't be read", path.display()))
}

fn prepare_one(lib: &Mutex<Library>, tools: &Tools, cache: &Cache, j: &PrepJob) -> Prepared {
    let r = current_probe(lib, tools, &j.track_id).and_then(|(path, key, probe)| prepare(tools, cache, &path, &probe, &key, &j.recipe).map_err(|e| e.to_string()));
    match r {
        Ok(p) => Prepared { key: j.key.clone(), ok: true, error: None, bytes: p.len },
        Err(e) => Prepared { key: j.key.clone(), ok: false, error: Some(e), bytes: 0 },
    }
}

fn analyze_one(lib: &Mutex<Library>, tools: &Tools, id: &str) -> Analyzed {
    let fail = |e: String| Analyzed { track_id: id.to_string(), lufs: None, true_peak: None, error: Some(e) };
    let (path, key, _) = match current_probe(lib, tools, id) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if let Ok(Some(l)) = lib.lock().unwrap().analysis(&key) {
        return Analyzed { track_id: id.into(), lufs: Some(l.integrated), true_peak: Some(l.true_peak), error: None };
    }
    match loudness::measure(tools, &path) {
        Ok(l) => {
            let _ = lib.lock().unwrap().put_analysis(&key, &l);
            Analyzed { track_id: id.into(), lufs: Some(l.integrated), true_peak: Some(l.true_peak), error: None }
        }
        Err(e) => fail(e.to_string()),
    }
}

/// For the player: the prepared file if the recipe's result is in the cache.
pub fn cached_for(lib: &Mutex<Library>, cache: &Cache, track_id: &str, recipe: &Recipe) -> Option<std::path::PathBuf> {
    if matches!(recipe, Recipe::Copy) {
        return None;
    }
    let (path, key, probe) = lib.lock().unwrap().probe_of(track_id).ok().flatten()?;
    ready(cache, &path, &probe, &key, recipe).map(|p| p.path)
}
