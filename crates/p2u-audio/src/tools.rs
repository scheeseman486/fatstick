//! Finding and running the bundled ffmpeg and ffprobe.
//!
//! Looked for in this order: the folder in `P2U_FFMPEG_DIR`; next to the
//! running program (where the app bundles them); `app/src-tauri/binaries`
//! with the target-triple suffix Tauri uses (development builds); the PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::{AudioError, Result};

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

const EXE: &str = std::env::consts::EXE_SUFFIX;

fn target_triple() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(windows, target_env = "gnu")) {
        "x86_64-pc-windows-gnu"
    } else if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    }
}

fn pair_in(dir: &Path, suffix: &str) -> Option<Tools> {
    let ffmpeg = dir.join(format!("ffmpeg{suffix}{EXE}"));
    let ffprobe = dir.join(format!("ffprobe{suffix}{EXE}"));
    (ffmpeg.is_file() && ffprobe.is_file()).then_some(Tools { ffmpeg, ffprobe })
}

impl Tools {
    pub fn locate() -> Result<Tools> {
        if let Some(d) = std::env::var_os("P2U_FFMPEG_DIR") {
            return pair_in(Path::new(&d), "").ok_or_else(|| AudioError::NoFfmpeg(format!("not in P2U_FFMPEG_DIR ({})", Path::new(&d).display())));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                if let Some(t) = pair_in(dir, "") {
                    return Ok(t);
                }
                let suffix = format!("-{}", target_triple());
                for up in dir.ancestors().take(6) {
                    for sub in ["app/src-tauri/binaries", "src-tauri/binaries", "binaries"] {
                        if let Some(t) = pair_in(&up.join(sub), &suffix) {
                            return Ok(t);
                        }
                    }
                }
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                if let Some(t) = pair_in(&dir, "") {
                    return Ok(t);
                }
            }
        }
        Err(AudioError::NoFfmpeg("ffmpeg and ffprobe were not found next to the app or on the PATH".into()))
    }

    pub fn ffmpeg(&self) -> Command {
        let mut c = quiet(&self.ffmpeg);
        c.args(["-nostdin", "-hide_banner"]);
        c
    }

    pub fn ffprobe(&self) -> Command {
        let mut c = quiet(&self.ffprobe);
        c.arg("-hide_banner");
        c
    }

    /// "ffmpeg version n9.0.2 ..." first line, for the About box.
    pub fn version(&self) -> String {
        run(self.ffmpeg().arg("-version"))
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_string))
            .unwrap_or_default()
    }
}

/// A command that doesn't open a console window on Windows.
fn quiet(p: &Path) -> Command {
    let mut c = Command::new(p);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Run to completion; a non-zero exit becomes an error carrying stderr's tail.
pub fn run(c: &mut Command) -> Result<Output> {
    let o = c.stdout(Stdio::piped()).stderr(Stdio::piped()).output()?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = tail[tail.len().saturating_sub(3)..].join(" / ");
        return Err(AudioError::Tool(if tail.is_empty() { format!("exited with {}", o.status) } else { tail }));
    }
    Ok(o)
}
