# Third-party software in Playlist2USB

Playlist2USB is free software under the GNU General Public License, version 3
or later (see `COPYING`). It ships with the following parts made by others.

## FFmpeg (the `ffmpeg` and `ffprobe` programs)

- Version: n9.0.2, from <https://github.com/FFmpeg/FFmpeg> (tag `n9.0.2`).
- Licence: GNU Lesser General Public License, version 2.1 or later
  (`LGPL-2.1.txt`). The build uses neither `--enable-gpl` nor
  `--enable-nonfree`.
- Built by `packaging/ffmpeg/build-ffmpeg.sh` in the Playlist2USB source,
  which fetches the exact sources and holds the full configure line: an
  audio-only build with decoders for MP3, FLAC, ALAC, WAV, AIFF, AAC, Vorbis,
  Opus, WMA, WavPack and APE, the libmp3lame encoder, and the aresample,
  volume and ebur128 filters.
- FFmpeg runs as a separate program; Playlist2USB starts it and reads its
  output. You may replace the bundled `ffmpeg` and `ffprobe` with your own
  build: set `P2U_FFMPEG_DIR` to the folder holding them.

## LAME (inside the bundled ffmpeg)

- Version: 3.100, from
  <https://downloads.sourceforge.net/project/lame/lame/3.100/lame-3.100.tar.gz>
  (SHA-256 `ddfe36cab873794038ae2c1210557ad34857a4b6bdc515785d1da9e175b1da1e`).
- Licence: GNU Library General Public License, version 2 or later
  (`LGPL-2.0.txt`). Linked statically into `ffmpeg`; the build script above
  rebuilds both from source, so the result can be relinked with a modified
  LAME.

## Written offer

For three years from the date you received a copy of Playlist2USB, the
source code of the exact FFmpeg and LAME versions above is available from the
same place you got Playlist2USB: every release carries the source archives
alongside the installers. The build script fetches them too.

## Fonts (in the window)

- DSEG14 Classic, by keshikan: SIL Open Font License 1.1 (`app/ui/fonts/DSEG-LICENSE.txt`).
- Barlow, by Jeremy Tribby: SIL Open Font License 1.1 (`app/ui/fonts/barlow-OFL.txt`).

## Signalsmith Stretch (key shifting)

Signalsmith Stretch 1.3.1 and Signalsmith Linear, by Geraint Luff /
Signalsmith Audio, compiled into the app for changing a track's key. The C
wrapper is adapted from the signalsmith-stretch crate by Colin Marc. All MIT:

> Copyright (c) 2022 Geraint Luff / Signalsmith Audio Ltd.
> Copyright (c) 2025 Signalsmith Audio
> Copyright 2024 Colin Marc
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to
> deal in the Software without restriction, including without limitation the
> rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
> sell copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in
> all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
> FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
> IN THE SOFTWARE.

Sources: `crates/p2u-stretch/vendor` in the Playlist2USB source.

## Rust libraries

Compiled into the Playlist2USB programs. All are under licences compatible
with the GPL version 3: mostly MIT and/or Apache-2.0, with a few under BSD,
ISC, Zlib, Unicode-3.0 or MPL-2.0 terms. Among them: Tauri, rusqlite (with
SQLite, public domain), id3, image, mp3rgain (MIT), cpal, blake3, serde,
zbus and quick-xml. `cargo tree` in the source lists every one with its
version; each crate's licence text is in its source package on crates.io.
