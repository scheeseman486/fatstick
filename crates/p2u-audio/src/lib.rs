//! Playlist2USB's audio engine. All media work goes to one bundled program,
//! an audio-only ffmpeg (decoders for every supported format, LAME, and the
//! loudness, gain and resampling filters); the rest is Rust:
//!
//! * [`probe`]: codec, stream facts and tags, and every MP3 frame header;
//! * [`loudness`]: BS.1770 integrated loudness and true peak;
//! * [`prepare`]: a track's audio for the stick, copied, gain-adjusted in
//!   lossless 1.5 dB steps (mp3rgain) or encoded (LAME), via the [`cache`];
//! * [`tags`] and [`cover`]: what goes in front of (and after) the audio;
//! * [`decode`]: PCM for the player.
//!
//! Source files are only ever opened for reading.

pub mod cache;
pub mod cover;
pub mod decode;
pub mod encode;
pub mod loudness;
pub mod mp3;
pub mod prepare;
pub mod probe;
pub mod tags;
pub mod tools;

pub use cache::Cache;
pub use probe::{probe, Probe, Tags};
pub use tools::Tools;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("ffmpeg: {0}")]
    Tool(String),
    #[error("ffmpeg isn't available: {0}")]
    NoFfmpeg(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, AudioError>;
