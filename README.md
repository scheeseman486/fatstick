# Playlist2USB (prototype)

![Playlist2USB main window](docs/gui.png)

Copies an M3U/M3U8, XSPF (VLC) or PLS playlist onto a FAT32 USB stick so that
cheap MP3 players and car stereos, which play files in **FAT order** rather than
by name or tag, play it in playlist order. FAT order is the order of the entries
in each folder's table on the FAT volume (what fatsort, YAFS and DriveSort sort);
it is not stored in the File Allocation Table itself, which only chains clusters.

## How it works

The order is always set and checked at FAT level. Copying files in playlist
order through the OS does **not** reliably give that order on disk: with the
real Linux driver it came out wrong when an earlier copy was overwritten, when
an existing folder had deleted files, and at the top level of a used stick
(`tests/kernel_vfat.py`), and Windows picks directory slots differently again.
So every copy is followed by the FAT-level step, in one action:

1. **Copy the files (no prompt).** One at a time, through the OS, into a folder
   or the top level. No temp-file-then-rename, no macOS `._` AppleDouble files
   (it never uses `std::fs::copy`, which copies xattrs on macOS). The order this
   leaves is not trusted.
2. **Set the order at FAT level (one OS prompt).** The volume is unmounted and
   raw access is obtained through the OS's own broker. Then, in this order:
   read the folder's directory and compute the new layout; save an undo backup
   (if that fails, nothing is written); write the folder's directory sectors;
   flush and drop the OS cache; read them back from the medium and compare;
   re-parse and check the playlist order, every file's size and any extra
   entries. Only the folder's directory sectors are written, never the FAT,
   FSInfo or file data.
3. **Unplug.** The stick is left unmounted (ejected on Windows), so nothing
   writes to it after the check.

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
crates/p2u-core     playlists, copy plan, copy, FAT12/16/32 reorder + verify from the medium (+ unit tests)
crates/p2u-device   volume discovery and OS-brokered raw access (linux/macos/windows)
crates/p2u-cli      `p2u` command-line tool
crates/p2u-helper   tiny privileged helper (Windows UAC, Linux pkexec fallback)
app/                Tauri v2 GUI (ui/ is plain HTML/CSS/JS, src-tauri/ is the Rust shell)
packaging/linux     polkit policy for the pkexec fallback
tests/e2e.py        end-to-end tests on FAT images (67 checks)
tests/kernel_vfat.py  copy + lock through the real Linux vfat driver in QEMU
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

The GNU build of the GUI (4.4 MB) needs `WebView2Loader.dll` shipped beside the
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
p2u copy "Road Trip.xspf" --dest /media/me/STICK --lock          # copy, then set + verify order (one prompt)
p2u copy "Road Trip.xspf" --dest /media/me/STICK --top-level --lock
p2u lock /org/freedesktop/UDisks2/block_devices/sdb1 --playlist "Road Trip.xspf"   # order only
# Images and unmounted devices you can already open:
p2u ls stick.img --dir "/Road Trip"
p2u sort stick.img --playlist "Road Trip.xspf" --dry-run
p2u verify stick.img --playlist "Road Trip.xspf"
p2u restore stick.img p2u-backup-xxxxxxxx-<time>.bin
```

## Test

```sh
cargo test --workspace
sudo apt install dosfstools mtools fatsort fdisk
python3 tests/e2e.py      # 67 checks; independent verification with fatsort -l and fsck.fat
# Real Linux driver (needs qemu-system-x86, busybox-static and an extracted Ubuntu kernel):
cargo build --release -p p2u-cli --target x86_64-unknown-linux-musl
python3 tests/kernel_vfat.py --kroot <kernel dir> --p2u target/x86_64-unknown-linux-musl/release/p2u
```

## Status

Prototype. The FAT engine is tested on FAT12/16/32 images and the copy + lock
flow through the real Linux vfat driver; the OS broker code paths compile for
Linux, Windows and macOS but have not yet run against real USB sticks. exFAT is not supported. Licence: MIT OR Apache-2.0 (no GPL code was
copied; the algorithm follows the FAT specification). The bundled DSEG14 font is
under the SIL Open Font License (app/ui/fonts/DSEG-LICENSE.txt).
