//! The tags on each file written to the stick: ID3v2.3 by default (older
//! players read it best) or ID3v2.4, plus an optional ID3v1.1 at the end.
//! Only these fields, and never REPLAYGAIN_* or mp3gain undo data: the gain is
//! already in the audio, and a player that also obeyed a tag would apply it
//! twice.

use id3::frame::{Picture, PictureType};
use id3::{Tag, TagLike, Version};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagSpec {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub albumartist: String,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub genre: String,
    #[serde(skip)]
    pub cover_jpeg: Option<Vec<u8>>,
}

/// The ID3v2 tag's bytes. `v24` picks ID3v2.4 over ID3v2.3.
pub fn id3v2(spec: &TagSpec, v24: bool) -> Vec<u8> {
    let mut t = Tag::new();
    if !spec.title.is_empty() {
        t.set_title(&spec.title);
    }
    if !spec.artist.is_empty() {
        t.set_artist(&spec.artist);
    }
    if !spec.album.is_empty() {
        t.set_album(&spec.album);
    }
    if !spec.albumartist.is_empty() {
        t.set_album_artist(&spec.albumartist);
    }
    if let Some(y) = spec.year {
        if v24 {
            t.set_date_recorded(id3::Timestamp { year: y as i32, month: None, day: None, hour: None, minute: None, second: None });
        } else {
            t.set_year(y as i32);
        }
    }
    if let Some(n) = spec.track {
        t.set_track(n);
        if let Some(total) = spec.track_total {
            t.set_total_tracks(total);
        }
    }
    if let Some(d) = spec.disc {
        t.set_disc(d);
    }
    if !spec.genre.is_empty() {
        t.set_genre(&spec.genre);
    }
    if let Some(jpeg) = &spec.cover_jpeg {
        t.add_frame(Picture { mime_type: "image/jpeg".into(), picture_type: PictureType::CoverFront, description: String::new(), data: jpeg.clone() });
    }
    let mut out = Vec::new();
    if t.frames().next().is_none() {
        return out;
    }
    t.write_to(&mut out, if v24 { Version::Id3v24 } else { Version::Id3v23 }).expect("writing a tag to memory");
    out
}

/// Latin-1 where it fits, otherwise the closest ASCII.
fn latin1(s: &str, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    for c in s.chars() {
        if (c as u32) < 0x100 && c >= ' ' {
            out.push(c as u8);
        } else {
            out.extend(deunicode::deunicode_char(c).unwrap_or("_").bytes().filter(|b| (0x20..0x7F).contains(b)));
        }
        if out.len() >= n {
            break;
        }
    }
    out.truncate(n);
    out.resize(n, 0);
    out
}

/// A 128-byte ID3v1.1 tag, for very old displays.
pub fn id3v1(spec: &TagSpec) -> [u8; 128] {
    let mut t = [0u8; 128];
    t[0..3].copy_from_slice(b"TAG");
    t[3..33].copy_from_slice(&latin1(&spec.title, 30));
    t[33..63].copy_from_slice(&latin1(&spec.artist, 30));
    t[63..93].copy_from_slice(&latin1(&spec.album, 30));
    if let Some(y) = spec.year {
        t[93..97].copy_from_slice(format!("{:04}", y.min(9999)).as_bytes());
    }
    t[125] = 0;
    t[126] = spec.track.unwrap_or(0).min(255) as u8;
    t[127] = 255; // genre: none
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TagSpec {
        TagSpec {
            title: "Hoppípolla".into(),
            artist: "Sigur Rós".into(),
            album: "Takk...".into(),
            albumartist: "Sigur Rós".into(),
            year: Some(2005),
            track: Some(7),
            track_total: Some(21),
            disc: None,
            genre: "Post-rock".into(),
            cover_jpeg: Some(vec![0xFF, 0xD8, 0xFF, 0xD9]),
        }
    }

    #[test]
    fn id3v23_round_trips() {
        let b = id3v2(&spec(), false);
        assert_eq!(&b[0..4], b"ID3\x03");
        let t = Tag::read_from2(std::io::Cursor::new(&b)).unwrap();
        assert_eq!(t.version(), Version::Id3v23);
        assert_eq!(t.title(), Some("Hoppípolla"));
        assert_eq!(t.artist(), Some("Sigur Rós"));
        assert_eq!((t.track(), t.total_tracks(), t.year()), (Some(7), Some(21), Some(2005)));
        assert_eq!(t.pictures().next().unwrap().data, vec![0xFF, 0xD8, 0xFF, 0xD9]);
        assert!(t.frames().all(|f| f.id() != "TXXX"), "no REPLAYGAIN or other user frames");
    }

    #[test]
    fn id3v24_and_v1() {
        let b = id3v2(&spec(), true);
        assert_eq!(&b[0..4], b"ID3\x04");
        let v1 = id3v1(&spec());
        assert_eq!(&v1[0..3], b"TAG");
        assert_eq!(&v1[3..13], b"Hopp\xEDpolla");
        assert_eq!((v1[125], v1[126], v1[127]), (0, 7, 255));
        assert!(id3v2(&TagSpec::default(), false).is_empty());
    }
}
