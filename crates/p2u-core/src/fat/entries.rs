//! Building directory entries: 8.3 short names, long-name (LFN) slots, FAT
//! timestamps, and comparing an on-disk order with a playlist.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::dir::{sfn_checksum, ENTRY};
use super::{FatError, Result};

pub const ATTR_VOLUME_ID: u8 = 0x08;
pub const ATTR_DIR: u8 = 0x10;
pub const ATTR_ARCHIVE: u8 = 0x20;

const SFN_ALLOWED: &[u8] = b"!#$%&'()-@^_`{}~";
const FORBIDDEN: &[char] = &['"', '*', '/', ':', '<', '>', '?', '\\', '|'];

/// Case-insensitive, Unicode-normalized form used to compare names.
pub fn normalize_name(s: &str) -> String {
    s.nfc().collect::<String>().to_uppercase()
}

/// Unicode-normalize a name the way it is written (NFC), so the order list and
/// the written entries agree.
pub fn written_name(name: &str) -> String {
    name.nfc().collect()
}

fn sfn_char(c: char) -> Option<u8> {
    if c.is_ascii_uppercase() || c.is_ascii_digit() || (c.is_ascii() && SFN_ALLOWED.contains(&(c as u8))) {
        Some(c as u8)
    } else {
        None
    }
}

/// The 8.3 name for `long`, unique among `taken`, and whether a long-name
/// entry is needed (Microsoft's basis-name algorithm with a numeric tail).
pub fn short_name(long: &str, taken: &HashSet<[u8; 11]>) -> ([u8; 11], bool) {
    let upper = long.to_uppercase();
    // A name that already is a plain upper-case 8.3 name needs no long entry.
    let (b, e) = match upper.rfind('.') {
        Some(i) if i > 0 => (&upper[..i], &upper[i + 1..]),
        _ => (upper.as_str(), ""),
    };
    let exact = upper == long
        && !b.is_empty()
        && b.len() <= 8
        && e.len() <= 3
        && !b.contains('.')
        && b.chars().all(|c| sfn_char(c).is_some())
        && e.chars().all(|c| sfn_char(c).is_some());
    if exact {
        let mut n = [b' '; 11];
        n[..b.len()].copy_from_slice(b.as_bytes());
        n[8..8 + e.len()].copy_from_slice(e.as_bytes());
        if !taken.contains(&n) {
            return (n, false);
        }
    }
    // Basis name: drop spaces and periods (except the last one), map the rest.
    let trimmed = upper.trim_start_matches('.');
    let (base_src, ext_src) = match trimmed.rfind('.') {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => (trimmed, ""),
    };
    let map = |s: &str| -> Vec<u8> {
        s.chars().filter(|&c| c != ' ' && c != '.').map(|c| sfn_char(c).unwrap_or(b'_')).collect()
    };
    let mut base = map(base_src);
    let mut ext = map(ext_src);
    ext.truncate(3);
    if base.is_empty() {
        base = b"_".to_vec();
    }
    for n in 1u32..1_000_000 {
        let tail = format!("~{n}");
        let keep = base.len().min(8 - tail.len());
        let mut name = [b' '; 11];
        name[..keep].copy_from_slice(&base[..keep]);
        name[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        name[8..8 + ext.len()].copy_from_slice(&ext);
        if !taken.contains(&name) {
            return (name, true);
        }
    }
    unreachable!("a million short-name collisions")
}

/// The volume label as FAT stores it: up to 11 upper-case 8.3 characters
/// (spaces allowed), padded with spaces.
pub fn volume_label(wanted: &str) -> [u8; 11] {
    let ascii = deunicode::deunicode(wanted).to_uppercase();
    let mut out = [b' '; 11];
    let mut n = 0;
    for c in ascii.chars() {
        if n == 11 {
            break;
        }
        let b = if c == ' ' { Some(b' ') } else { sfn_char(c) };
        if let Some(b) = b {
            if n == 0 && b == b' ' {
                continue;
            }
            out[n] = b;
            n += 1;
        }
    }
    if out.iter().all(|&b| b == b' ') {
        out.copy_from_slice(b"PLAYLIST   ");
    }
    out
}

/// FAT date and time (2-second resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FatTimestamp {
    pub date: u16,
    pub time: u16,
}

impl FatTimestamp {
    /// From seconds since the Unix epoch (written as UTC; FAT has no zone).
    pub fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Howard Hinnant's civil-from-days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        let y = y.clamp(1980, 2107);
        let (h, mi, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
        FatTimestamp {
            date: (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16,
            time: ((h as u16) << 11) | ((mi as u16) << 5) | (s / 2) as u16,
        }
    }
}

fn lfn_entries(long: &str, sfn: &[u8; 11]) -> Vec<[u8; ENTRY]> {
    const POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let units: Vec<u16> = long.encode_utf16().collect();
    let n = units.len().div_ceil(13);
    let chk = sfn_checksum(sfn);
    let mut out = Vec::with_capacity(n);
    for k in (1..=n).rev() {
        let mut e = [0u8; ENTRY];
        e[0] = k as u8 | if k == n { 0x40 } else { 0 };
        e[11] = 0x0F;
        e[13] = chk;
        for (i, p) in POS.iter().enumerate() {
            let idx = (k - 1) * 13 + i;
            let u = match idx.cmp(&units.len()) {
                std::cmp::Ordering::Less => units[idx],
                std::cmp::Ordering::Equal => 0x0000,
                std::cmp::Ordering::Greater => 0xFFFF,
            };
            e[*p..*p + 2].copy_from_slice(&u.to_le_bytes());
        }
        out.push(e);
    }
    out
}

/// One 8.3 entry. Dates go in creation, access and write fields alike.
pub fn sfn_entry(name: &[u8; 11], attr: u8, cluster: u32, size: u32, ts: FatTimestamp) -> [u8; ENTRY] {
    let mut e = [0u8; ENTRY];
    e[0..11].copy_from_slice(name);
    if e[0] == 0xE5 {
        e[0] = 0x05;
    }
    e[11] = attr;
    e[14..16].copy_from_slice(&ts.time.to_le_bytes());
    e[16..18].copy_from_slice(&ts.date.to_le_bytes());
    e[18..20].copy_from_slice(&ts.date.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&ts.time.to_le_bytes());
    e[24..26].copy_from_slice(&ts.date.to_le_bytes());
    e[26..28].copy_from_slice(&((cluster & 0xFFFF) as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// The long-name entries (if any) followed by the 8.3 entry for one name.
/// Returns the slots and the short name used.
pub fn entry_group(
    long: &str,
    taken: &mut HashSet<[u8; 11]>,
    attr: u8,
    cluster: u32,
    size: u32,
    ts: FatTimestamp,
) -> (Vec<[u8; ENTRY]>, [u8; 11]) {
    let (sfn, needs_lfn) = short_name(long, taken);
    taken.insert(sfn);
    let mut g = if needs_lfn { lfn_entries(long, &sfn) } else { Vec::new() };
    g.push(sfn_entry(&sfn, attr, cluster, size, ts));
    (g, sfn)
}

/// Whether `name` can be written as a FAT long name.
pub fn validate_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || name.ends_with(' ')
        || name.chars().any(|c| FORBIDDEN.contains(&c) || (c as u32) < 0x20)
        || name.encode_utf16().count() > 255;
    if bad {
        return Err(FatError::Invalid(format!("{name:?} is not a valid FAT long name")));
    }
    Ok(())
}

/// How a folder's actual order compares with the order wanted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCheck {
    pub in_order: bool,
    pub expected: usize,
    pub found: usize,
    pub missing: Vec<String>,
    /// Files present in the folder but not in the playlist (a player would play them too).
    pub extra: Vec<String>,
    /// First playlist position (0-based) whose file is not where it should be.
    pub first_mismatch: Option<usize>,
    pub actual: Vec<String>,
}

/// Compare the order of `actual` names (as stored in the directory) with `expected`.
pub fn check_order(actual: &[String], expected: &[String]) -> OrderCheck {
    let want: HashMap<String, usize> = expected.iter().enumerate().map(|(i, n)| (normalize_name(n), i)).collect();
    let mut seen_positions = Vec::new();
    let mut extra = Vec::new();
    for n in actual {
        match want.get(&normalize_name(n)) {
            Some(&i) => seen_positions.push(i),
            None => extra.push(n.clone()),
        }
    }
    let present: HashSet<usize> = seen_positions.iter().copied().collect();
    let missing: Vec<String> =
        expected.iter().enumerate().filter(|(i, _)| !present.contains(i)).map(|(_, n)| n.clone()).collect();
    let expected_present: Vec<usize> = (0..expected.len()).filter(|i| present.contains(i)).collect();
    let first_mismatch =
        seen_positions.iter().zip(expected_present.iter()).position(|(a, b)| a != b).map(|k| expected_present[k]);
    OrderCheck {
        in_order: first_mismatch.is_none() && missing.is_empty() && seen_positions.len() == expected_present.len(),
        expected: expected.len(),
        found: seen_positions.len(),
        missing,
        extra,
        first_mismatch,
        actual: actual.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fat::dir::parse_directory;
    use crate::fat::DirRef;

    #[test]
    fn short_names_follow_the_basis_rules() {
        let mut taken = HashSet::new();
        let (a, lfn) = short_name("README.TXT", &taken);
        assert_eq!(&a, b"README  TXT");
        assert!(!lfn);
        let (b, lfn) = short_name("01 - Night Ferry.mp3", &taken);
        assert_eq!(&b, b"01-NIG~1MP3");
        assert!(lfn);
        taken.insert(b);
        let (c, _) = short_name("01 - Night Train.mp3", &taken);
        assert_eq!(&c, b"01-NIG~2MP3");
        let (d, _) = short_name("Café+Bar.flac", &HashSet::new());
        assert_eq!(&d, b"CAF__B~1FLA");
    }

    #[test]
    fn lfn_round_trips_through_the_parser() {
        let mut taken = HashSet::new();
        let name = "12 - Last Exit Before the Bridge (Live).mp3";
        let (g, _) = entry_group(name, &mut taken, ATTR_ARCHIVE, 1234, 5678, FatTimestamp::from_unix(1_700_000_000));
        let mut bytes = Vec::new();
        for e in &g {
            bytes.extend_from_slice(e);
        }
        bytes.extend_from_slice(&[0u8; 32]);
        let l = parse_directory(&bytes, DirRef::Root);
        assert_eq!(l.orphan_lfn_slots, 0);
        assert_eq!(l.groups.len(), 1);
        assert_eq!(l.groups[0].name(), name);
        assert_eq!(l.groups[0].first_cluster, 1234);
        assert_eq!(l.groups[0].size, 5678);
    }

    #[test]
    fn labels() {
        assert_eq!(&volume_label("Road Trip"), b"ROAD TRIP  ");
        assert_eq!(&volume_label("Café: Mix #2 (long name)"), b"CAFE MIX #2");
        assert_eq!(&volume_label("  "), b"PLAYLIST   ");
    }

    #[test]
    fn fat_dates() {
        // 2026-10-27 12:34:56 UTC
        let t = FatTimestamp::from_unix(1_793_104_496);
        assert_eq!(t.date >> 9, 2026 - 1980);
        assert_eq!((t.date >> 5) & 0xF, 10);
        assert_eq!(t.date & 0x1F, 27);
        assert_eq!(t.time >> 11, 12);
        assert_eq!((t.time >> 5) & 0x3F, 34);
        assert_eq!((t.time & 0x1F) * 2, 56);
    }
}
