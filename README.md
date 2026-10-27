# Playlist2USB (prototype)

![Playlist2USB main window](docs/gui.png)

Writes an M3U/M3U8, XSPF (VLC) or PLS playlist onto a FAT USB stick so that
cheap MP3 players and car stereos, which play files in **FAT order** rather than
by name or tag, play it in playlist order. FAT order is the order of the entries
in each folder's table on the FAT volume (what fatsort, YAFS and DriveSort sort);
it is not stored in the File Allocation Table itself, which only chains clusters.

## How it works

Copying files in playlist order through the OS does **not** reliably give that
order on disk: with the real Linux driver it came out wrong when an earlier copy
was overwritten, when an existing folder had deleted files, and at the top level
of a used stick (`tests/kernel_vfat.py`), and Windows picks directory slots
differently again. Some players may also go by where files sit on the stick.

So the app doesn't copy through the OS at all. **Write to stick** unmounts the
stick, gets raw access through the OS's own broker (one prompt), and writes the
files into the FAT file system itself:

- entries go into the folder's table in playlist order;
- each file gets one unbroken run of clusters, files back to back in playlist
  order, from the lowest free block big enough. If there is none, the write is
  refused, unless split files are allowed (clusters still ascend through the
  playlist);
- names are numbered and modification times spaced 2 s apart, so players that
  sort by name or date agree.

The write runs in a fixed order, so pulling the stick at any point leaves it as
it was (plus clusters marked used that nothing points to, which `fsck`
reclaims) or holding the complete folder:

1. Plan every name and cluster. Refuse if the volume is flagged in use, the FAT
   copies disagree, the folder exists (without `--replace`), a file is over
   4 GiB, or there's no room.
2. Before each phase, save an undo backup of every metadata sector it will
   touch. No backup, no write.
3. Replacing: mark the old entries deleted, then free their clusters.
4. Write file data and the new folder's table into free clusters.
5. Link the clusters in every FAT copy, active copy first.
6. Commit: one entry in the parent table makes the folder appear, complete. At
   the top level the table is rewritten in two passes (delete, then add), so a
   pull can hide an entry but never duplicate one.
7. Update FSInfo, drop the OS cache and read everything back from the stick:
   entry order, every chain against the plan, the FAT copies against each other
   and every byte against the source.

The stick is then left unmounted (ejected on Windows), so nothing writes to it
after the check. **Fix order only** reorders a folder already on the stick, in
place, with the same backup-first and verify-from-the-medium rules.

| OS      | Raw access                                   | Prompt                                   |
|---------|----------------------------------------------|------------------------------------------|
| Linux   | udisks2 `Block.OpenDevice` returns an fd     | polkit agent; fingerprint if PAM has `pam_fprintd` |
| macOS   | `/usr/libexec/authopen -stdoutpipe` returns an fd | authd dialog (Touch ID untested)    |
| Windows | `p2u-helper.exe` (UAC) locks + dismounts `\\.\E:` | UAC; Windows Hello when configured |

The GUI never runs as root/admin. No in-app biometric check is used: those
APIs only return yes/no and cannot grant privileges; the OS prompt is what
accepts the fingerprint.

## Layout

```
crates/p2u-core     playlists, naming plan, FAT12/16/32 writer + reorder, verify from the medium, backup/restore
crates/p2u-device   volume discovery and OS-brokered raw access (linux/macos/windows)
crates/p2u-cli      `p2u` command-line tool
crates/p2u-helper   tiny privileged helper (Windows UAC, Linux pkexec fallback)
app/                Tauri v2 GUI (ui/ is plain HTML/CSS/JS, src-tauri/ is the Rust shell)
packaging/linux     polkit policy for the pkexec fallback
tests/write_e2e.py    raw writer on FAT images: 65 checks, a pull at every write point (76 runs)
tests/kernel_write.py raw writer on block devices in QEMU, then the real Linux vfat driver
tests/e2e.py          reorder ("Fix order only") on FAT images: 67 checks
tests/kernel_vfat.py  OS copy + reorder through the real Linux vfat driver
.github/workflows   CI draft: Linux + Windows built on Linux, macOS on a macOS runner
```

## Build

Linux (Ubuntu 24.04 packages):

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev
cargo build --release                       # p2u, p2u-helper
cargo build --release -p p2u-helper && mkdir -p app/src-tauri/binaries \
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

The GNU build of the GUI (4.6 MB) needs `WebView2Loader.dll` shipped beside the
exe; cargo leaves it in the same output folder. For an installer, use Tauri's
documented route instead: `npx @tauri-apps/cli@^2 build --runner cargo-xwin
--target x86_64-pc-windows-msvc` (NSIS, LLVM and cargo-xwin installed; see the
CI workflow), with the helper copied to
`binaries/p2u-helper-x86_64-pc-windows-msvc.exe`.

macOS: build on a Mac or the `macos` CI job (`--target universal-apple-darwin`);
the Apple SDK licence rules out building on Linux.

## CLI

```sh
p2u volumes
p2u plan "Road Trip.xspf" --dest /media/me/STICK
# Write at FAT level: a volume from `p2u volumes` (OS prompt), or a device/image you can open
p2u write /org/freedesktop/UDisks2/block_devices/sdb1 --playlist "Road Trip.xspf"
p2u write stick.img --playlist "Road Trip.xspf" --folder "Road Trip" --replace
p2u write stick.img --playlist "Road Trip.xspf" --top-level --allow-fragmented --dry-run
# Inspect, reorder in place, undo
p2u ls stick.img --dir "/Road Trip"
p2u verify stick.img --playlist "Road Trip.xspf"
p2u sort stick.img --playlist "Road Trip.xspf"
p2u restore stick.img p2u-backup-*.bin          # several backups are applied newest first
```

## Test

```sh
cargo test --workspace
sudo apt install dosfstools mtools fatsort fdisk
cargo build -p p2u-cli
python3 tests/write_e2e.py   # 65 checks; verified independently with fsck.fat, fatsort -l, mshowfat, mcopy
python3 tests/e2e.py         # 67 checks for the reorder
# Real Linux driver (needs qemu-system-x86, busybox-static and an extracted Ubuntu kernel):
cargo build --release -p p2u-cli --target x86_64-unknown-linux-musl
python3 tests/kernel_write.py --kroot <kernel dir> --p2u target/x86_64-unknown-linux-musl/release/p2u
python3 tests/kernel_vfat.py  --kroot <kernel dir> --p2u target/x86_64-unknown-linux-musl/release/p2u
```

## Status

Prototype. The writer is tested on FAT12/16/32 images, with a simulated pull
at every write point, and on four used virtual sticks read back by the real
Linux vfat driver. The OS broker code paths compile for Linux, Windows and
macOS but have not yet run against real USB sticks, so try it first on a stick
holding nothing you need. exFAT is detected and refused. Restore is CLI-only.
Licence: MIT OR Apache-2.0 (no GPL code was copied; the code follows the FAT
specification). The bundled DSEG14 font is under the SIL Open Font License
(app/ui/fonts/DSEG-LICENSE.txt).
