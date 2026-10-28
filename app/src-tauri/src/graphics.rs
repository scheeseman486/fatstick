//! A workaround for WebKitGTK, the web view behind the window on Linux.
//!
//! Under Wayland with NVIDIA's driver, WebKitGTK can die as the window opens
//! with "Error 71 (Protocol error) dispatching to Wayland display". Turning off
//! the driver's explicit sync fixes it without slowing anything down (Tauri's
//! "Linux Graphics Issues" page). It is set only on Wayland with the NVIDIA
//! driver loaded, never over a value set by the user, and only NVIDIA's own
//! libraries read it. If the window still won't open, these can be tried by
//! hand, in order: `WEBKIT_DISABLE_DMABUF_RENDERER=1`, then `GDK_BACKEND=x11`.

pub const NV_EXPLICIT_SYNC: &str = "__NV_DISABLE_EXPLICIT_SYNC";

/// Whether to turn off NVIDIA's explicit sync, given the environment and
/// whether the NVIDIA kernel driver is loaded.
pub fn needs_nvidia_fix(var: impl Fn(&str) -> Option<String>, nvidia_loaded: bool) -> bool {
    if !nvidia_loaded || var(NV_EXPLICIT_SYNC).is_some() {
        return false;
    }
    // GDK_BACKEND lists backends in order of preference; x11 first means no Wayland.
    if var("GDK_BACKEND").is_some_and(|b| b.split(',').next().map(str::trim) == Some("x11")) {
        return false;
    }
    var("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty()) || var("XDG_SESSION_TYPE").as_deref() == Some("wayland")
}

/// Call first thing in `main`, before any thread starts: changing the
/// environment isn't safe once other threads may read it. The web view's
/// helper processes, started later, inherit it.
pub fn apply() {
    let nvidia = std::path::Path::new("/proc/driver/nvidia/version").exists() || std::path::Path::new("/sys/module/nvidia_drm").exists();
    if needs_nvidia_fix(|k| std::env::var(k).ok(), nvidia) {
        std::env::set_var(NV_EXPLICIT_SYNC, "1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn only_on_wayland_with_nvidia_and_never_over_the_users_choice() {
        assert!(needs_nvidia_fix(env(&[("WAYLAND_DISPLAY", "wayland-0")]), true));
        assert!(needs_nvidia_fix(env(&[("XDG_SESSION_TYPE", "wayland")]), true));
        assert!(!needs_nvidia_fix(env(&[("WAYLAND_DISPLAY", "wayland-0")]), false), "no NVIDIA driver");
        assert!(!needs_nvidia_fix(env(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")]), true), "X11 session");
        assert!(!needs_nvidia_fix(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("GDK_BACKEND", "x11")]), true), "forced to X");
        assert!(needs_nvidia_fix(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("GDK_BACKEND", "wayland,x11")]), true));
        assert!(!needs_nvidia_fix(env(&[("WAYLAND_DISPLAY", "wayland-0"), (NV_EXPLICIT_SYNC, "0")]), true), "the user's own value");
    }
}
