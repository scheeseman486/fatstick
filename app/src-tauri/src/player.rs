//! The preview player. The bundled ffmpeg decodes the track to float PCM at
//! the output device's rate (so the player plays exactly what the converter
//! reads), a decoder thread keeps about two seconds buffered, and the device's
//! callback drains the buffer. Seeking restarts the decoder at the new place.
//!
//! A playlist entry plays as it will be on the stick, apart from the encode:
//! from its cut start to its cut end, faded in and out, in its key. The fades
//! and the key are applied as the device takes the audio, after the buffer,
//! so moving a slider (or seeking) is heard at once (the key after the
//! shifter's tenth of a second). A plain track skips all of it.
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
use p2u_audio::render::fade_curve;
use p2u_audio::stretch::{Key, Shifter};
use p2u_audio::Tools;
use serde::Serialize;

/// Where a playlist entry starts and ends in its file (seconds, after the
/// cuts), and its fades.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub start: f64,
    pub end: f64,
    pub fade_in: f64,
    pub fade_out: f64,
}

impl Default for Shape {
    fn default() -> Shape {
        Shape { start: 0.0, end: f64::INFINITY, fade_in: 0.0, fade_out: 0.0 }
    }
}

impl Shape {
    fn plain(&self) -> bool {
        self.fade_in <= 0.0 && self.fade_out <= 0.0 && !self.end.is_finite()
    }
    /// The gain at `t` seconds into the file: 0 past the end.
    fn gain(&self, t: f64) -> f32 {
        if t >= self.end {
            return 0.0;
        }
        let mut g = 1.0;
        if self.fade_in > 0.0 {
            g *= fade_curve((t - self.start) / self.fade_in);
        }
        if self.fade_out > 0.0 && self.end.is_finite() {
            g *= fade_curve((self.end - t) / self.fade_out);
        }
        g as f32
    }
}

pub enum Cmd {
    Play { path: PathBuf, from: f64, gain_db: f64, duration: f64, token: String, key: Option<Key>, shape: Shape },
    /// Another start, end and fades for what's playing (live, from the fade sliders).
    Shape(Shape),
    /// Another key for what's playing (live, from the key slider), or none.
    Key(Option<Key>),
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

/// The key shifter and its buffers, used by the device callback.
#[derive(Default)]
struct Shift {
    shifter: Option<Shifter>,
    input: Vec<f32>,
    output: Vec<f32>,
}

/// Samples shared between the decoder thread and the device callback.
struct Shared {
    buf: Mutex<VecDeque<f32>>,
    shift: Mutex<Shift>,
    shape: Mutex<Shape>,
    /// Where in the file the decoder started (seconds, as f64 bits), and the rate.
    base: AtomicU64,
    rate: AtomicU32,
    /// The device reached the entry's end.
    past_end: AtomicBool,
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
        let playing = self.playing.load(Ordering::Relaxed);
        let n = out.len() / channels;
        let mut frames = 0u64;
        let shape = *self.shape.lock().unwrap();
        let shaped = !shape.plain();
        let (base, rate) = (f64::from_bits(self.base.load(Ordering::Relaxed)), self.rate.load(Ordering::Relaxed).max(1) as f64);
        let f0 = self.frames.load(Ordering::Relaxed);
        let mut sh = self.shift.lock().unwrap();
        let Shift { shifter, input, output } = &mut *sh;
        input.clear();
        {
            let mut buf = self.buf.lock().unwrap();
            for _ in 0..n {
                if playing && buf.len() >= 2 {
                    let (l, r) = (buf.pop_front().unwrap(), buf.pop_front().unwrap());
                    let g = if shaped { shape.gain(base + (f0 + frames) as f64 / rate) } else { 1.0 };
                    if g == 0.0 && base + (f0 + frames) as f64 / rate >= shape.end {
                        self.past_end.store(true, Ordering::Relaxed);
                    }
                    frames += 1;
                    input.push(l * g);
                    input.push(r * g);
                } else {
                    input.push(0.0);
                    input.push(0.0);
                }
            }
        }
        let stereo: &[f32] = match shifter.as_mut() {
            Some(s) if playing => {
                output.resize(input.len(), 0.0);
                s.process(input, output);
                output
            }
            _ => input,
        };
        for (i, frame) in out.chunks_mut(channels).enumerate() {
            let (l, r) = (stereo[2 * i], stereo[2 * i + 1]);
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
    // It goes by the time that has passed, not by how often it wakes: macOS
    // stretches short sleeps a lot when the machine is busy.
    let s = shared.clone();
    let h = std::thread::spawn(move || {
        const RATE: u64 = 44100;
        let start = std::time::Instant::now();
        let mut consumed = 0u64;
        let mut buf = Vec::new();
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let due = (start.elapsed().as_secs_f64() * RATE as f64) as u64;
            // After a long stall (a suspended machine), don't race to catch up.
            consumed = consumed.max(due.saturating_sub(RATE / 4));
            let n = (due - consumed) as usize;
            if n > 0 {
                buf.resize(2 * n, 0.0);
                s.take(&mut buf, 2);
                consumed = due;
            }
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
        shift: Mutex::new(Shift::default()),
        shape: Mutex::new(Shape::default()),
        base: AtomicU64::new(0f64.to_bits()),
        rate: AtomicU32::new(44100),
        past_end: AtomicBool::new(false),
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
        s.past_end.store(false, Ordering::SeqCst);
        if let Some(sh) = s.shift.lock().unwrap().shifter.as_mut() {
            sh.reset();
        }
    };
    // A shifter for the key (made here, never in the device callback), or none.
    let set_key = |s: &Shared, key: Option<Key>, rate: u32, fresh: bool| {
        let mut made = match key {
            Some(_) if fresh || s.shift.lock().unwrap().shifter.is_none() => Some(Shifter::new(2, rate)),
            _ => None,
        };
        let mut sh = s.shift.lock().unwrap();
        match key {
            None => sh.shifter = None,
            Some(k) => {
                if let Some(new) = made.take() {
                    sh.input.reserve(rate as usize);
                    sh.output.reserve(rate as usize);
                    sh.shifter = Some(new);
                }
                if let Some(x) = sh.shifter.as_mut() {
                    x.set_key(k);
                }
            }
        }
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
            Some(Cmd::Play { path, from, gain_db, duration, token, key, shape }) => {
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
                set_key(&shared, key, rate, true);
                // Not before the cut start: what's before it isn't on the stick.
                let from = from.max(shape.start);
                *shared.shape.lock().unwrap() = shape;
                shared.rate.store(rate, Ordering::SeqCst);
                shared.base.store(from.to_bits(), Ordering::SeqCst);
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
                    let start = shared.shape.lock().unwrap().start;
                    base = to.max(start).clamp(0.0, dur.get().max(to));
                    shared.base.store(base.to_bits(), Ordering::SeqCst);
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
            Some(Cmd::Key(key)) => set_key(&shared, key, rate, false),
            Some(Cmd::Shape(sh)) => {
                *shared.shape.lock().unwrap() = sh;
                shared.past_end.store(false, Ordering::SeqCst);
            }
            None => {
                if let Ok(e) = err_rx.try_recv() {
                    shared.playing.store(false, Ordering::SeqCst);
                    current = None;
                    state = "stopped".into();
                    emit(ev("error", pos, Some(e), &token));
                    continue;
                }
                if state == "playing" && current.is_some() {
                    let done = shared.past_end.load(Ordering::SeqCst) || (shared.eof.load(Ordering::SeqCst) && shared.buf.lock().unwrap().len() < 2);
                    if done {
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

#[cfg(test)]
mod key_tests {
    use super::*;

    fn shared(samples: Vec<f32>) -> Shared {
        Shared {
            buf: Mutex::new(samples.into()),
            shift: Mutex::new(Shift::default()),
            shape: Mutex::new(Shape::default()),
            base: AtomicU64::new(0f64.to_bits()),
            rate: AtomicU32::new(44100),
            past_end: AtomicBool::new(false),
            playing: AtomicBool::new(true),
            frames: AtomicU64::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
            eof: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }
    fn tone(hz: f32, secs: f32) -> Vec<f32> {
        (0..(secs * 44100.0) as usize).flat_map(|i| {
            let x = (i as f32 * hz * std::f32::consts::TAU / 44100.0).sin() * 0.5;
            [x, x]
        }).collect()
    }
    fn hz(s: &[f32]) -> f32 {
        let left: Vec<f32> = s.iter().step_by(2).copied().collect();
        let ups = (1..left.len()).filter(|&i| left[i - 1] < 0.0 && left[i] >= 0.0).count();
        ups as f32 * 44100.0 / left.len() as f32
    }
    fn drain(s: &Shared, blocks: usize) -> Vec<f32> {
        let mut all = Vec::new();
        let mut out = vec![0.0f32; 512 * 2];
        for _ in 0..blocks {
            s.take(&mut out, 2);
            all.extend_from_slice(&out);
        }
        all
    }

    #[test]
    fn the_device_hears_the_key_and_follows_the_slider() {
        let s = shared(tone(440.0, 6.0));
        let mut sh = Shifter::new(2, 44100);
        sh.set_key(Key { semitones: 12.0, keep_formants: false });
        s.shift.lock().unwrap().shifter = Some(sh);
        let a = drain(&s, 200); // about 2.3 s
        assert!((hz(&a[44100..]) - 880.0).abs() < 15.0, "an octave up: {}", hz(&a[44100..]));
        // The slider moves: the next blocks are in the new key.
        s.shift.lock().unwrap().shifter.as_mut().unwrap().set_key(Key { semitones: -12.0, keep_formants: false });
        let b = drain(&s, 200);
        assert!((hz(&b[44100..]) - 220.0).abs() < 8.0, "an octave down: {}", hz(&b[44100..]));
        assert_eq!(s.frames.load(Ordering::Relaxed), 400 * 512, "the position counts the source");
    }

    #[test]
    fn without_a_key_it_passes_through() {
        let t = tone(440.0, 1.0);
        let s = shared(t.clone());
        let a = drain(&s, 40);
        assert_eq!(&a[..], &t[..a.len()]);
    }

    #[test]
    fn fades_follow_the_position_and_the_end_stops_it() {
        // 4 s of a steady signal, fading in over 1 s and out over 1 s before a cut end at 3 s.
        let s = shared(vec![0.5f32; 44100 * 2 * 4]);
        *s.shape.lock().unwrap() = Shape { start: 0.0, end: 3.0, fade_in: 1.0, fade_out: 1.0 };
        let a = drain(&s, 300); // 3.5 s
        let at = |t: f64| a[(t * 44100.0) as usize * 2];
        assert!(at(0.001) < 0.01, "starts silent");
        assert!((at(0.5) - 0.25).abs() < 0.01, "half way in (0.5 x 0.5): {}", at(0.5));
        assert!((at(1.5) - 0.5).abs() < 1e-6, "full between");
        assert!((at(2.5) - 0.25).abs() < 0.01, "half way out: {}", at(2.5));
        assert!(at(3.2) == 0.0 && s.past_end.load(Ordering::Relaxed), "past the cut end: silence, and the end is noticed");
        // A seek (the decoder restarts at 2.5 s): the fade follows the new position.
        let s = shared(vec![0.5f32; 44100 * 2]);
        *s.shape.lock().unwrap() = Shape { start: 0.0, end: 3.0, fade_in: 1.0, fade_out: 1.0 };
        s.base.store(2.5f64.to_bits(), Ordering::Relaxed);
        let b = drain(&s, 1);
        assert!((b[0] - 0.25).abs() < 0.01, "2.5 s in: half way out: {}", b[0]);
    }
}
