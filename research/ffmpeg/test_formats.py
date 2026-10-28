#!/usr/bin/env python3
"""Convert one test file per source format with the audio-only ffmpeg, the way
the converter would (CBR at the cap, 44.1 or 48 kHz kept, otherwise 44.1 kHz,
stereo), and check every MP3 frame header is at the cap.

    python3 test_formats.py <audio-only ffmpeg> <dir with t_* test files> <out dir>

Test files were made with a full ffmpeg from one 20-second source: AAC, AIFF,
ALAC, FLAC, 5.1 FLAC, MP3 320, Opus, Vorbis, 96 kHz/24-bit WAV, WavPack, WMA 1, WMA 2.
"""
import os, subprocess, sys
FF, SRC, OUT = sys.argv[1:4]
FP = os.path.join(os.path.dirname(FF), "ffprobe")
BR = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320]
SR = {0: 44100, 1: 48000, 2: 32000}

def frame_bitrates(path):
    d = open(path, "rb").read()
    i = 0
    if d[:3] == b"ID3":
        i = 10 + ((d[6] & 127) << 21 | (d[7] & 127) << 14 | (d[8] & 127) << 7 | (d[9] & 127))
    seen, n = set(), 0
    while i + 4 <= len(d) and d[i] == 0xFF and (d[i + 1] & 0xE0) == 0xE0:
        b, s, pad = BR[d[i + 2] >> 4], SR[(d[i + 2] >> 2) & 3], (d[i + 2] >> 1) & 1
        seen.add(b); n += 1; i += 144000 * b // s + pad
    return n, sorted(seen)

os.makedirs(OUT, exist_ok=True)
ok = True
for cap in (256, 320):
    for f in sorted(x for x in os.listdir(SRC) if x.startswith("t_")):
        src = os.path.join(SRC, f)
        sr = int(subprocess.run([FP, "-v", "error", "-select_streams", "a:0", "-show_entries", "stream=sample_rate",
                                 "-of", "csv=p=0", src], capture_output=True, text=True).stdout.strip())
        ar = sr if sr in (44100, 48000) else 44100
        out = os.path.join(OUT, f"{os.path.splitext(f)[0]}.{cap}.mp3")
        r = subprocess.run([FF, "-v", "error", "-y", "-i", src, "-map", "0:a", "-af", f"aresample={ar},aformat=channel_layouts=stereo",
                            "-c:a", "libmp3lame", "-b:a", f"{cap}k", "-compression_level", "2", out], capture_output=True, text=True)
        n, rates = frame_bitrates(out) if r.returncode == 0 else (0, [])
        good = r.returncode == 0 and rates == [cap]
        ok &= good
        print(f"{'ok  ' if good else 'FAIL'} cap {cap}: {f:18} {ar} Hz, {n} frames, bitrates {rates} {r.stderr.strip()[:80]}")
sys.exit(0 if ok else 1)
