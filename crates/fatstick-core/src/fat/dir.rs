//! Parsing a FAT directory into groups of (long-name entries + short entry).

use serde::Serialize;

use super::DirRef;

pub const ENTRY: usize = 32;
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;
const LAST_LONG_ENTRY: u8 = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EntryKind {
    VolumeLabel,
    Dot,
    DotDot,
    Dir,
    File,
}

/// One logical directory entry: its long-name slots (in on-disk order) followed
/// by the short 8.3 slot.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntryGroup {
    #[serde(skip)]
    pub slots: Vec<[u8; ENTRY]>,
    /// Index of the group's first slot in the directory.
    pub first_slot: usize,
    pub long_name: Option<String>,
    pub short_name: String,
    pub attr: u8,
    pub kind: EntryKind,
    pub first_cluster: u32,
    pub size: u32,
}

impl DirEntryGroup {
    /// The name a user sees: the long name if there is one, else the 8.3 name.
    pub fn name(&self) -> &str {
        self.long_name.as_deref().unwrap_or(&self.short_name)
    }

    pub fn is_hidden(&self) -> bool {
        self.attr & ATTR_HIDDEN != 0
    }
}

/// Result of parsing one directory.
#[derive(Debug, Clone, Serialize)]
pub struct DirListing {
    pub dir: DirRef,
    pub groups: Vec<DirEntryGroup>,
    /// Number of 32-byte slots the directory's clusters can hold.
    pub capacity_slots: usize,
    /// Slot index of the 0x00 end marker (or capacity if the directory is full).
    pub end_slot: usize,
    pub deleted_slots: usize,
    /// Long-name slots that were not followed by a matching short entry.
    pub orphan_lfn_slots: usize,
    /// Slots after the end marker that are not zero (some drivers leave garbage).
    pub nonzero_after_end: usize,
}

impl DirListing {
    pub fn is_consistent(&self) -> bool {
        self.orphan_lfn_slots == 0
    }
}

pub fn sfn_checksum(name11: &[u8]) -> u8 {
    name11.iter().fold(0u8, |sum, &b| ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(b))
}

fn decode_short_name(e: &[u8]) -> String {
    let nt = e[12];
    let mut base: Vec<u8> = e[0..8].to_vec();
    if base[0] == 0x05 {
        base[0] = 0xE5;
    }
    let decode = |bytes: &[u8], lower: bool| -> String {
        let s: String = bytes.iter().map(|&b| cp437(b)).collect::<String>().trim_end().to_string();
        if lower {
            s.to_lowercase()
        } else {
            s
        }
    };
    let b = decode(&base, nt & 0x08 != 0);
    let x = decode(&e[8..11], nt & 0x10 != 0);
    if x.is_empty() {
        b
    } else {
        format!("{b}.{x}")
    }
}

/// Code page 437 is the default OEM code page for 8.3 names; only the upper
/// half differs from ASCII.
fn cp437(b: u8) -> char {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    if b < 0x80 {
        b as char
    } else {
        HIGH.chars().nth((b - 0x80) as usize).unwrap_or('?')
    }
}

fn lfn_units(e: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let ranges = [(1usize, 11usize), (14, 26), (28, 32)];
    ranges
        .into_iter()
        .flat_map(move |(a, b)| (a..b).step_by(2).map(move |i| u16::from_le_bytes([e[i], e[i + 1]])))
}

fn decode_long_name(slots: &[[u8; ENTRY]]) -> String {
    // On disk the slot with the highest ordinal comes first; text starts in ordinal 1.
    let mut units = Vec::with_capacity(slots.len() * 13);
    for s in slots.iter().rev() {
        for u in lfn_units(s) {
            if u == 0x0000 {
                break;
            }
            units.push(u);
        }
    }
    // Stop at the first NUL (handled per slot) and drop 0xFFFF padding.
    units.retain(|&u| u != 0xFFFF);
    String::from_utf16_lossy(&units)
}

pub fn parse_directory(bytes: &[u8], dir: DirRef) -> DirListing {
    let cap = bytes.len() / ENTRY;
    let mut groups = Vec::new();
    let mut pending: Vec<[u8; ENTRY]> = Vec::new();
    let mut pending_start = 0usize;
    // (checksum, next ordinal expected)
    let mut expect: Option<(u8, u8)> = None;
    let mut deleted = 0usize;
    let mut orphans = 0usize;
    let mut end = cap;

    for i in 0..cap {
        let e: [u8; ENTRY] = bytes[i * ENTRY..(i + 1) * ENTRY].try_into().unwrap();
        if e[0] == 0x00 {
            end = i;
            break;
        }
        if e[0] == 0xE5 {
            deleted += 1;
            orphans += pending.len();
            pending.clear();
            expect = None;
            continue;
        }
        let attr = e[11];
        if attr & 0x3F == ATTR_LONG_NAME {
            let ord = e[0];
            if ord & LAST_LONG_ENTRY != 0 {
                orphans += pending.len();
                pending.clear();
                let n = ord & 0x1F;
                if (1..=20).contains(&n) {
                    pending.push(e);
                    pending_start = i;
                    expect = Some((e[13], n - 1));
                } else {
                    orphans += 1;
                    expect = None;
                }
            } else {
                match expect {
                    Some((chk, next)) if next >= 1 && ord & 0x1F == next && e[13] == chk => {
                        pending.push(e);
                        expect = Some((chk, next - 1));
                    }
                    _ => {
                        orphans += pending.len() + 1;
                        pending.clear();
                        expect = None;
                    }
                }
            }
            continue;
        }

        // A short (8.3) entry.
        let sum = sfn_checksum(&e[0..11]);
        let lfn_ok = matches!(expect, Some((chk, 0)) if chk == sum) && !pending.is_empty();
        let (slots, long_name, first_slot) = if lfn_ok {
            let ln = decode_long_name(&pending);
            let mut s = std::mem::take(&mut pending);
            s.push(e);
            (s, Some(ln), pending_start)
        } else {
            orphans += pending.len();
            pending.clear();
            (vec![e], None, i)
        };
        expect = None;

        let kind = if attr & ATTR_VOLUME_ID != 0 && attr & ATTR_DIRECTORY == 0 {
            EntryKind::VolumeLabel
        } else if &e[0..11] == b".          " {
            EntryKind::Dot
        } else if &e[0..11] == b"..         " {
            EntryKind::DotDot
        } else if attr & ATTR_DIRECTORY != 0 {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let hi = u16::from_le_bytes([e[20], e[21]]) as u32;
        let lo = u16::from_le_bytes([e[26], e[27]]) as u32;
        groups.push(DirEntryGroup {
            slots,
            first_slot,
            long_name,
            short_name: decode_short_name(&e),
            attr,
            kind,
            first_cluster: (hi << 16) | lo,
            size: u32::from_le_bytes([e[28], e[29], e[30], e[31]]),
        });
    }
    orphans += pending.len();
    let nonzero_after_end = (end + 1..cap)
        .filter(|&i| bytes[i * ENTRY..(i + 1) * ENTRY].iter().any(|&b| b != 0))
        .count();

    DirListing {
        dir,
        groups,
        capacity_slots: cap,
        end_slot: end,
        deleted_slots: deleted,
        orphan_lfn_slots: orphans,
        nonzero_after_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_spec_example() {
        // Checksum of "FOO     BAR" computed with the spec's algorithm.
        let name = b"FOO     BAR";
        let mut sum: u8 = 0;
        for &b in name {
            sum = (if sum & 1 != 0 { 0x80u8 } else { 0 }).wrapping_add(sum >> 1).wrapping_add(b);
        }
        assert_eq!(sfn_checksum(name), sum);
    }

    #[test]
    fn short_name_case_flags() {
        let mut e = [0u8; 32];
        e[0..11].copy_from_slice(b"SONG    MP3");
        e[12] = 0x18;
        assert_eq!(decode_short_name(&e), "song.mp3");
    }
}
