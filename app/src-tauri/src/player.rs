//! The preview player. The bundled ffmpeg decodes the track to float PCM at
//! the output device's rate (so the player plays exactly what the converter
//! reads), a decoder thread keeps about two seconds buffered, and the device's
//! callback drains the buffer. Seeking restarts the decoder at the new place.
//!
//! The device lives on its own thread (cpal streams aren't Send everywhere);
//! the UI talks to it with commands and hears back through `player` events
//! about ten times a second.

use std::collections::VecDeque;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use p2u_audio::decode::PcmStream;
use p2u_audio::Tools;
use serde::Serialize;

pub enum Cmd {
    Play { path: PathBuf, from: f64, gain_db: f64, duration: f64, token: String },
    Pause,
    Resume,
    Seek(f64),
    Stop,
    Volume(f32),
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayerEvent {
    /// "playing", "paused", "stopped", "ended" or "error".
    pub state: String,
    /// Whatever the UI passed with Play (its entry id).
    pub token: String,
    pub pos: f64,
    pub dur: f64,
    pub error: Option<String>,
}

/// Samples shared between the decoder thread and the device callback.
struct Shared {
    buf: Mutex<VecDeque<f32>>,
    playing: AtomicBool,
    /// Stereo frames handed to the device since the last (re)start.
    frames: AtomicU64,
    volume: AtomicU32,
    /// The decoder reached the end of the file.
    eof: AtomicBool,
    /// Bumped on every Play/Seek/Stop so old decoder threads stop.
    generation: AtomicU64,
}

impl Shared {
    fn take(&self, out: &mut [f32], channels: usize) {
        let vol = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let mut buf = self.buf.lock().unwrap();
        let playing = self.playing.load(Ordering::Relaxed);
        let mut frames = 0u64;
        for frame in out.chunks_mut(channels) {
            let (l, r) = if playing && buf.len() >= 2 {
                frames += 1;
                (buf.pop_front().unwrap(), buf.pop_front().unwrap())
            } else {
                (0.0, 0.0)
            };
            match frame.len() {
                1 => frame[0] = (l + r) * 0.5 * vol,
                _ => {
                    frame[0] = l * vol;
                    frame[1] = r * vol;
                    for x in &mut frame[2..] {
                        *x = 0.0;
                    }
                }
            }
        }
        self.frames.fetch_add(frames, Ordering::Relaxed);
    }
}

/// An open output: a cpal stream, or a silent clock when there's no device.
enum Output {
    Device(#[allow(dead_code)] cpal::Stream),
    Null(#[allow(dead_code)] std::thread::JoinHandle<()>),
}

fn open_output(shared: &Arc<Shared>) -> Result<(Output, u32), String> {
    if std::env::var_os("P2U_NULL_AUDIO").is_none() {
        let host = cpal::default_host();
        if let Some(dev) = host.default_output_device() {
            let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
            let rate = cfg.sample_rate().0;
            let ch = cfg.channels() as usize;
            let sc: cpal::StreamConfig = cfg.clone().into();
            let err = |e| eprintln!("audio output: {e}");
            let s = shared.clone();
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::F32 => dev.build_output_stream(&sc, move |out: &mut [f32], _| s.take(out, ch), err, None),
                cpal::SampleFormat::I16 => {
                    let mut tmp = Vec::new();
                    dev.build_output_stream(
                        &sc,
                        move |out: &mut [i16], _| {
                            tmp.resize(out.len(), 0.0);
                            s.take(&mut tmp, ch);
                            for (o, x) in out.iter_mut().zip(&tmp) {
                                *o = (x.clamp(-1.0, 1.0) * 32767.0) as i16;
                            }
                        },
                        err,
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    let mut tmp = Vec::new();
                    dev.build_output_stream(
                        &sc,
                        move |out: &mut [u16], _| {
                            tmp.resize(out.len(), 0.0);
                            s.take(&mut tmp, ch);
                            for (o, x) in out.iter_mut().zip(&tmp) {
                                *o = ((x.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16;
                            }
                        },
                        err,
                        None,
                    )
                }
                f => return Err(format!("the audio output wants {f:?} samples, which the player doesn't handle")),
            }
            .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;
            return Ok((Output::Device(stream), rate));
        }
        if std::env::var_os("P2U_NULL_AUDIO_FALLBACK").is_none() {
            return Err("No audio output device was found.".into());
        }
    }
    // A clock that consumes samples in real time, silently (tests, headless).
    let s = shared.clone();
    let h = std::thread::spawn(move || {
        let mut buf = vec![0f32; 2 * 441];
        loop {
            std::thread::sleep(Duration::from_millis(10));
            s.take(&mut buf, 2);
            if Arc::strong_count(&s) == 1 {
                break;
            }
        }
    });
    Ok((Output::Null(h), 44100))
}

fn spawn_decoder(tools: Tools, shared: Arc<Shared>, path: PathBuf, from: f64, gain_db: f64, rate: u32, err_tx: Sender<String>) {
    let gen = shared.generation.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let mut pcm = match PcmStream::start(&tools, &path, from, rate, gain_db) {
            Ok(p) => p,
            Err(e) => {
                let _ = err_tx.send(e.to_string());
                return;
            }
        };
        let mut raw = vec![0u8; 16384];
        let mut carry: Vec<u8> = Vec::new();
        let high = rate as usize * 2 * 2; // two seconds of stereo samples
        let mut got_any = false;
        loop {
            if shared.generation.load(Ordering::SeqCst) != gen {
                return;
            }
            if shared.buf.lock().unwrap().len() > high {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match pcm.out.read(&mut raw) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    got_any = true;
                    carry.extend_from_slice(&raw[..n]);
                    let whole = carry.len() / 4 * 4;
                    let samples: Vec<f32> = carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
                    carry.drain(..whole);
                    if shared.generation.load(Ordering::SeqCst) != gen {
                        return;
                    }
                    shared.buf.lock().unwrap().extend(samples);
                }
            }
        }
        if shared.generation.load(Ordering::SeqCst) == gen {
            if !got_any {
                let _ = err_tx.send(format!("{} couldn't be decoded.", path.display()));
            }
            shared.eof.store(true, Ordering::SeqCst);
        }
    });
}

#[derive(Clone)]
pub struct Player {
    tx: Sender<Cmd>,
}

impl Player {
    pub fn start(tools: Option<Tools>, emit: impl Fn(PlayerEvent) + Send + 'static) -> Player {
        let (tx, rx) = channel();
        std::thread::spawn(move || run(tools, rx, emit));
        Player { tx }
    }
    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
}

fn run(tools: Option<Tools>, rx: Receiver<Cmd>, emit: impl Fn(PlayerEvent)) {
    let shared = Arc::new(Shared {
        buf: Mutex::new(VecDeque::new()),
        playing: AtomicBool::new(false),
        frames: AtomicU64::new(0),
        volume: AtomicU32::new(0.8f32.to_bits()),
        eof: AtomicBool::new(false),
        generation: AtomicU64::new(0),
    });
    let mut output: Option<(Output, u32)> = None;
    let (err_tx, err_rx) = channel::<String>();
    let mut current: Option<(PathBuf, f64, String)> = None; // path, gain, token
    let mut base = 0.0f64;
    let dur = std::cell::Cell::new(0.0f64);
    let mut state = "stopped".to_string();
    let reset = |s: &Shared| {
        s.generation.fetch_add(1, Ordering::SeqCst);
        s.buf.lock().unwrap().clear();
        s.frames.store(0, Ordering::SeqCst);
        s.eof.store(false, Ordering::SeqCst);
    };
    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let rate = output.as_ref().map(|o| o.1).unwrap_or(44100);
        let pos = base + shared.frames.load(Ordering::Relaxed) as f64 / rate as f64;
        let token = current.as_ref().map(|c| c.2.clone()).unwrap_or_default();
        let ev = |state: &str, pos: f64, error: Option<String>, token: &str| PlayerEvent { state: state.into(), token: token.into(), pos, dur: dur.get(), error };
        match cmd {
            Some(Cmd::Play { path, from, gain_db, duration, token }) => {
                let Some(t) = tools.clone() else {
                    emit(ev("error", 0.0, Some("ffmpeg isn't available, so nothing can play.".into()), &token));
                    continue;
                };
                if output.is_none() {
                    match open_output(&shared) {
                        Ok(o) => output = Some(o),
                        Err(e) => {
                            emit(ev("error", 0.0, Some(e), &token));
                            continue;
                        }
                    }
                }
                reset(&shared);
                while err_rx.try_recv().is_ok() {}
                let rate = output.as_ref().unwrap().1;
                base = from;
                dur.set(duration);
                current = Some((path.clone(), gain_db, token.clone()));
                spawn_decoder(t, shared.clone(), path, from, gain_db, rate, err_tx.clone());
                shared.playing.store(true, Ordering::SeqCst);
                state = "playing".into();
                emit(ev(&state, from, None, &token));
            }
            Some(Cmd::Pause) => {
                shared.playing.store(false, Ordering::SeqCst);
                if current.is_some() {
                    state = "paused".into();
                    emit(ev(&state, pos, None, &token));
                }
            }
            Some(Cmd::Resume) => {
                if current.is_some() {
                    shared.playing.store(true, Ordering::SeqCst);
                    state = "playing".into();
                    emit(ev(&state, pos, None, &token));
                }
            }
            Some(Cmd::Seek(to)) => {
                if let (Some((path, gain, tok)), Some(t), Some((_, rate))) = (current.clone(), tools.clone(), output.as_ref()) {
                    reset(&shared);
                    base = to.clamp(0.0, dur.get().max(to));
                    spawn_decoder(t, shared.clone(), path, base, gain, *rate, err_tx.clone());
                    emit(ev(&state, base, None, &tok));
                }
            }
            Some(Cmd::Stop) => {
                shared.playing.store(false, Ordering::SeqCst);
                reset(&shared);
                current = None;
                state = "stopped".into();
                emit(ev(&state, 0.0, None, &token));
            }
            Some(Cmd::Volume(v)) => shared.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed),
            None => {
                if let Ok(e) = err_rx.try_recv() {
                    shared.playing.store(false, Ordering::SeqCst);
                    current = None;
                    state = "stopped".into();
                    emit(ev("error", pos, Some(e), &token));
                    continue;
                }
                if state == "playing" && current.is_some() {
                    if shared.eof.load(Ordering::SeqCst) && shared.buf.lock().unwrap().len() < 2 {
                        shared.playing.store(false, Ordering::SeqCst);
                        current = None;
                        state = "stopped".into();
                        emit(ev("ended", pos, None, &token));
                    } else {
                        emit(ev("playing", pos, None, &token));
                    }
                }
            }
        }
    }
}
