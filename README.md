# Playlist2USB (prototype)

![Playlist2USB main window](docs/gui.png)

Writes an M3U/M3U8, XSPF (VLC) or PLS playlist onto a USB stick so that cheap
MP3 players and car stereos, which play files in **FAT order** rather than by
name or tag, play it in playlist order. FAT order is the order of the entries
in each folder's table on the FAT volume (what fatsort, YAFS and DriveSort
sort); it is not stored in the File Allocation Table itself, which only chains
clusters.

## How it works

**Writing a stick erases it.** The app doesn't copy through the OS, and it
doesn't try to adapt to whatever is already on the stick. It lays down a fresh
FAT32 file system itself:

- one MBR partition (type 0x0C, FAT32 LBA) from 1 MiB to the end of the stick,
  cluster size from the table in Microsoft's FAT specification, first cluster
  aligned to 4 MiB (1 MiB on sticks under 2 GiB);
- at the top level only the volume label and the playlist folder (or the
  tracks themselves);
- in the folder, `.` and `..`, then one entry per track in playlist order;
- every track's data in one unbroken run of clusters, back to back in playlist
  order, straight after the folder tables;
- names numbered and modification times spaced 2 s apart, so players that
  sort by name or date agree.

Why not copy? With the real Linux driver an in-order copy came out wrong in 4
of 7 stick histories (`research/`), and Windows picks directory slots
differently again. Why not rewrite the existing file system? Because then every
odd format, leftover partition, FAT12/16 volume and scattered free space is an
edge case. FAT32 only: the players this is for implement FAT32 and little else.

The write runs in a fixed order, with the partition table last, so pulling the
stick at any point leaves either a stick every OS sees as blank or the complete
one:

1. Plan everything (geometry, names, every cluster). If it doesn't fit, stop:
   nothing has been written.
2. Erase the first and last MiB: old partition tables (including a GPT's backup
   header) and file-system signatures.
3. Reserved sectors (boot sector still zero), both FATs, the folder tables,
   every track.
4. The backup boot sector, then the primary.
5. The MBR.
6. Drop the OS cache and read it all back: MBR, boot sectors, FSInfo, both FATs
   and the folder tables byte for byte against the plan; the folder parsed the
   way a player reads it; every byte of every track against its source.

Then the stick is powered off (Linux) or ejected, so nothing writes to it
before you unplug it.

| OS      | Raw access to the whole stick                            | Prompt                                   |
|---------|----------------------------------------------------------|------------------------------------------|
| Linux   | unmount every partition, udisks2 `Block.OpenDevice` (O_EXCL) returns an fd | polkit agent; fingerprint if PAM has `pam_fprintd` |
| macOS   | `diskutil unmountDisk`, `/usr/libexec/authopen -stdoutpipe /dev/rdiskN` returns an fd | authd dialog (Touch ID untested) |
| Windows | `p2u-helper.exe` (UAC) locks + dismounts every volume, opens `\\.\PhysicalDriveN` | UAC; Windows Hello when configured |

Only USB and SD disks are listed, and the disk the system runs from is refused
on all three. The GUI never runs as root/admin. No in-app biometric check is
used: those APIs only return yes/no and cannot grant privileges; the OS prompt
is what accepts the fingerprint.

## The window (prototype)

`app/ui` is the app's real front end, currently running on demo data: a
playlists sidebar, the track list (multi-select, drag sorting with pointer
events, drops from the file manager, undo), a stick panel (player profile,
playlists on the stick, file name template with a live preview, the layout of
the stick and whether it fits) and a player bar. It comes in three looks to
choose from, switched from the top right: Backlit, Night drive and Clip.

In the desktop app it lists your real USB sticks and imports real playlist
files, but **Sync is simulated there too**: it needs the library, converter and
multi-folder writer from the design doc first. To write a stick today, use the
CLI below. To try the window in a browser:

```sh
cd app/ui && python3 -m http.server 8000      # then open http://localhost:8000
python3 app/bundle_preview.py preview.html     # or one self-contained file
node tests/ui_names.js                         # name template rules
node tests/ui_check.js                         # clicks through it (needs playwright and ffmpeg)
```

## Layout

```
crates/p2u-core     playlists, naming plan, FAT32 layout + writer + read-back check, FAT32 reader (+ unit tests)
crates/p2u-device   USB stick discovery and OS-brokered raw disk access (linux/macos/windows)
crates/p2u-cli      `p2u` command-line tool
crates/p2u-helper   tiny privileged helper (Windows UAC, Linux pkexec fallback)
app/                Tauri v2 GUI: ui/ is plain HTML/CSS/JS (js/names.js and js/rules.js hold the naming
                    and copy-or-encode rules), src-tauri/ the Rust shell, bundle_preview.py a one-file preview
packaging/linux     polkit policy for the pkexec fallback
tests/stick_e2e.py    84 checks on disk images with awkward prior layouts, plus a pull at every write point
tests/kernel_stick.py `p2u write` on whole virtual disks in QEMU, then the real Linux vfat driver
tests/ui_*.js        name template unit test; a headless click-through of the window
research/           the copy-order measurement (needs the older prototype, commit 93f9b75)
.github/workflows   CI draft: Linux + Windows built on Linux, macOS on a macOS runner
```

## Build

Linux (Ubuntu 24.04 packages):

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev
cargo build --release                       # p2u, p2u-helper
mkdir -p app/src-tauri/binaries \
  && cp target/release/p2u-helper app/src-tauri/binaries/p2u-helper-x86_64-unknown-linux-gnu
cd app/src-tauri && cargo build --release   # GUI; `npx @tauri-apps/cli@^2 build` for .deb/.rpm/AppImage
```

Windows, from Linux:

```sh
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release -p p2u-cli -p p2u-helper --target x86_64-pc-windows-gnu
cp target/x86_64-pc-windows-gnu/release/p2u-helper.exe app/src-tauri/binaries/p2u-helper-x86_64-pc-windows-gnu.exe
cd app/src-tauri && cargo build --release --target x86_64-pc-windows-gnu
```

The GNU build of the GUI needs `WebView2Loader.dll` shipped beside the exe;
cargo leaves it in the same output folder. For an installer, use Tauri's
documented route instead: `npx @tauri-apps/cli@^2 build --runner cargo-xwin
--target x86_64-pc-windows-msvc` (NSIS, LLVM and cargo-xwin installed; see the
CI workflow), with the helper copied to
`binaries/p2u-helper-x86_64-pc-windows-msvc.exe`.

macOS: build on a Mac or the `macos` CI job (`--target universal-apple-darwin`);
the Apple SDK licence rules out building on Linux.

## CLI

```sh
p2u sticks                                            # USB sticks and what is on them
p2u plan "Road Trip.xspf"                             # names and order, writes nothing
p2u write <stick> --playlist "Road Trip.xspf"         # ERASES the stick: asks you to type "erase", then the OS prompt
p2u write <stick> --playlist "Road Trip.xspf" --top-level --label ROADTRIP
# Disk images, and whole-disk device nodes you can open yourself:
truncate -s 1G stick.img
p2u write stick.img --playlist "Road Trip.xspf" --yes
p2u write stick.img --playlist "Road Trip.xspf" --dry-run   # the layout as JSON, writes nothing
p2u ls stick.img --dir "/Road Trip"                   # FAT order, as a player reads it
p2u verify stick.img --playlist "Road Trip.xspf"      # order and sizes
p2u inspect stick.img
```

## Test

```sh
cargo test --workspace
sudo apt install dosfstools mtools fatsort fdisk gdisk exfatprogs ntfs-3g e2fsprogs
cargo build -p p2u-cli
python3 tests/stick_e2e.py      # 84 checks, verified with sfdisk, blkid, wipefs, fsck.fat, fatsort, mtools
# Real Linux driver (needs qemu-system-x86, busybox-static and an extracted Ubuntu kernel):
cargo build --release -p p2u-cli --target x86_64-unknown-linux-musl
python3 tests/kernel_stick.py --kroot <kernel dir> --p2u target/x86_64-unknown-linux-musl/release/p2u
```

## Status

Prototype. The writer is tested on disk images from 64 MiB to 40 GiB that
started out blank, junk-filled, GPT/exFAT, NTFS/ext4, FAT16 or used FAT32, with
a simulated pull at every write point, and on four virtual sticks read back by
the real Linux vfat driver. The OS broker and disk-level code (unmount, raw
whole-disk access, power-off/eject) compiles for Linux, Windows and macOS but
has not yet run against real USB sticks. Writing erases the stick; it is not a
secure erase. Licence: MIT OR Apache-2.0 (no GPL code was copied; the code
follows the FAT specification). The bundled fonts (DSEG14, Barlow, Saira Semi
Condensed, M PLUS Rounded 1c, Doto, Silkscreen) are under the SIL Open Font
License; their licence texts are in app/ui/fonts.
