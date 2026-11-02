//! What a song is called. YouTube knows a track, artist and album for the music
//! it recognizes; otherwise there's a video's title, usually "Artist - Title
//! (Official Video)", and a channel. Tags come from those, cleaned up, and the
//! file's name from the tags. The upload date isn't the song's year, so it isn't
//! used as one.

use fatstick_audio::tagwrite::TagEdit;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<u32>,
}

/// Brackets that say what kind of upload this is, not which song.
fn is_noise(inside: &str) -> bool {
    let s = inside.trim().to_lowercase();
    const ANYWHERE: &[&str] = &["official", "lyric", "visuali", "music video", "full hd", "high quality", "full album"];
    const WHOLE: &[&str] = &["audio", "video", "hd", "hq", "4k", "8k", "mv", "m/v", "clip", "explicit", "free download", "out now", "new"];
    s.is_empty() || ANYWHERE.iter().any(|x| s.contains(x)) || WHOLE.contains(&s.as_str())
}

/// A title without its "(Official Video)" and the like; "(Live)" and "(Remix)" stay.
pub fn clean_title(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    while let Some(at) = rest.find(['(', '[']) {
        let close = if rest[at..].starts_with('(') { ')' } else { ']' };
        let Some(end) = rest[at..].find(close) else { break };
        let inside = &rest[at + 1..at + end];
        out.push_str(&rest[..at]);
        if !is_noise(inside) {
            out.push_str(&rest[at..=at + end]);
        }
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    // "Song | Official Video" likewise.
    let out = match out.rsplit_once('|') {
        Some((head, tail)) if is_noise(tail) && !head.trim().is_empty() => head.to_string(),
        _ => out,
    };
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "Artist - Title", with whichever dash the uploader used.
fn split_artist(title: &str) -> Option<(String, String)> {
    for sep in [" - ", " – ", " — ", " ― "] {
        if let Some((a, t)) = title.split_once(sep) {
            let (a, t) = (a.trim(), t.trim());
            if !a.is_empty() && !t.is_empty() {
                return Some((a.to_string(), t.to_string()));
            }
        }
    }
    None
}

/// The artist behind a channel: "Queen - Topic", "QueenVEVO", "Queen Official".
fn artist_of_channel(channel: &str) -> String {
    let c = channel.trim();
    let c = c.strip_suffix(" - Topic").unwrap_or(c);
    let c = c.strip_suffix("VEVO").unwrap_or(c);
    let c = c.strip_suffix(" Official").unwrap_or(c);
    c.trim().to_string()
}

/// The tags of a video, from yt-dlp's description of it.
pub fn tags_of(info: &serde_json::Value) -> SongTags {
    let s = |k: &str| info.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let artists = info.get("artists").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let known = [artists, s("artist"), s("creator")].into_iter().find(|a| !a.is_empty()).unwrap_or_default();
    let channel = [s("channel"), s("uploader")].into_iter().find(|a| !a.is_empty()).unwrap_or_default();
    let year = info.get("release_year").and_then(|y| y.as_u64()).map(|y| y as u32).filter(|y| (1000..=9999).contains(y));
    let track = s("track");
    let (title, artist) = if !track.is_empty() {
        let artist = if known.is_empty() { artist_of_channel(&channel) } else { known };
        (track, artist)
    } else {
        let cleaned = clean_title(&s("title"));
        match split_artist(&cleaned) {
            Some((a, t)) => (t, a),
            None => (cleaned, if known.is_empty() { artist_of_channel(&channel) } else { known }),
        }
    };
    SongTags { title, artist, album: s("album"), year }
}

/// The label on every song added from YouTube.
pub const LABEL: &str = "youtube";

/// What gets written into the file; `source` says where it came from. An album
/// of an earlier keep, still in the cached file, is cleared.
pub fn tag_edit(t: &SongTags, source: &str) -> TagEdit {
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    TagEdit {
        title: some(&t.title),
        artist: some(&t.artist),
        album: Some(t.album.clone()),
        albumartist: Some(String::new()),
        track: Some(String::new()),
        year: t.year.map(|y| y.to_string()),
        comment: Some(source.to_string()),
        labels: Some(vec![LABEL.to_string()]),
        ..TagEdit::default()
    }
}

/// The album artist of a playlist whose songs are by many.
pub const VARIOUS: &str = "Various Artists";

/// A playlist as an album: what it's called and whose it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub title: String,
    pub artist: String,
}

/// "The Band - The Album (Official Album Playlist)" is The Album by The Band.
/// Without an artist in its title, it's by whoever uploaded all of its songs, or
/// by many if they weren't all from one channel.
pub fn album_of(title: &str, song_channels: &[&str]) -> Album {
    let plain = title.trim();
    // What YouTube calls its own album playlists.
    let plain = ["Album - ", "Album – "].iter().find_map(|p| plain.strip_prefix(p)).unwrap_or(plain);
    let cleaned = clean_title(plain);
    let from_channels = || {
        let mut names = song_channels.iter().map(|c| artist_of_channel(c)).filter(|c| !c.is_empty());
        match names.next() {
            Some(first) if names.all(|n| n.eq_ignore_ascii_case(&first)) => first,
            _ => VARIOUS.to_string(),
        }
    };
    let (album, artist) = match split_artist(&cleaned) {
        // "FULL ALBUM - RUMOURS" names no artist.
        Some((a, t)) if is_noise(&a) => (t, from_channels()),
        Some((a, t)) => (t, a),
        None => (cleaned.clone(), from_channels()),
    };
    Album { title: if album.is_empty() { plain.to_string() } else { album }, artist }
}

/// A song's place on an album made from a playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumSlot {
    /// The playlist's id.
    pub list: String,
    pub album: String,
    pub artist: String,
    /// Its place in the playlist, from 1.
    pub track: u32,
    /// The channel the song is on.
    pub channel: String,
}

/// A song's tags on its album. Where all that's known of its artist is the channel
/// it's on, and that's someone's upload of an album, the album's artist is better.
pub fn on_album(t: &SongTags, slot: &AlbumSlot) -> SongTags {
    let only_the_channel = t.artist.eq_ignore_ascii_case(&artist_of_channel(&slot.channel));
    let artist = if only_the_channel && slot.artist != VARIOUS { slot.artist.clone() } else { t.artist.clone() };
    SongTags { title: t.title.clone(), artist, album: slot.album.clone(), year: t.year }
}

/// Just the album's tags, for a song already in the library.
pub fn album_place(slot: &AlbumSlot) -> TagEdit {
    TagEdit {
        album: Some(slot.album.clone()),
        albumartist: Some(slot.artist.clone()),
        track: Some(slot.track.to_string()),
        ..TagEdit::default()
    }
}

/// What gets written into a song kept on an album.
pub fn album_edit(t: &SongTags, slot: &AlbumSlot, source: &str) -> TagEdit {
    TagEdit { albumartist: Some(slot.artist.clone()), track: Some(slot.track.to_string()), ..tag_edit(&on_album(t, slot), source) }
}

/// The folder an album goes in: a name that works on every system.
pub fn album_folder(slot: &AlbumSlot) -> String {
    let artist = if slot.artist == VARIOUS { String::new() } else { slot.artist.clone() };
    file_stem(&SongTags { title: slot.album.clone(), artist, ..SongTags::default() }, &slot.list)
}

/// A file name (no extension) that works on every system.
pub fn file_stem(t: &SongTags, id: &str) -> String {
    let joined = if t.artist.is_empty() { t.title.clone() } else { format!("{} - {}", t.artist, t.title) };
    let mut s: String = joined.chars().map(|c| if "/\\:*?\"<>|".contains(c) { ' ' } else { c }).filter(|c| !c.is_control()).collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // At most 150 bytes, on a character's edge: room for the extension and an id.
    if s.len() > 150 {
        let mut end = 150;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    let s = s.trim_start_matches(['.', ' ']).trim_end_matches(['.', ' ']).to_string();
    let upper = s.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str()) || ((upper.starts_with("COM") || upper.starts_with("LPT")) && upper.len() == 4 && upper.as_bytes()[3].is_ascii_digit());
    if s.is_empty() {
        id.to_string()
    } else if reserved {
        format!("{s} {id}")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Bohemian Rhapsody (Official Video Remastered)"), "Bohemian Rhapsody");
        assert_eq!(clean_title("Song [Official Music Video] (HD)"), "Song");
        assert_eq!(clean_title("Song (Lyrics)"), "Song");
        assert_eq!(clean_title("Song (with lyrics) [4K]"), "Song");
        assert_eq!(clean_title("Song (Live at Wembley)"), "Song (Live at Wembley)");
        assert_eq!(clean_title("Song (Remix) [Official Audio]"), "Song (Remix)");
        assert_eq!(clean_title("Song | Official Video"), "Song");
        assert_eq!(clean_title("One | Two"), "One | Two");
        assert_eq!(clean_title("Unclosed (bracket"), "Unclosed (bracket");
        assert_eq!(clean_title("Títulö (Official Vídeo) ñ"), "Títulö ñ");
    }

    #[test]
    fn splits_the_usual_title() {
        let t = tags_of(&json!({"title": "Queen – Bohemian Rhapsody (Official Video Remastered)", "channel": "Queen Official", "upload_date": "20080801"}));
        assert_eq!((t.artist.as_str(), t.title.as_str(), t.year), ("Queen", "Bohemian Rhapsody", None));
        let t = tags_of(&json!({"title": "Queen - Bohemian Rhapsody (Lyrics)", "channel": "7clouds Rock"}));
        assert_eq!((t.artist.as_str(), t.title.as_str()), ("Queen", "Bohemian Rhapsody"));
    }

    #[test]
    fn falls_back_on_the_channel() {
        let t = tags_of(&json!({"title": "Dreams (Official Audio)", "channel": "Some Band - Topic"}));
        assert_eq!((t.artist.as_str(), t.title.as_str()), ("Some Band", "Dreams"));
        let t = tags_of(&json!({"title": "Dreams", "channel": "SomeBandVEVO"}));
        assert_eq!(t.artist, "SomeBand");
        let t = tags_of(&json!({"title": "Dreams", "uploader": "Someone"}));
        assert_eq!(t.artist, "Someone");
    }

    #[test]
    fn what_youtube_knows_comes_first() {
        let t = tags_of(&json!({"title": "A - B (Official Video)", "track": "Real Title", "artists": ["One", "Two"], "artist": "One", "album": "The Album", "release_year": 1975, "channel": "X"}));
        assert_eq!(t, SongTags { title: "Real Title".into(), artist: "One, Two".into(), album: "The Album".into(), year: Some(1975) });
    }

    #[test]
    fn writes_only_what_it_knows() {
        let e = tag_edit(&SongTags { title: "T".into(), ..Default::default() }, "https://x");
        assert_eq!(e.title.as_deref(), Some("T"));
        assert_eq!(e.artist, None, "an empty tag isn't written");
        assert_eq!(e.comment.as_deref(), Some("https://x"));
        assert_eq!(e.labels, Some(vec!["youtube".to_string()]));
        // What a keep on an album left in the cached file doesn't carry over.
        assert_eq!((e.album.as_deref(), e.albumartist.as_deref(), e.track.as_deref()), (Some(""), Some(""), Some("")));
    }

    #[test]
    fn a_playlist_is_an_album() {
        let a = |t: &str, c: &[&str]| { let x = album_of(t, c); (x.title, x.artist) };
        let s = |x: &str, y: &str| (x.to_string(), y.to_string());
        assert_eq!(a("Fleetwood Mac - Rumours (Official Album Playlist)", &["Fleetwood Mac"]), s("Rumours", "Fleetwood Mac"));
        assert_eq!(a("Fleetwood Mac - Rumours (Full Album HQ)", &["Caveguy22", "Other"]), s("Rumours", "Fleetwood Mac"));
        assert_eq!(a("Album - Rumours", &["Fleetwood Mac - Topic", "Fleetwood Mac - Topic"]), s("Rumours", "Fleetwood Mac"));
        assert_eq!(a("FULL ALBUM - RUMOURS", &["QueenVEVO"]), s("RUMOURS", "Queen"));
        assert_eq!(a("Road Trip Tunes", &["One", "Two"]), s("Road Trip Tunes", VARIOUS));
        assert_eq!(a("Road Trip Tunes", &[]), s("Road Trip Tunes", VARIOUS));
        assert_eq!(a("(Official Video)", &["One"]), s("(Official Video)", "One"), "a name is still needed");
    }

    #[test]
    fn a_song_on_an_album() {
        let slot = |artist: &str, channel: &str| AlbumSlot { list: "PLx".into(), album: "The Album".into(), artist: artist.into(), track: 3, channel: channel.into() };
        let t = SongTags { title: "T".into(), artist: "Caveguy22".into(), album: "Not This".into(), year: Some(1999) };
        // The album's artist over an uploader's name, but not over a real one.
        assert_eq!(on_album(&t, &slot("The Band", "Caveguy22")).artist, "The Band");
        assert_eq!(on_album(&t, &slot("The Band", "Someone Else")).artist, "Caveguy22");
        assert_eq!(on_album(&t, &slot(VARIOUS, "Caveguy22")).artist, "Caveguy22");
        let e = album_edit(&t, &slot("The Band", "Caveguy22"), "https://x");
        assert_eq!((e.album.as_deref(), e.albumartist.as_deref(), e.track.as_deref(), e.year.as_deref()), (Some("The Album"), Some("The Band"), Some("3"), Some("1999")));
        assert_eq!(e.labels, Some(vec!["youtube".to_string()]));
        // Only what makes it part of the album, for a file already kept.
        let p = album_place(&slot("The Band", "x"));
        assert_eq!((p.title, p.labels, p.comment), (None, None, None));
        assert_eq!(album_folder(&slot("The Band", "x")), "The Band - The Album");
        assert_eq!(album_folder(&slot(VARIOUS, "x")), "The Album");
    }

    #[test]
    fn names_work_everywhere() {
        let t = |a: &str, ti: &str| SongTags { artist: a.into(), title: ti.into(), ..Default::default() };
        assert_eq!(file_stem(&t("AC/DC", "T.N.T."), "i"), "AC DC - T.N.T");
        assert_eq!(file_stem(&t("", "What?"), "i"), "What");
        assert_eq!(file_stem(&t("", "..."), "abc"), "abc");
        assert_eq!(file_stem(&t("", "CON"), "abc"), "CON abc");
        assert_eq!(file_stem(&t("", "COM3"), "abc"), "COM3 abc");
        assert_eq!(file_stem(&t("", ".hidden"), "abc"), "hidden");
        let long = file_stem(&t("ñ", &"é".repeat(300)), "abc");
        assert!(long.len() <= 150 && long.starts_with("ñ - é"));
    }
}
