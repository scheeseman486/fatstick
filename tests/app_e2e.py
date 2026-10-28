#!/usr/bin/env python3
"""The real app, end to end, on a disk image standing in for a USB stick.

Makes real audio (FLAC, Opus, MP3s at 320 kbps and at 22.05 kHz, WAV), then
runs a debug build of the app twice under Xvfb with P2U_SELFTEST:

1. tests/app_selftest.js drives the window from an empty library to a
   finished sync of two playlists (see that file).
2. tests/app_selftest2.js restarts on the same library and checks that
   everything was kept.

Then it reads the image with tools that aren't ours: fsck.fat, mtools (the
order a player sees), ffprobe (codec, bitrate, tags, cover) and ebur128
(loudness evened out to -18 LUFS).

    cargo build --manifest-path app/src-tauri/Cargo.toml   # debug build
    packaging/ffmpeg/build-ffmpeg.sh x86_64-unknown-linux-gnu   # the bundled ffmpeg
    python3 tests/app_e2e.py

Needs ffmpeg and ffprobe on the PATH (for fixtures and checks), xvfb-run,
mtools and dosfstools.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "app", "src-tauri", "target", "debug", "playlist2usb")
RESULTS = []


def record(name, ok, extra=""):
    RESULTS.append((name, bool(ok)))
    print(f"{'PASS' if ok else 'FAIL'} {name}{f'  ({extra})' if extra else ''}", flush=True)


def ff(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def make_fixtures(work):
    music = os.path.join(work, "Music", "Album")
    music2 = os.path.join(work, "Music", "Live")
    os.makedirs(music)
    os.makedirs(music2)
    cover = os.path.join(work, "cover.png")
    ff("-f", "lavfi", "-i", "testsrc2=size=800x800:duration=1", "-frames:v", "1", cover)
    tone = lambda f, db, secs=8, rate=44100: f"sine=frequency={f}:sample_rate={rate}:duration={secs},volume={db}dB"
    meta = lambda title, n, artist="The Fixtures": ["-metadata", f"title={title}", "-metadata", f"artist={artist}", "-metadata", "album=Album", "-metadata", f"track={n}"]
    # File names sort differently from the track numbers: album order is by number.
    ff("-f", "lavfi", "-i", tone(440, -4), "-i", cover, "-map", "0:a", "-map", "1:v", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "320k",
       "-c:v", "png", "-disposition:v", "attached_pic", "-id3v2_version", "3", *meta("Third", 3), os.path.join(music, "a third.mp3"))
    ff("-f", "lavfi", "-i", tone(330, -3), "-ac", "2", "-c:a", "flac", *meta("First", 1), os.path.join(music, "b first.flac"))
    ff("-f", "lavfi", "-i", tone(550, -12), "-ac", "2", "-c:a", "libopus", "-b:a", "128k", *meta("Second", 2), os.path.join(music, "c second.opus"))
    ff("-f", "lavfi", "-i", tone(660, -9, rate=22050), "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k", *meta("Fourth", 4), os.path.join(music, "d fourth.mp3"))
    ff("-f", "lavfi", "-i", tone(220, -6), "-ac", "2", "-c:a", "libmp3lame", "-b:a", "192k", *meta("Encore", 1, "Live Band"), os.path.join(music2, "01 encore.mp3"))
    ff("-f", "lavfi", "-i", tone(880, -20), "-ac", "2", "-c:a", "pcm_s16le", *meta("Outro", 2, "Live Band"), os.path.join(music2, "02 outro.wav"))
    return {
        "music": music,
        "music2": music2,
        "titles": ["First", "Second", "Third", "Fourth"],
        "codecs": ["flac", "opus", "mp3", "mp3"],
        "titles2": ["Encore", "Outro"],
    }


def run_app(phase, script, cfg, env):
    e = dict(env, P2U_SELFTEST=os.path.join(ROOT, "tests", script), P2U_SELFTEST_CONFIG=json.dumps(cfg))
    r = subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 1280x800x24", APP], env=e, capture_output=True, text=True, timeout=900)
    lines = [l for l in r.stdout.splitlines() if l.startswith(("PASS", "FAIL", "INFO", "SELFTEST"))]
    info = {}
    for l in lines:
        if l.startswith(("PASS", "FAIL")):
            name = l[5:].split(" — ")[0]
            record(f"app {phase}: {name}", l.startswith("PASS"), l.split(" — ", 1)[1] if " — " in l else "")
        elif l.startswith("INFO "):
            k, v = l[5:].split("=", 1)
            info[k] = v
    record(f"app {phase}: the app reported and exited", r.returncode == 0 and any(l.startswith("SELFTEST") for l in lines),
           f"exit {r.returncode}; {r.stderr.strip().splitlines()[-1] if r.stderr.strip() else ''}")
    return info


def mtools_env(work):
    rc = os.path.join(work, "mtoolsrc")
    with open(rc, "w") as f:
        f.write("mtools_skip_check=1\n")
    return dict(os.environ, MTOOLSRC=rc)


def main():
    if not os.path.exists(APP):
        sys.exit(f"build the app first: {APP}")
    work = tempfile.mkdtemp(prefix="p2u-app-e2e-")
    cfg = make_fixtures(work)
    img = os.path.join(work, "stick.img")
    with open(img, "wb") as f:
        f.write(b"\xa5" * (64 << 20))  # old junk where the stick's contents were
    env = dict(os.environ, P2U_DATA_DIR=os.path.join(work, "data"), P2U_CACHE_DIR=os.path.join(work, "cache"),
               P2U_TEST_STICKS=img, P2U_NULL_AUDIO="1")
    env.pop("P2U_FFMPEG_DIR", None)  # use the bundled sidecar next to the debug build's sources

    info = run_app("phase 1", "app_selftest.js", cfg, env)
    names = json.loads(info.get("names", "[]"))

    # The image, read by tools that aren't ours.
    part = os.path.join(work, "part.img")
    with open(img, "rb") as src, open(part, "wb") as dst:
        src.seek(2048 * 512)
        shutil.copyfileobj(src, dst)
    fk = subprocess.run(["fsck.fat", "-n", "-v", part], capture_output=True, text=True)
    record("fsck.fat finds a clean file system", fk.returncode == 0, (fk.stdout.strip().splitlines() or [fk.stderr])[-1])
    menv = mtools_env(work)
    top = subprocess.run(["mdir", "-b", "-i", part, "::/"], capture_output=True, text=True, env=menv).stdout.splitlines()
    want_folders = [f[0] for f in names]
    record("the top level holds the two folders, in stick order", [os.path.basename(x.rstrip("/")) for x in top] == want_folders, f"{top} vs {want_folders}")
    out = os.path.join(work, "out")
    os.makedirs(out)
    all_ok_order = True
    files = []
    for folder, want in names:
        got = subprocess.run(["mdir", "-b", "-i", part, f"::/{folder}"], capture_output=True, text=True, env=menv).stdout.split("\n")
        got = [os.path.basename(x) for x in got if x.strip() and not x.rstrip().endswith((".", ".."))]
        if got != want:
            all_ok_order = False
            print("   order:", got, "vs", want)
        for n in want:
            dst = os.path.join(out, f"{folder}--{n}")
            subprocess.run(["mcopy", "-n", "-o", "-i", part, f"::/{folder}/{n}", dst], check=True, env=menv)
            files.append((folder, n, dst))
    record("each folder lists its tracks in playlist order (mtools, FAT order)", all_ok_order)

    probes = []
    for folder, n, path in files:
        j = json.loads(subprocess.run(["ffprobe", "-v", "error", "-print_format", "json", "-show_streams", "-show_format", path],
                                      capture_output=True, text=True).stdout)
        a = next(s for s in j["streams"] if s["codec_type"] == "audio")
        tags = {k.lower(): v for k, v in j["format"].get("tags", {}).items()}
        pic = any(s.get("disposition", {}).get("attached_pic") for s in j["streams"])
        probes.append((folder, n, path, a, tags, pic))
    record("every file is MP3 at 44.1 or 48 kHz, stereo", all(a["codec_name"] == "mp3" and a["sample_rate"] in ("44100", "48000") and a["channels"] == 2 for *_, a, _, _ in probes))
    tracks = [t.get("track") for *_, t, _ in probes]
    record("the track tag is the position in the playlist", tracks == ["1/4", "2/4", "3/4", "4/4", "1/2", "2/2"], str(tracks))
    record("titles come from the source tags", [t.get("title") for *_, t, _ in probes] == cfg["titles"] + cfg["titles2"])
    record("the 320 kbps MP3 kept its cover (as a small JPEG)", probes[2][5] and not probes[0][5])
    with open(probes[2][2], "rb") as f:
        head = f.read(200_000)
    record("ID3v2.3 tags, no ReplayGain", head.startswith(b"ID3\x03") and b"REPLAYGAIN" not in head)
    # Loudness: evened out to -18 LUFS (copies in 1.5 dB steps, encodes exact).
    louds = []
    for *_, path, _, _, _ in probes:
        r = subprocess.run(["ffmpeg", "-nostats", "-hide_banner", "-i", path, "-af", "ebur128", "-f", "null", "-"], capture_output=True, text=True)
        m = re.findall(r"I:\s+(-?[\d.]+) LUFS", r.stderr)
        louds.append(float(m[-1]) if m else 0.0)
    # The first playlist evens out the volume; the second one doesn't, so its
    # tracks keep their levels (about -28 and -41 LUFS as made).
    record("the first playlist's tracks measure -18 LUFS within 0.8 LU", all(abs(l + 18) <= 0.8 for l in louds[:4]), ", ".join(f"{l:.1f}" for l in louds[:4]))
    record("the second playlist's tracks keep their own levels", louds[4] < -25 and louds[5] < -35, ", ".join(f"{l:.1f}" for l in louds[4:]))

    run_app("phase 2", "app_selftest2.js", cfg, env)

    shutil.rmtree(work, ignore_errors=True)
    passed = sum(ok for _, ok in RESULTS)
    print(f"\n{passed}/{len(RESULTS)} checks passed")
    sys.exit(0 if passed == len(RESULTS) else 1)


if __name__ == "__main__":
    main()
