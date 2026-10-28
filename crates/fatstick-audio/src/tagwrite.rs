//! Writing tags into files in the user's music library, the one place the app
//! changes a source file, and only when asked to in the library view.
//!
//! Each field of an edit is either left alone (`None`), cleared (`Some("")`)
//! or set. Labels, the user's own free-form tags, live in the Grouping field
//! (ID3v2 TIT1, Vorbis GROUPING, MP4 ©grp, APE Grouping), separated by "; ",
//! where other players show them too. An ID3v2 tag can also have the
//! Grouping in a TXXX:GROUPING frame (ffmpeg writes it there) or in GRP1
//! (Apple's): those that are there get the same labels, so no player sees
//! old ones. The audio, the cover and any other fields are kept. MP3s get
//! ID3v2.3, which every player reads; an existing ID3v1 tag is updated to
//! match.

use std::borrow::Cow;
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::FileType;
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, TextInformationFrame};
use lofty::prelude::*;
use lofty::tag::items::Timestamp;
use lofty::tag::{Tag, TagType};
use lofty::TextEncoding;
use serde::{Deserialize, Serialize};

use crate::{AudioError, Result};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagEdit {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub albumartist: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub composer: Option<String>,
    /// Numbers as text, so "" can clear them.
    #[serde(default)]
    pub year: Option<String>,
    #[serde(default)]
    pub track: Option<String>,
    #[serde(default)]
    pub disc: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        *self == TagEdit::default()
    }
}

/// The labels as one Grouping value, and back.
pub fn join_labels(labels: &[String]) -> String {
    let mut seen = Vec::<String>::new();
    for l in labels {
        let l = l.trim().replace(';', ",");
        if !l.is_empty() && !seen.iter().any(|x| x.eq_ignore_ascii_case(&l)) {
            seen.push(l);
        }
    }
    seen.join("; ")
}
pub fn split_labels(grouping: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for l in grouping.split(';').map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|x| x.eq_ignore_ascii_case(l)) {
            out.push(l.to_string());
        }
    }
    out
}

fn number(field: &str, v: &str, max: u32) -> Result<Option<u32>> {
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let lead: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
    match lead.parse::<u32>() {
        Ok(n) if n > 0 && n <= max => Ok(Some(n)),
        _ => Err(AudioError::Invalid(format!("{field} must be a number from 1 to {max}, not {v:?}"))),
    }
}

fn apply(tag: &mut Tag, e: &TagEdit, numbers: (Option<Option<u32>>, Option<Option<u32>>, Option<Option<u32>>)) {
    let text = |tag: &mut Tag, key: ItemKey, v: &Option<String>| {
        if let Some(v) = v {
            let v = v.trim();
            if v.is_empty() {
                tag.remove_key(key);
            } else {
                tag.insert_text(key, v.to_string());
            }
        }
    };
    text(tag, ItemKey::TrackTitle, &e.title);
    text(tag, ItemKey::TrackArtist, &e.artist);
    text(tag, ItemKey::AlbumTitle, &e.album);
    text(tag, ItemKey::AlbumArtist, &e.albumartist);
    text(tag, ItemKey::Genre, &e.genre);
    text(tag, ItemKey::Composer, &e.composer);
    if let Some(l) = &e.labels {
        text(tag, ItemKey::ContentGroup, &Some(join_labels(l)));
    }
    let (year, track, disc) = numbers;
    match year {
        Some(Some(y)) => tag.set_date(Timestamp { year: y as u16, ..Timestamp::default() }),
        Some(None) => tag.remove_date(),
        None => {}
    }
    match track {
        Some(Some(n)) => tag.set_track(n),
        Some(None) => tag.remove_track(),
        None => {}
    }
    match disc {
        Some(Some(n)) => tag.set_disk(n),
        Some(None) => tag.remove_disk(),
        None => {}
    }
}

/// Write `e` into the file at `path`.
pub fn write_tags(path: &Path, e: &TagEdit) -> Result<()> {
    if e.is_empty() {
        return Ok(());
    }
    let numbers = (
        e.year.as_deref().map(|v| number("The year", v, 9999)).transpose()?,
        e.track.as_deref().map(|v| number("The track number", v, 999)).transpose()?,
        e.disc.as_deref().map(|v| number("The disc number", v, 99)).transpose()?,
    );
    let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "wma" || ext == "asf" {
        return Err(AudioError::Invalid("tags can't be written into WMA files".into()));
    }
    let mut f = lofty::read_from_path(path).map_err(|err| AudioError::Invalid(format!("couldn't read the tags: {err}")))?;
    let primary = f.primary_tag_type();
    if f.tag_mut(primary).is_none() {
        f.insert_tag(Tag::new(primary));
    }
    if let Some(t) = f.tag_mut(primary) {
        apply(t, e, numbers);
    }
    if primary != TagType::Id3v1 {
        if let Some(t) = f.tag_mut(TagType::Id3v1) {
            apply(t, e, numbers);
        }
    }
    let mut opts = WriteOptions::default();
    opts.use_id3v23(true);
    f.save_to_path(path, opts).map_err(|err| AudioError::Invalid(format!("couldn't write the tags: {err}")))?;
    if let Some(l) = &e.labels {
        align_id3_grouping(path, &join_labels(l), opts)?;
    }
    Ok(())
}

/// The file's ID3v2 tag, for the file types that keep one.
fn id3v2_of(path: &Path) -> Option<Id3v2Tag> {
    let mut f = std::fs::File::open(path).ok()?;
    let opts = ParseOptions::new().read_properties(false);
    match FileType::from_path(path)? {
        FileType::Mpeg => lofty::mpeg::MpegFile::read_from(&mut f, opts).ok()?.id3v2().cloned(),
        FileType::Aiff => lofty::iff::aiff::AiffFile::read_from(&mut f, opts).ok()?.id3v2().cloned(),
        FileType::Wav => lofty::iff::wav::WavFile::read_from(&mut f, opts).ok()?.id3v2().cloned(),
        _ => None,
    }
}

/// Every other Grouping frame in an ID3v2 tag (TXXX:GROUPING in any case,
/// GRP1) gets the labels TIT1 got, or goes when there are none.
fn align_id3_grouping(path: &Path, value: &str, opts: WriteOptions) -> Result<()> {
    let Some(mut tag) = id3v2_of(path) else { return Ok(()) };
    let mut changed = false;
    let txxx: Vec<String> = tag
        .iter()
        .filter_map(|fr| match fr {
            Frame::UserText(x) if x.description.trim().eq_ignore_ascii_case("grouping") => Some(x.description.to_string()),
            _ => None,
        })
        .collect();
    for d in txxx {
        tag.remove_user_text(&d);
        if !value.is_empty() {
            tag.insert_user_text(d, value.to_string());
        }
        changed = true;
    }
    let grp1 = FrameId::Valid(Cow::Borrowed("GRP1"));
    if tag.get(&grp1).is_some() {
        tag.remove(&grp1).for_each(drop);
        if !value.is_empty() {
            tag.insert(Frame::Text(TextInformationFrame::new(grp1, TextEncoding::UTF16, value.to_string())));
        }
        changed = true;
    }
    if changed {
        tag.save_to_path(path, opts).map_err(|err| AudioError::Invalid(format!("couldn't write the tags: {err}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        let l = vec!["chill".to_string(), " Kids ".into(), "chill".into(), "a;b".into(), "".into()];
        assert_eq!(join_labels(&l), "chill; Kids; a,b");
        assert_eq!(split_labels("chill; Kids;;a,b ; CHILL"), ["chill", "Kids", "a,b"]);
    }

    #[test]
    fn numbers() {
        assert_eq!(number("x", "3/12", 999).unwrap(), Some(3));
        assert_eq!(number("x", " ", 999).unwrap(), None);
        assert!(number("x", "zero", 999).is_err());
        assert!(number("x", "0", 999).is_err());
    }
}
