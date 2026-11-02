//! The preview player. The bundled ffmpeg decodes the track to float PCM at
//! the output device's rate (so the player plays exactly what the converter
//! reads), a decoder thread keeps about two seconds buffered, and the device's
//! callback drains the buffer. Seeking restarts the decoder at the new place.
//!
//! A playlist entry plays as it will be on the stick, apart from the encode:
//! the silence added before it (when it plays from its start), then from its
//! cut start to its cut end, faded in and out, in its key, then the silence
//! added after it. The silences, fades and key are applied as the device
//! takes the audio, after the buffer, so moving a slider (or seeking) is
//! heard at once (the key after the shifter's tenth of a second). A plain
//! track skips all of it.
//!
//! That live way is for trying things in the dialogs. Ordinary playback is an
//! `Item`: a playlist's track or a setlist's file, as the stick will have it.
//! Its parts (a track with its cuts, fades, key, gain and silences; a whole
//! joined playlist; a stretch of silence) come from the renderer as one stream,
//! the one the encoder gets, into the same buffer, with nothing left to do after
//! it. And it's gapless: what the window queues joins the same stream, ahead of
//! time, with a mark where it starts. The device never stops, and at that frame
//! the position starts again and the window hears that the next one is playing.
//!
//! The device lives on its own thread (cpal streams aren't Send everywhere);
//! the UI talks to it with commands and hears back through `player` events
//! about ten times a second.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fatstick_audio::decode::PcmStream;
use fatstick_audio::render::{self, FadeShape, RenderPart, Source};
use fatstick_audio::stretch::{Key, Shifter};
use fatstick_audio::Tools;
use serde::Serialize;

/// Where a playlist entry starts and ends in its file (seconds, after the
/// cuts), its fades, and the silence added before and after it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub start: f64,
    pub end: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub fade_in_shape: FadeShape,
    pub fade_out_shape: FadeShape,
    pub pad_start: f64,
    pub pad_end: f64,
}

impl Default for Shape {
    fn default() -> Shape {
        Shape { start: 0.0, end: f64::INFINITY, fade_in: 0.0, fade_out: 0.0, fade_in_shape: FadeShape::Smooth, fade_out_shape: FadeShape::Smooth, pad_start: 0.0, pad_end: 0.0 }
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
            g *= self.fade_in_shape.gain((t - self.start) / self.fade_in, false);
        }
        if self.fade_out > 0.0 && self.end.is_finite() {
            g *= self.fade_out_shape.gain((self.end - t) / self.fade_out, true);
        }
        g as f32
    }
}

/// One track or stick file as the renderer makes it, and how long each part's track is.
pub struct Item {
    /// Whatever the UI passed (it hears this back as the item starts).
    pub token: String,
    /// Seconds, in all.
    pub duration: f64,
    pub parts: Vec<RenderPart>,
    pub sources: Vec<Option<Source>>,
    pub lengths: Vec<f64>,
}

/// What's playing, for starting it again from another place.
#[derive(Clone)]
enum Playing {
    File { path: PathBuf, gain_db: f64 },
    Stream(Arc<Item>),
}

pub enum Cmd {
    Play { path: PathBuf, from: f64, gain_db: f64, duration: f64, token: String, key: Option<Key>, shape: Shape },
    /// An item, made as it plays.
    PlayStream { item: Item, from: f64 },
    /// What follows the item playing, with nothing between them (none: nothing yet).
    Queue(Option<Item>),
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
    /// "playing", "paused", "stopped", "ended", "error", or "next": what was
    /// queued (this token) has just started.
    pub state: String,
    /// Whatever the UI passed with Play (its entry id).
    pub token: String,
    pub pos: f64,
    pub dur: f64,
    pub error: Option<String>,
    /// Seconds the device has waited for audio since this began playing: 0 is gapless.
    pub starved: f64,
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
    /// The device reached the entry's end (and played the silence after it).
    past_end: AtomicBool,
    /// Frames of silence still to play before the audio (the silence added
    /// before the entry, when it plays from its start).
    lead: AtomicU64,
    /// Frames of silence played since the entry's end.
    tail: AtomicU64,
    playing: AtomicBool,
    /// Stereo frames handed to the device since the last (re)start.
    frames: AtomicU64,
    volume: AtomicU32,
    /// The decoder reached the end of the file.
    eof: AtomicBool,
    /// Bumped on every Play/Seek/Stop so old decoder threads stop.
    generation: AtomicU64,
    /// What follows the item playing, for the stream to take when it's ready.
    queue: Mutex<Option<Arc<Item>>>,
    /// The items in the stream after the one playing: where each starts, in
    /// frames since the last (re)start.
    marks: Mutex<VecDeque<(u64, Arc<Item>)>>,
    /// Items the device has reached, for the player to tell the window.
    reached: Mutex<VecDeque<Arc<Item>>>,
    /// Frames taken from the buffer since the last (re)start.
    taken: AtomicU64,
    /// Frames the device waited for audio, once it had begun.
    starved: AtomicU64,
}

impl Shared {
    fn take(&self, out: &mut [f32], channels: usize) {
        let vol = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let playing = self.playing.load(Ordering::Relaxed);
        let n = out.len() / channels;
        let mut frames = 0u64;
        let shape = *self.shape.lock().unwrap();
        let shaped = !shape.plain();
        let (mut base, rate) = (f64::from_bits(self.base.load(Ordering::Relaxed)), self.rate.load(Ordering::Relaxed).max(1) as f64);
        let mut f0 = self.frames.load(Ordering::Relaxed);
        let mut taken = self.taken.load(Ordering::Relaxed);
        let mut starved = 0u64;
        let pad_end = (shape.pad_end.max(0.0) * rate) as u64;
        let (mut lead, mut tail) = (self.lead.load(Ordering::Relaxed), self.tail.load(Ordering::Relaxed));
        let mut sh = self.shift.lock().unwrap();
        let Shift { shifter, input, output } = &mut *sh;
        input.clear();
        {
            let mut buf = self.buf.lock().unwrap();
            let eof = self.eof.load(Ordering::Relaxed);
            let mut mark = if playing { self.marks.lock().unwrap().front().map(|m| m.0) } else { None };
            for _ in 0..n {
                // The next item starts here, in the same stream: its position
                // runs from 0, and the player tells the window.
                if mark.is_some_and(|at| taken >= at) {
                    let mut marks = self.marks.lock().unwrap();
                    if let Some((_, item)) = marks.pop_front() {
                        self.reached.lock().unwrap().push_back(item);
                    }
                    mark = marks.front().map(|m| m.0);
                    self.base.store(0f64.to_bits(), Ordering::Relaxed);
                    self.frames.store(0, Ordering::Relaxed);
                    (base, f0, frames) = (0.0, 0, 0);
                }
                let (mut l, mut r) = (0.0, 0.0);
                if !playing {
                } else if lead > 0 {
                    // The silence before the entry: the position waits at its start.
                    lead -= 1;
                } else if base + (f0 + frames) as f64 / rate >= shape.end || (eof && buf.len() < 2) {
                    // Past the end: the silence after it, then the end is noticed.
                    if tail >= pad_end {
                        self.past_end.store(true, Ordering::Relaxed);
                    } else {
                        tail += 1;
                    }
                } else if buf.len() >= 2 {
                    let g = if shaped { shape.gain(base + (f0 + frames) as f64 / rate) } else { 1.0 };
                    l = buf.pop_front().unwrap() * g;
                    r = buf.pop_front().unwrap() * g;
                    frames += 1;
                    taken += 1;
                } else if taken > 0 {
                    starved += 1;
                }
                input.push(l);
                input.push(r);
            }
        }
        self.lead.store(lead, Ordering::Relaxed);
        self.tail.store(tail, Ordering::Relaxed);
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
        self.taken.store(taken, Ordering::Relaxed);
        self.starved.fetch_add(starved, Ordering::Relaxed);
    }
}

/// An open output: a cpal stream, or a silent clock when there's no device.
enum Output {
    Device(#[allow(dead_code)] cpal::Stream),
    Null(#[allow(dead_code)] std::thread::JoinHandle<()>),
}

fn open_output(shared: &Arc<Shared>) -> Result<(Output, u32), String> {
    if std::env::var_os("FATSTICK_NULL_AUDIO").is_none() {
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
        if std::env::var_os("FATSTICK_NULL_AUDIO_FALLBACK").is_none() {
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

/// Playing from the entry's start begins with the silence added before it.
fn lead_in(shared: &Shared, shape: &Shape, from: f64, rate: u32) {
    let frames = if from <= shape.start + 0.01 { (shape.pad_start.max(0.0) * rate as f64) as u64 } else { 0 };
    shared.lead.store(frames, Ordering::SeqCst);
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

/// Takes the renderer's PCM into the buffer, at the pace the device plays it.
/// When the player has moved on, the write fails and the render stops.
struct Sink<'a> {
    shared: &'a Shared,
    generation: u64,
    high: usize,
    carry: Vec<u8>,
    /// Frames written so far: where in the stream the next item will start.
    frames: u64,
}

impl Write for Sink<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let moved_on = || std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the player moved on");
        self.carry.extend_from_slice(data);
        let whole = self.carry.len() / 4 * 4;
        let samples: Vec<f32> = self.carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        self.carry.drain(..whole);
        loop {
            let mut buf = self.shared.buf.lock().unwrap();
            if self.shared.generation.load(Ordering::SeqCst) != self.generation {
                return Err(moved_on());
            }
            if buf.len() <= self.high {
                self.frames += samples.len() as u64 / 2;
                buf.extend(samples);
                return Ok(data.len());
            }
            drop(buf);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The stream: the item from `from`, then what's queued, for as long as there's
/// more. The buffer's two seconds ahead, so the next decoder starts well before
/// the device gets there. With nothing queued the device can run out, and then
/// that's the end; something queued in time carries on.
fn spawn_stream(tools: Tools, shared: Arc<Shared>, item: Arc<Item>, from: f64, rate: u32, err_tx: Sender<String>) {
    let generation = shared.generation.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let stale = || shared.generation.load(Ordering::SeqCst) != generation;
        let mut sink = Sink { shared: &shared, generation, high: rate as usize * 2 * 2, carry: Vec::new(), frames: 0 };
        let (mut item, mut from) = (item, from);
        loop {
            let r = render::stream(&tools, &item.parts, &item.sources, &item.lengths, from, rate, &mut sink);
            if stale() {
                return;
            }
            shared.eof.store(true, Ordering::SeqCst);
            if let Err(e) = r {
                // What's in the buffer still plays: a queued file that can't be
                // read doesn't cut short the one before it.
                while !stale() && shared.buf.lock().unwrap().len() >= 2 {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if !stale() {
                    let _ = err_tx.send(e.to_string());
                }
                return;
            }
            let next = loop {
                if stale() {
                    return;
                }
                if let Some(n) = shared.queue.lock().unwrap().take() {
                    break n;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            {
                // Under the buffer's lock, so the device sees both or neither.
                let _buf = shared.buf.lock().unwrap();
                if stale() {
                    return;
                }
                shared.eof.store(false, Ordering::SeqCst);
                shared.marks.lock().unwrap().push_back((sink.frames, next.clone()));
            }
            (item, from) = (next, 0.0);
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
        lead: AtomicU64::new(0),
        tail: AtomicU64::new(0),
        playing: AtomicBool::new(false),
        frames: AtomicU64::new(0),
        volume: AtomicU32::new(0.8f32.to_bits()),
        eof: AtomicBool::new(false),
        generation: AtomicU64::new(0),
        queue: Mutex::new(None),
        marks: Mutex::new(VecDeque::with_capacity(4)),
        reached: Mutex::new(VecDeque::with_capacity(4)),
        taken: AtomicU64::new(0),
        starved: AtomicU64::new(0),
    });
    let mut output: Option<(Output, u32)> = None;
    let (err_tx, err_rx) = channel::<String>();
    let mut current: Option<(Playing, String)> = None;
    let dur = std::cell::Cell::new(0.0f64);
    let mut state = "stopped".to_string();
    let reset = |s: &Shared| {
        s.generation.fetch_add(1, Ordering::SeqCst);
        s.buf.lock().unwrap().clear();
        s.frames.store(0, Ordering::SeqCst);
        s.eof.store(false, Ordering::SeqCst);
        s.past_end.store(false, Ordering::SeqCst);
        s.lead.store(0, Ordering::SeqCst);
        s.tail.store(0, Ordering::SeqCst);
        s.marks.lock().unwrap().clear();
        s.reached.lock().unwrap().clear();
        s.taken.store(0, Ordering::SeqCst);
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
        let starved = shared.starved.load(Ordering::Relaxed) as f64 / rate as f64;
        // The device has reached what was queued: that's what's playing now.
        loop {
            let Some(item) = shared.reached.lock().unwrap().pop_front() else { break };
            dur.set(item.duration);
            emit(PlayerEvent { state: "next".into(), token: item.token.clone(), pos: 0.0, dur: item.duration, error: None, starved });
            current = Some((Playing::Stream(item.clone()), item.token.clone()));
        }
        let pos = f64::from_bits(shared.base.load(Ordering::Relaxed)) + shared.frames.load(Ordering::Relaxed) as f64 / rate as f64;
        let token = current.as_ref().map(|c| c.1.clone()).unwrap_or_default();
        let ev = |state: &str, pos: f64, error: Option<String>, token: &str| PlayerEvent { state: state.into(), token: token.into(), pos, dur: dur.get(), error, starved };
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
                shared.starved.store(0, Ordering::SeqCst);
                *shared.queue.lock().unwrap() = None;
                while err_rx.try_recv().is_ok() {}
                let rate = output.as_ref().unwrap().1;
                set_key(&shared, key, rate, true);
                // Not before the cut start: what's before it isn't on the stick.
                let from = from.max(shape.start);
                lead_in(&shared, &shape, from, rate);
                *shared.shape.lock().unwrap() = shape;
                shared.rate.store(rate, Ordering::SeqCst);
                shared.base.store(from.to_bits(), Ordering::SeqCst);
                dur.set(duration);
                current = Some((Playing::File { path: path.clone(), gain_db }, token.clone()));
                spawn_decoder(t, shared.clone(), path, from, gain_db, rate, err_tx.clone());
                shared.playing.store(true, Ordering::SeqCst);
                state = "playing".into();
                emit(ev(&state, from, None, &token));
            }
            Some(Cmd::PlayStream { item, from }) => {
                let token = item.token.clone();
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
                shared.starved.store(0, Ordering::SeqCst);
                *shared.queue.lock().unwrap() = None;
                while err_rx.try_recv().is_ok() {}
                let rate = output.as_ref().unwrap().1;
                // Everything is in the stream: nothing to do after the buffer.
                set_key(&shared, None, rate, true);
                *shared.shape.lock().unwrap() = Shape::default();
                let from = from.max(0.0);
                shared.rate.store(rate, Ordering::SeqCst);
                shared.base.store(from.to_bits(), Ordering::SeqCst);
                dur.set(item.duration);
                let item = Arc::new(item);
                current = Some((Playing::Stream(item.clone()), token.clone()));
                spawn_stream(t, shared.clone(), item, from, rate, err_tx.clone());
                shared.playing.store(true, Ordering::SeqCst);
                state = "playing".into();
                emit(ev(&state, from, None, &token));
            }
            Some(Cmd::Queue(item)) => {
                if matches!(current, Some((Playing::Stream(_), _))) {
                    *shared.queue.lock().unwrap() = item.map(Arc::new);
                }
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
                if let (Some((what, tok)), Some(t), Some((_, rate))) = (current.clone(), tools.clone(), output.as_ref()) {
                    // What the stream had taken from the queue is still to come.
                    let taken = shared.marks.lock().unwrap().front().map(|m| m.1.clone());
                    reset(&shared);
                    if taken.is_some() {
                        let mut q = shared.queue.lock().unwrap();
                        if q.is_none() {
                            *q = taken;
                        }
                    }
                    let shape = *shared.shape.lock().unwrap();
                    let base = to.max(shape.start).clamp(0.0, dur.get().max(to));
                    lead_in(&shared, &shape, base, *rate);
                    shared.base.store(base.to_bits(), Ordering::SeqCst);
                    match what {
                        Playing::File { path, gain_db } => spawn_decoder(t, shared.clone(), path, base, gain_db, *rate, err_tx.clone()),
                        Playing::Stream(item) => spawn_stream(t, shared.clone(), item, base, *rate, err_tx.clone()),
                    }
                    emit(ev(&state, base, None, &tok));
                }
            }
            Some(Cmd::Stop) => {
                shared.playing.store(false, Ordering::SeqCst);
                reset(&shared);
                *shared.queue.lock().unwrap() = None;
                current = None;
                state = "stopped".into();
                emit(ev(&state, 0.0, None, &token));
            }
            Some(Cmd::Volume(v)) => shared.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed),
            Some(Cmd::Key(key)) => set_key(&shared, key, rate, false),
            Some(Cmd::Shape(sh)) => {
                *shared.shape.lock().unwrap() = sh;
                shared.tail.store(0, Ordering::SeqCst);
                shared.past_end.store(false, Ordering::SeqCst);
            }
            None => {
                if let Ok(e) = err_rx.try_recv() {
                    shared.playing.store(false, Ordering::SeqCst);
                    reset(&shared);
                    *shared.queue.lock().unwrap() = None;
                    current = None;
                    state = "stopped".into();
                    emit(ev("error", pos, Some(e), &token));
                    continue;
                }
                if state == "playing" && current.is_some() {
                    // The device notices the end (the file's, or the cut's) once
                    // the silence after it has played.
                    let done = shared.past_end.load(Ordering::SeqCst);
                    if done {
                        shared.playing.store(false, Ordering::SeqCst);
                        reset(&shared);
                        *shared.queue.lock().unwrap() = None;
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
            lead: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            playing: AtomicBool::new(true),
            frames: AtomicU64::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
            eof: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            queue: Mutex::new(None),
            marks: Mutex::new(VecDeque::new()),
            reached: Mutex::new(VecDeque::new()),
            taken: AtomicU64::new(0),
            starved: AtomicU64::new(0),
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
    fn the_next_item_starts_at_its_exact_frame_with_nothing_between() {
        // Three items in one stream, 700, 1 and 900 frames, none of them a whole
        // number of the device's blocks.
        let item = |t: &str| Arc::new(Item { token: t.into(), duration: 1.0, parts: vec![], sources: vec![], lengths: vec![] });
        let want: Vec<f32> = (0..1601).flat_map(|i| [(i + 1) as f32 / 4000.0, -((i + 1) as f32) / 4000.0]).collect();
        let s = shared(want.clone());
        s.marks.lock().unwrap().extend([(700u64, item("b")), (701u64, item("c"))]);
        s.base.store(5f64.to_bits(), Ordering::SeqCst);
        s.eof.store(true, Ordering::SeqCst);
        let mut out = vec![0.0f32; 512 * 2];
        s.take(&mut out, 2);
        assert_eq!(&out[..], &want[..1024], "the first block is the first item");
        assert!(s.reached.lock().unwrap().is_empty(), "not there yet");
        assert_eq!(s.frames.load(Ordering::SeqCst), 512);
        assert_eq!(f64::from_bits(s.base.load(Ordering::SeqCst)), 5.0, "its position goes on");
        s.take(&mut out, 2);
        assert_eq!(&out[..], &want[1024..2048], "the second has both joins in it, and no hole");
        let reached: Vec<String> = s.reached.lock().unwrap().iter().map(|i| i.token.clone()).collect();
        assert_eq!(reached, ["b", "c"], "the device got to both, in order");
        assert_eq!(f64::from_bits(s.base.load(Ordering::SeqCst)), 0.0, "the position starts again");
        assert_eq!(s.frames.load(Ordering::SeqCst), 1024 - 701, "from where the last one started");
        assert!(!s.past_end.load(Ordering::SeqCst));
        let mut rest = Vec::new();
        while !s.past_end.load(Ordering::SeqCst) {
            s.take(&mut out, 2);
            rest.extend_from_slice(&out);
        }
        assert_eq!(&rest[..want.len() - 2048], &want[2048..], "and the rest, to the last frame");
        assert!(s.marks.lock().unwrap().is_empty());
        assert_eq!(s.starved.load(Ordering::SeqCst), 0, "it never waited");
    }

    #[test]
    fn a_device_kept_waiting_is_counted() {
        let s = shared(vec![0.5; 200]);
        let mut out = vec![0.0f32; 512 * 2];
        s.take(&mut out, 2);
        assert_eq!(s.starved.load(Ordering::SeqCst), 412, "100 frames, then waiting");
        assert!(!s.past_end.load(Ordering::SeqCst), "more may come");
        // Before the first audio it's only starting up.
        let t = shared(Vec::new());
        t.take(&mut out, 2);
        assert_eq!(t.starved.load(Ordering::SeqCst), 0);
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
        *s.shape.lock().unwrap() = Shape { start: 0.0, end: 3.0, fade_in: 1.0, fade_out: 1.0, ..Default::default() };
        let a = drain(&s, 300); // 3.5 s
        let at = |t: f64| a[(t * 44100.0) as usize * 2];
        assert!(at(0.001) < 0.01, "starts silent");
        assert!((at(0.5) - 0.25).abs() < 0.01, "half way in (0.5 x 0.5): {}", at(0.5));
        assert!((at(1.5) - 0.5).abs() < 1e-6, "full between");
        assert!((at(2.5) - 0.25).abs() < 0.01, "half way out: {}", at(2.5));
        assert!(at(3.2) == 0.0 && s.past_end.load(Ordering::Relaxed), "past the cut end: silence, and the end is noticed");
        // A seek (the decoder restarts at 2.5 s): the fade follows the new position.
        let s = shared(vec![0.5f32; 44100 * 2]);
        *s.shape.lock().unwrap() = Shape { start: 0.0, end: 3.0, fade_in: 1.0, fade_out: 1.0, ..Default::default() };
        s.base.store(2.5f64.to_bits(), Ordering::Relaxed);
        let b = drain(&s, 1);
        assert!((b[0] - 0.25).abs() < 0.01, "2.5 s in: half way out: {}", b[0]);
    }

    #[test]
    fn a_fade_takes_its_shape() {
        // 2 s fading out over all of it: a quarter of the way in, each shape
        // is where its curve says, and a late one is still louder than an early one.
        let level = |shape: FadeShape| {
            let s = shared(vec![1.0f32; 44100 * 2 * 2]);
            *s.shape.lock().unwrap() = Shape { start: 0.0, end: 2.0, fade_out: 2.0, fade_out_shape: shape, ..Default::default() };
            let a = drain(&s, 50);
            a[(0.5 * 44100.0) as usize * 2]
        };
        let near = |a: f32, b: f64| (a as f64 - b).abs() < 0.01;
        assert!(near(level(FadeShape::Linear), 0.75), "linear: {}", level(FadeShape::Linear));
        assert!(near(level(FadeShape::Late), (std::f64::consts::FRAC_PI_2 * 0.75).sin()));
        assert!(near(level(FadeShape::VeryEarly), 0.75f64.powi(3)));
        assert!(level(FadeShape::VeryLate) > level(FadeShape::Late) && level(FadeShape::Late) > level(FadeShape::Smooth) && level(FadeShape::Smooth) > level(FadeShape::Early) && level(FadeShape::Early) > level(FadeShape::VeryEarly));
        // A fade in the other way round: early comes up sooner.
        let s = shared(vec![1.0f32; 44100 * 2]);
        *s.shape.lock().unwrap() = Shape { start: 0.0, fade_in: 1.0, fade_in_shape: FadeShape::Early, ..Default::default() };
        let a = drain(&s, 30);
        assert!(near(a[(0.25 * 44100.0) as usize * 2], (std::f64::consts::FRAC_PI_2 * 0.25).sin()));
    }

    #[test]
    fn the_silences_before_and_after_are_heard() {
        // 1 s of signal, cut to end at 0.8 s, with 0.5 s of silence before and 0.3 s after.
        let s = shared(vec![0.5f32; 44100 * 2]);
        let shape = Shape { end: 0.8, pad_start: 0.5, pad_end: 0.3, ..Default::default() };
        *s.shape.lock().unwrap() = shape;
        lead_in(&s, &shape, 0.0, 44100);
        let a = drain(&s, 135); // 1.57 s: the end, and most of the silence after
        let at = |t: f64| a[(t * 44100.0) as usize * 2];
        assert!(at(0.1) == 0.0 && at(0.45) == 0.0, "silence first");
        assert!(at(0.55) == 0.5 && at(1.25) == 0.5, "then the track, from its start");
        assert!(at(1.35) == 0.0 && !s.past_end.load(Ordering::Relaxed), "the silence after the end isn't the end yet");
        assert_eq!(s.frames.load(Ordering::Relaxed), (0.8 * 44100.0) as u64, "the position counts the track, not the silences");
        drain(&s, 30);
        assert!(s.past_end.load(Ordering::Relaxed), "and then it ends");
        // Played from the middle (a seek), there's no silence first.
        let s = shared(vec![0.5f32; 44100 * 2]);
        *s.shape.lock().unwrap() = shape;
        lead_in(&s, &shape, 0.4, 44100);
        assert_eq!(drain(&s, 1)[0], 0.5);
        // The end of the file is an end too: the silence after it, then done.
        let s = shared(vec![0.5f32; 4410 * 2]);
        *s.shape.lock().unwrap() = Shape { pad_end: 0.2, ..Default::default() };
        s.eof.store(true, Ordering::Relaxed);
        drain(&s, 20); // 0.23 s: the file and part of the silence
        assert!(!s.past_end.load(Ordering::Relaxed));
        drain(&s, 10);
        assert!(s.past_end.load(Ordering::Relaxed));
    }
}
