//! The window has no title bar from the system: the window's own top bar is the
//! title bar, and the page draws the buttons in each system's way. This says which
//! system it is and where the buttons go. On Linux that's where the desktop puts
//! them: KDE and GNOME-like desktops each have a setting for it.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chrome {
    pub os: &'static str,
    /// The buttons at each end of the bar, as the desktop orders them.
    pub left: Vec<&'static str>,
    pub right: Vec<&'static str>,
}

type Layout = (Vec<&'static str>, Vec<&'static str>);

fn usual() -> Layout {
    (vec![], vec!["minimize", "maximize", "close"])
}

pub fn chrome() -> Chrome {
    if cfg!(target_os = "macos") {
        return Chrome { os: "macos", left: vec!["close", "minimize", "maximize"], right: vec![] };
    }
    if cfg!(windows) {
        let (left, right) = usual();
        return Chrome { os: "windows", left, right };
    }
    let (left, right) = desktop_layout().unwrap_or_else(usual);
    Chrome { os: "linux", left, right }
}

fn desktop_layout() -> Option<Layout> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    if desktop.contains("KDE") {
        let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
        // No kwinrc, or nothing about buttons in it: KDE's own default.
        return parse_kde(&std::fs::read_to_string(config.join("kwinrc")).unwrap_or_default());
    }
    let schema = button_schema(&desktop)?;
    let out = std::process::Command::new("gsettings").args(["get", schema, "button-layout"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_gnome(&String::from_utf8_lossy(&out.stdout))
}

/// Where a desktop keeps its button layout. Asking GNOME's setting elsewhere would be
/// wrong: it's installed on plenty of desktops that ignore it, and its default there
/// is GNOME's, a lone Close, which nobody on those desktops chose.
fn button_schema(desktop: &str) -> Option<&'static str> {
    if desktop.contains("CINNAMON") {
        Some("org.cinnamon.desktop.wm.preferences")
    } else if desktop.contains("MATE") {
        Some("org.mate.Marco.general")
    } else if ["GNOME", "UNITY", "PANTHEON"].iter().any(|d| desktop.contains(d)) {
        Some("org.gnome.desktop.wm.preferences")
    } else {
        None
    }
}

/// KWin's kwinrc: letters for buttons, I minimize, A maximize, X close. Others
/// (the menu, help, keep above...) aren't drawn here.
fn parse_kde(rc: &str) -> Option<Layout> {
    let mut section = "";
    let (mut left, mut right) = ("MS", "HIAX");
    for line in rc.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[org.kde.kdecoration2]" {
            if let Some(v) = line.strip_prefix("ButtonsOnLeft=") {
                left = v;
            } else if let Some(v) = line.strip_prefix("ButtonsOnRight=") {
                right = v;
            }
        }
    }
    let buttons = |s: &str| -> Vec<&'static str> {
        s.chars().filter_map(|c| match c {
            'I' => Some("minimize"),
            'A' => Some("maximize"),
            'X' => Some("close"),
            _ => None,
        }).collect()
    };
    some_buttons((buttons(left), buttons(right)))
}

/// GNOME's setting: 'appmenu:minimize,maximize,close', left of the colon and right.
fn parse_gnome(value: &str) -> Option<Layout> {
    let v = value.trim().trim_matches('\'');
    let (l, r) = v.split_once(':')?;
    let buttons = |s: &str| -> Vec<&'static str> {
        s.split(',').filter_map(|b| match b.trim() {
            "minimize" => Some("minimize"),
            "maximize" => Some("maximize"),
            "close" => Some("close"),
            _ => None,
        }).collect()
    };
    some_buttons((buttons(l), buttons(r)))
}

/// A window with no way of closing it is no layout at all.
fn some_buttons(l: Layout) -> Option<Layout> {
    (l.0.contains(&"close") || l.1.contains(&"close")).then_some(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_asks_a_desktop_that_uses_the_setting() {
        assert_eq!(button_schema("GNOME"), Some("org.gnome.desktop.wm.preferences"));
        assert_eq!(button_schema("UBUNTU:GNOME"), Some("org.gnome.desktop.wm.preferences"));
        assert_eq!(button_schema("X-CINNAMON"), Some("org.cinnamon.desktop.wm.preferences"));
        assert_eq!(button_schema("MATE"), Some("org.mate.Marco.general"));
        for other in ["", "XFCE", "OPENBOX", "SWAY", "HYPRLAND"] {
            assert_eq!(button_schema(other), None, "{other}");
        }
    }

    #[test]
    fn kde_says_where_its_buttons_go() {
        assert_eq!(parse_kde(""), Some(usual()), "KDE's default");
        let rc = "[Windows]\nButtonsOnLeft=X\n\n[org.kde.kdecoration2]\nButtonsOnLeft=XIA\nButtonsOnRight=HS\nlibrary=org.kde.breeze\n";
        assert_eq!(parse_kde(rc), Some((vec!["close", "minimize", "maximize"], vec![])));
        assert_eq!(parse_kde("[org.kde.kdecoration2]\nButtonsOnLeft=X\nButtonsOnRight=IA\n"), Some((vec!["close"], vec!["minimize", "maximize"])));
        assert_eq!(parse_kde("[org.kde.kdecoration2]\nButtonsOnLeft=M\nButtonsOnRight=\n"), None, "no close button");
    }

    #[test]
    fn gnome_says_where_its_buttons_go() {
        assert_eq!(parse_gnome("'appmenu:minimize,maximize,close'\n"), Some(usual()));
        assert_eq!(parse_gnome("'close,minimize,maximize:appmenu'\n"), Some((vec!["close", "minimize", "maximize"], vec![])));
        assert_eq!(parse_gnome("'appmenu:close'"), Some((vec![], vec!["close"])));
        assert_eq!(parse_gnome("':'"), None);
        assert_eq!(parse_gnome("nonsense"), None);
    }

    #[test]
    fn this_system_has_a_close_button() {
        let c = chrome();
        assert!(c.left.contains(&"close") || c.right.contains(&"close"));
        assert!(["linux", "macos", "windows"].contains(&c.os));
    }
}
