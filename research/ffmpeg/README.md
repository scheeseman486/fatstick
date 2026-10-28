# Audio-only ffmpeg for Fatstick

`build-audio-ffmpeg.sh` builds the ffmpeg the app bundles: decoders for MP1/2/3,
FLAC, ALAC, AAC, Vorbis, Opus, WMA 1, 2, Pro and Lossless, APE, WavPack and PCM;
the matching demuxers; LAME (`libmp3lame`, linked statically); and the
`aresample`, `aformat`, `pan`, `volume` and `ebur128` filters. No `--enable-gpl`,
so the build is LGPL 2.1 or later.

Result on Ubuntu 24.04, x86-64, ffmpeg n9.0.2 (2026-10-28):

- `ffmpeg` 5.8 MB stripped (`ffprobe` 5.6 MB), depending only on libc and libm;
- 25 audio decoders;
- `test_formats.py`: all twelve test files (AAC, AIFF, ALAC, FLAC, 5.1 FLAC, MP3,
  Opus, Vorbis, 96 kHz/24-bit WAV, WavPack, WMA 1, WMA 2) converted to CBR MP3 at
  256 and 320 kbps with every frame header at the cap; each 20-second file took
  under 0.2 s;
- seeking 10 s into WMA, Opus and 5.1 FLAC files and decoding 2 s of 48 kHz float
  PCM to a pipe (as the player would) took 7 to 12 ms.

WMA Pro and WMA Lossless are built in but untested: ffmpeg can't create such files.

Watch out: ffmpeg's `-q:a` switches libmp3lame to VBR. LAME's algorithm quality
(`lame -q`) is `-compression_level`. With `-b:a 256k -q:a 2` the "256 kbps"
files came out at about 115 kbps.

Licence duties when shipping this binary: include ffmpeg's and LAME's licence
texts, credit both, publish the exact source and this script next to each
release's downloads, and keep the binary's name recognisable.
