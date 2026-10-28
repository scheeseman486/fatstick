//! Core of Fatstick: read a playlist, then erase a USB stick and write a
//! fresh FAT32 file system holding the tracks in playlist order, both in FAT
//! order (which is what cheap players follow) and in where the data sits.

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
    let mut s = String::from("# Fatstick order file: one directory entry name per line\n");
    for n in names {
        s.push_str(n);
        s.push('\n');
    }
    std::fs::write(path, s)
}
