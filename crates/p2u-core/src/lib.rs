//! Core of Playlist2USB: read a playlist, copy it to a FAT volume in order, and
//! (with raw device access) rewrite the directory so the on-disk entry order,
//! which is what cheap players follow, matches the playlist exactly.

pub mod copy;
pub mod fat;
pub mod job;
pub mod playlist;
pub mod plan;

/// Read an order file: one name per line (UTF-8), blank lines and `#` comments ignored.
pub fn read_order_file(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

pub fn write_order_file(path: &std::path::Path, names: &[String]) -> std::io::Result<()> {
    let mut s = String::from("# Playlist2USB order file: one directory entry name per line\n");
    for n in names {
        s.push_str(n);
        s.push('\n');
    }
    std::fs::write(path, s)
}
