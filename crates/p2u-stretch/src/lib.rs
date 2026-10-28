//! Key shifting: audio played higher or lower without getting faster or
//! slower, by Signalsmith Stretch (vendored, MIT; see `vendor/README.md`).
//!
//! The same shifter serves the live preview in the player (`process`, a
//! block at a time, the key changeable as it plays) and the render that goes
//! on a stick (`exact`, a whole track at once, the same length out as in).
//! Audio is interleaved f32; lengths are in frames.

use std::os::raw::{c_float, c_int};

#[repr(C)]
struct Raw {
    _private: [u8; 0],
}

extern "C" {
    fn p2u_stretch_new(channels: c_int, sample_rate: c_float) -> *mut Raw;
    fn p2u_stretch_free(h: *mut Raw);
    fn p2u_stretch_reset(h: *mut Raw);
    fn p2u_stretch_input_latency(h: *mut Raw) -> c_int;
    fn p2u_stretch_output_latency(h: *mut Raw) -> c_int;
    fn p2u_stretch_set(h: *mut Raw, semitones: c_float, keep_formants: c_int);
    fn p2u_stretch_process(h: *mut Raw, input: *mut c_float, input_frames: c_int, output: *mut c_float, output_frames: c_int);
    fn p2u_stretch_exact(h: *mut Raw, input: *mut c_float, input_frames: c_int, output: *mut c_float, output_frames: c_int) -> c_int;
    fn p2u_stretch_seek(h: *mut Raw, input: *mut c_float, input_frames: c_int, rate: f64);
    fn p2u_stretch_flush(h: *mut Raw, output: *mut c_float, output_frames: c_int);
}

/// A key shift: semitones up (or down, negative; cents as fractions), and
/// whether voices keep their natural character (formants) instead of
/// sounding smaller when higher and bigger when lower.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    pub semitones: f32,
    pub keep_formants: bool,
}

pub struct Shifter {
    raw: *mut Raw,
    channels: usize,
    key: Option<Key>,
    scratch: Vec<f32>,
}

// The C++ object is used from one thread at a time (behind a Mutex in the player).
unsafe impl Send for Shifter {}

impl Shifter {
    /// Signalsmith's default preset: blocks of 120 ms, every 30 ms.
    pub fn new(channels: usize, sample_rate: u32) -> Shifter {
        assert!(channels > 0);
        let raw = unsafe { p2u_stretch_new(channels as c_int, sample_rate as c_float) };
        assert!(!raw.is_null());
        Shifter { raw, channels, key: None, scratch: Vec::new() }
    }

    pub fn set_key(&mut self, key: Key) {
        if self.key != Some(key) {
            unsafe { p2u_stretch_set(self.raw, key.semitones, key.keep_formants as c_int) };
            self.key = Some(key);
        }
    }

    /// Forget the audio so far (after a seek).
    pub fn reset(&mut self) {
        unsafe { p2u_stretch_reset(self.raw) }
    }

    /// Frames between audio going in and coming out of `process`.
    pub fn latency(&self) -> usize {
        unsafe { (p2u_stretch_input_latency(self.raw) + p2u_stretch_output_latency(self.raw)).max(0) as usize }
    }

    /// The next block, live: as many frames out as in (later by `latency`).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let n = input.len() / self.channels;
        assert_eq!(output.len(), n * self.channels);
        // The library takes non-const pointers; it doesn't write to the input.
        self.scratch.clear();
        self.scratch.extend_from_slice(input);
        unsafe { p2u_stretch_process(self.raw, self.scratch.as_mut_ptr(), n as c_int, output.as_mut_ptr(), n as c_int) }
    }

    /// A whole stretch of audio at once, the same length out, lined up with
    /// the input (no latency). False if it's too short to shift (under a
    /// quarter of a second or so); the output is then untouched.
    pub fn exact(&mut self, input: &mut [f32], output: &mut [f32]) -> bool {
        let n = input.len() / self.channels;
        assert_eq!(output.len(), n * self.channels);
        if n > i32::MAX as usize {
            return false;
        }
        unsafe { p2u_stretch_exact(self.raw, input.as_mut_ptr(), n as c_int, output.as_mut_ptr(), n as c_int) != 0 }
    }
}

impl Drop for Shifter {
    fn drop(&mut self) {
        unsafe { p2u_stretch_free(self.raw) }
    }
}

/// What `Shifter::exact` does to a whole track, a block at a time in constant
/// memory, for renders of long tracks: the first moments go in as pre-roll,
/// the output is lined up with the input, and the end is flushed. Too short
/// to shift (under half a second or so), it comes out as it went in.
pub struct ExactStream {
    s: Shifter,
    ch: usize,
    in_lat: usize,
    out_lat: usize,
    /// Input held until there's enough to start (and to fall back on).
    start: Vec<f32>,
    started: bool,
    /// Output held until the start can be folded back.
    head: Vec<f32>,
    head_done: bool,
    /// Input frames fed in since the pre-roll.
    fed: usize,
    total: usize,
    block: Vec<f32>,
}

impl ExactStream {
    pub fn new(channels: usize, sample_rate: u32, key: Key) -> ExactStream {
        let mut s = Shifter::new(channels, sample_rate);
        s.set_key(key);
        let in_lat = unsafe { p2u_stretch_input_latency(s.raw).max(0) as usize };
        let out_lat = unsafe { p2u_stretch_output_latency(s.raw).max(0) as usize };
        ExactStream { s, ch: channels, in_lat, out_lat, start: Vec::new(), started: false, head: Vec::new(), head_done: false, fed: 0, total: 0, block: Vec::new() }
    }

    /// Frames needed before starting: the pre-roll, and enough after it to fold the start back.
    fn needed(&self) -> usize {
        self.in_lat + 2 * self.out_lat
    }

    fn feed(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len() / self.ch;
        self.block.resize(input.len(), 0.0);
        let mut block = std::mem::take(&mut self.block);
        self.s.process(input, &mut block);
        self.fed += n;
        if self.head_done {
            out.extend_from_slice(&block);
        } else {
            self.head.extend_from_slice(&block);
            self.fold(out);
        }
        self.block = block;
    }

    /// Once 2 x output latency frames are out: fold the first bit back onto
    /// itself and drop the latency, as `exact` does.
    fn fold(&mut self, out: &mut Vec<f32>) {
        let (ch, lat) = (self.ch, self.out_lat);
        if self.head.len() < 2 * lat * ch {
            return;
        }
        for i in 0..lat {
            for c in 0..ch {
                let v = self.head[(lat - 1 - i) * ch + c];
                self.head[(i + lat) * ch + c] -= v;
            }
        }
        out.extend_from_slice(&self.head[lat * ch..]);
        self.head = Vec::new();
        self.head_done = true;
    }

    fn begin(&mut self, out: &mut Vec<f32>) {
        let ch = self.ch;
        let mut start = std::mem::take(&mut self.start);
        unsafe { p2u_stretch_seek(self.s.raw, start.as_mut_ptr(), self.in_lat as c_int, 1.0) };
        self.started = true;
        let rest = start[self.in_lat * ch..].to_vec();
        for chunk in rest.chunks(4096 * ch) {
            self.feed(chunk, out);
        }
    }

    /// More input; output that's ready is added to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.total += input.len() / self.ch;
        if !self.started {
            self.start.extend_from_slice(input);
            if self.start.len() / self.ch >= self.needed() {
                self.begin(out);
            }
            return;
        }
        for chunk in input.chunks(4096 * self.ch) {
            self.feed(chunk, out);
        }
    }

    /// The end of the input: the rest of the output (the same length in all as went in).
    pub fn finish(mut self, out: &mut Vec<f32>) {
        if !self.started {
            out.extend_from_slice(&self.start);
            return;
        }
        // Zeros as far as the pre-roll reached ahead, then what's still inside the shifter.
        let zeros = vec![0.0f32; self.in_lat * self.ch];
        for chunk in zeros.chunks(4096 * self.ch) {
            self.feed(chunk, out);
        }
        self.fold(out);
        let mut tail = vec![0.0f32; self.out_lat * self.ch];
        unsafe { p2u_stretch_flush(self.s.raw, tail.as_mut_ptr(), self.out_lat as c_int) };
        out.extend_from_slice(&tail);
    }
}

/// Shift a whole stretch of interleaved audio, in place, keeping its length.
pub fn shift(audio: &mut [f32], channels: usize, sample_rate: u32, key: Key) -> bool {
    if key.semitones == 0.0 || audio.is_empty() {
        return true;
    }
    let mut s = Shifter::new(channels, sample_rate);
    s.set_key(key);
    let mut out = vec![0.0f32; audio.len()];
    if !s.exact(audio, &mut out) {
        return false;
    }
    audio.copy_from_slice(&out);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44100;

    fn tone(hz: f32, secs: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        (0..n).flat_map(|i| {
            let x = (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.5;
            [x, x]
        }).collect()
    }

    /// Frequency from upward zero crossings of the left channel, away from the edges.
    fn freq(a: &[f32]) -> f32 {
        let left: Vec<f32> = a.iter().step_by(2).copied().collect();
        let (from, to) = (left.len() / 4, left.len() * 3 / 4);
        let ups = (from..to).filter(|&i| left[i - 1] < 0.0 && left[i] >= 0.0).count();
        ups as f32 * RATE as f32 / (to - from) as f32
    }

    fn rms(a: &[f32]) -> f32 {
        (a.iter().map(|x| x * x).sum::<f32>() / a.len() as f32).sqrt()
    }

    #[test]
    fn up_an_octave_and_down_a_fifth_same_length() {
        for (st, want) in [(12.0, 880.0), (-7.0, 440.0 * 2f32.powf(-7.0 / 12.0)), (2.0, 440.0 * 2f32.powf(2.0 / 12.0))] {
            let mut a = tone(440.0, 3.0);
            let len = a.len();
            assert!(shift(&mut a, 2, RATE, Key { semitones: st, keep_formants: false }));
            assert_eq!(a.len(), len, "same length");
            let f = freq(&a);
            assert!((f - want).abs() / want < 0.01, "{st:+} semitones: {f} Hz, wanted {want}");
            assert!((rms(&a) - rms(&tone(440.0, 3.0))).abs() < 0.08, "about as loud");
        }
    }

    #[test]
    fn live_blocks_follow_key_changes() {
        let input = tone(440.0, 4.0);
        let mut s = Shifter::new(2, RATE);
        s.set_key(Key { semitones: 12.0, keep_formants: true });
        let mut out = vec![0.0; input.len()];
        let block = 512 * 2;
        let half = input.len() / 2 / block * block;
        for (i, (inp, o)) in input.chunks(block).zip(out.chunks_mut(block)).enumerate() {
            if i * block == half {
                s.set_key(Key { semitones: 0.0, keep_formants: true });
            }
            s.process(inp, o);
        }
        let lat = s.latency() * 2;
        let first = &out[lat..half];
        let second = &out[half + lat + 44100..];
        assert!((freq(first) - 880.0).abs() < 15.0, "{}", freq(first));
        assert!((freq(second) - 440.0).abs() < 10.0, "{}", freq(second));
    }

    #[test]
    fn the_stream_matches_the_whole() {
        let key = Key { semitones: -3.0, keep_formants: true };
        let mut input = tone(330.0, 2.5);
        for (i, x) in input.iter_mut().enumerate() {
            *x += ((i * 7919) % 1000) as f32 / 20000.0 - 0.025; // some noise, so it isn't just a sine
        }
        let mut whole = input.clone();
        assert!(shift(&mut whole, 2, RATE, key));
        for step in [333usize, 4096, 44100] {
            let mut st = ExactStream::new(2, RATE, key);
            let mut out = Vec::new();
            for chunk in input.chunks(step * 2) {
                st.push(chunk, &mut out);
            }
            st.finish(&mut out);
            assert_eq!(out.len(), input.len(), "same length, blocks of {step}");
            let worst = out.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-3, "blocks of {step}: off by {worst}");
        }
        // Too short: as it went in.
        let short = tone(440.0, 0.05);
        let mut st = ExactStream::new(2, RATE, key);
        let mut out = Vec::new();
        st.push(&short, &mut out);
        st.finish(&mut out);
        assert_eq!(out, short);
    }

    #[test]
    fn a_reset_keeps_the_key() {
        // After a seek the player resets the shifter: the key it was given stays.
        let input = tone(440.0, 2.0);
        let mut s = Shifter::new(2, RATE);
        s.set_key(Key { semitones: 12.0, keep_formants: false });
        let mut out = vec![0.0; input.len()];
        s.process(&input, &mut out);
        s.reset();
        s.process(&input, &mut out);
        let lat = s.latency() * 2;
        assert!((freq(&out[lat..]) - 880.0).abs() < 15.0, "{}", freq(&out[lat..]));
    }

    #[test]
    fn too_short_is_left_alone() {
        let mut a = tone(440.0, 0.01);
        let before = a.clone();
        assert!(!shift(&mut a, 2, RATE, Key { semitones: 3.0, keep_formants: false }));
        assert_eq!(a, before);
    }
}

