//! Devices left out: a card reader that's always plugged in, a backup drive.
//! They aren't listed to copy to, nothing on them is read (not even the note
//! a sync leaves), and a sync refuses them. The list is kept in the library's
//! settings and read from there each time, so it holds across restarts.
//!
//! A device is known by its own serial number and size when the system gives
//! a serial (a card reader has one serial for every card, so the size tells
//! cards apart), else by its name and size; neither changes whatever is
//! written to it.

use fatstick_library::Library;
use serde::{Deserialize, Serialize};

pub const SETTING: &str = "excludedDevices";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Excluded {
    /// The device's own serial number, if the system gives one.
    pub serial: String,
    pub name: String,
    pub size_bytes: u64,
    /// A disk image standing in for a stick (tests): its id.
    pub image: String,
}

/// What a device is known by, from the device list.
#[derive(Debug, Clone, Default)]
pub struct Ident {
    pub serial: String,
    pub name: String,
    pub size_bytes: u64,
    pub image: Option<String>,
}

impl Excluded {
    pub fn of(d: &Ident) -> Excluded {
        Excluded { serial: d.serial.clone(), name: d.name.clone(), size_bytes: d.size_bytes, image: d.image.clone().unwrap_or_default() }
    }

    pub fn matches(&self, d: &Ident) -> bool {
        if !self.image.is_empty() || d.image.is_some() {
            return d.image.as_deref() == Some(self.image.as_str());
        }
        if !self.serial.is_empty() && !d.serial.is_empty() {
            return self.serial == d.serial && self.size_bytes == d.size_bytes;
        }
        !self.name.is_empty() && self.name == d.name && self.size_bytes == d.size_bytes
    }

    /// An entry saved by an earlier version: `{ key: "sn:…" | "ms:name|size" | "img:…", name, sizeBytes }`.
    fn from_old(v: &serde_json::Value) -> Option<Excluded> {
        let key = v.get("key")?.as_str()?;
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let size_bytes = v.get("sizeBytes").and_then(|x| x.as_u64()).unwrap_or(0);
        if let Some(id) = key.strip_prefix("img:") {
            return Some(Excluded { image: id.to_string(), name, size_bytes, ..Default::default() });
        }
        if let Some(sn) = key.strip_prefix("sn:") {
            return Some(Excluded { serial: sn.to_string(), name, size_bytes, ..Default::default() });
        }
        (key.starts_with("ms:") && !name.is_empty()).then_some(Excluded { name, size_bytes, ..Default::default() })
    }
}

/// The list as saved. Entries it can't read are skipped, never the whole list.
pub fn load(lib: &Library) -> Vec<Excluded> {
    let Ok(Some(text)) = lib.setting(SETTING) else { return vec![] };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    items
        .iter()
        .filter_map(|v| if v.get("key").is_some() { Excluded::from_old(v) } else { serde_json::from_value(v.clone()).ok() })
        .filter(|x| !(x.serial.is_empty() && x.name.is_empty() && x.image.is_empty()))
        .collect()
}

pub fn save(lib: &Library, list: &[Excluded]) -> Result<(), String> {
    let text = serde_json::to_string(list).map_err(|e| e.to_string())?;
    lib.set_setting(SETTING, &text).map_err(|e| e.to_string())?;
    // Read it back: a list that didn't save must not look as if it had.
    if load(lib) != list {
        return Err("the list of excluded devices didn't save".into());
    }
    Ok(())
}

pub fn is_excluded(list: &[Excluded], d: &Ident) -> bool {
    list.iter().any(|x| x.matches(d))
}

/// Entries saved without a serial (before the system gave one, or by an
/// earlier version) take the serial of a device they match by name and size,
/// so from then on they're told apart from a twin of the same make and size.
/// True if anything changed.
pub fn learn_serials(list: &mut [Excluded], devices: &[Ident]) -> bool {
    let mut changed = false;
    for x in list.iter_mut().filter(|x| x.serial.is_empty() && x.image.is_empty()) {
        let mut found = devices.iter().filter(|d| d.image.is_none() && !d.serial.is_empty() && x.matches(d));
        if let (Some(d), None) = (found.next(), found.next()) {
            x.serial = d.serial.clone();
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(serial: &str, name: &str, size: u64) -> Ident {
        Ident { serial: serial.into(), name: name.into(), size_bytes: size, image: None }
    }

    #[test]
    fn devices_are_known_by_serial_and_size_or_by_name_and_size() {
        let stick = dev("ABC123", "SanDisk Cruzer", 32_000_000_000);
        let ex = Excluded::of(&stick);
        assert!(ex.matches(&stick));
        // Its twin: the same make and size, another serial.
        assert!(!ex.matches(&dev("XYZ789", "SanDisk Cruzer", 32_000_000_000)));
        // Another card in the same reader: the same serial, another size.
        assert!(!ex.matches(&dev("ABC123", "SanDisk Cruzer", 64_000_000_000)));
        // Read without its serial this time: name and size.
        assert!(ex.matches(&dev("", "SanDisk Cruzer", 32_000_000_000)));
        // No serial at all (macOS before this version): name and size.
        let plain = Excluded { name: "Generic SD/MMC".into(), size_bytes: 8_000_000_000, ..Default::default() };
        assert!(plain.matches(&dev("", "Generic SD/MMC", 8_000_000_000)) && plain.matches(&dev("R1", "Generic SD/MMC", 8_000_000_000)));
        assert!(!plain.matches(&dev("", "Generic SD/MMC", 16_000_000_000)));
        // Images by id only.
        let img = Ident { image: Some("image:/a.img".into()), ..dev("", "Disk image a.img", 64) };
        assert!(Excluded::of(&img).matches(&img) && !Excluded::of(&img).matches(&Ident { image: Some("image:/b.img".into()), ..img.clone() }));
        assert!(!plain.matches(&img));
    }

    #[test]
    fn the_list_saves_reads_back_and_takes_the_old_form() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::open(&d.path().join("lib.db")).unwrap();
        assert!(load(&lib).is_empty());
        let list = vec![Excluded::of(&dev("ABC123", "SanDisk Cruzer", 32)), Excluded::of(&dev("", "Card Reader", 8))];
        save(&lib, &list).unwrap();
        assert_eq!(load(&Library::open(&d.path().join("lib.db")).unwrap()), list, "kept across a restart");
        // What the window used to save (and never read back).
        lib.set_setting(SETTING, r#"[{"key":"ms:Generic SD/MMC|8000","name":"Generic SD/MMC","sizeBytes":8000},{"key":"sn:QQQ","name":"Kingston","sizeBytes":16},{"key":"img:image:/x.img","name":"Disk image x.img","sizeBytes":64},{"junk":1},"nope"]"#).unwrap();
        let old = load(&lib);
        assert_eq!(old.len(), 3, "{old:?}");
        assert!(old[0].matches(&dev("", "Generic SD/MMC", 8000)));
        assert!(old[1].matches(&dev("QQQ", "Kingston", 16)));
        assert!(old[2].matches(&Ident { image: Some("image:/x.img".into()), ..Default::default() }));
    }

    #[test]
    fn an_entry_without_a_serial_learns_it() {
        let mut list = vec![Excluded { name: "Cruzer".into(), size_bytes: 32, ..Default::default() }];
        assert!(learn_serials(&mut list, &[dev("S1", "Cruzer", 32)]));
        assert_eq!(list[0].serial, "S1");
        assert!(!list[0].matches(&dev("S2", "Cruzer", 32)), "now its twin isn't left out");
        // Two twins plugged in: which one was meant isn't known, so it stays as it is.
        let mut list = vec![Excluded { name: "Cruzer".into(), size_bytes: 32, ..Default::default() }];
        assert!(!learn_serials(&mut list, &[dev("S1", "Cruzer", 32), dev("S2", "Cruzer", 32)]));
        assert!(list[0].serial.is_empty());
    }
}
