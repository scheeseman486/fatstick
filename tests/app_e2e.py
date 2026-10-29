#!/usr/bin/env python3
"""The real app, end to end, on a disk image standing in for a USB stick.

Makes real audio (FLAC, Opus, MP3s at 320 kbps and at 22.05 kHz, WAV), then
runs a debug build of the app three times under Xvfb with FATSTICK_SELFTEST:

1. tests/app_selftest.js drives the window from an empty library to a
   finished sync of two playlists, with silence between them, the first
   track cut and padded and the second playlist joined into one track (see
   that file), then shows the stick's view.
2. tests/app_selftest2.js restarts on the same library and checks that
   everything was kept.
3. tests/app_selftest3.js chooses the music folder (phase 1's folders and
   a few more files), scans and searches it, writes tags into two files and
   renames them through the library view, and sees a file copied into the
   folder show up by itself. ffprobe reads the tags back.

Then it reads the image with tools that aren't ours: fsck.fat, mtools (the
order a player sees), ffprobe (codec, bitrate, tags, cover) and ebur128
(loudness evened out to -18 LUFS).

    cargo build --manifest-path app/src-tauri/Cargo.toml   # debug build
    packaging/ffmpeg/build-ffmpeg.sh x86_64-unknown-linux-gnu   # the bundled ffmpeg
    python3 tests/app_e2e.py

Needs ffmpeg and ffprobe on the PATH (for fixtures and checks), xvfb-run,
mtools and dosfstools.
"""
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "app", "src-tauri", "target", "debug", "fatstick")
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
    # The rest of the music folder, for the library (phase 3).
    owls = os.path.join(work, "Music", "Night Owls", "Lanterns")
    unsorted = os.path.join(work, "Music", "Unsorted")
    os.makedirs(owls)
    os.makedirs(unsorted)
    lib_meta = lambda title, n, grouping: ["-metadata", f"title={title}", "-metadata", "artist=Night Owls", "-metadata", "album=Lanterns",
                                           "-metadata", f"track={n}", "-metadata", "genre=Folk", "-metadata", f"grouping={grouping}"]
    ff("-f", "lavfi", "-i", tone(392, -8, 5), "-ac", "2", "-c:a", "libmp3lame", "-b:a", "192k", "-id3v2_version", "3",
       *lib_meta("Lanterns", 1, "chill; road trip"), os.path.join(owls, "lanterns.mp3"))
    ff("-f", "lavfi", "-i", tone(294, -8, 5), "-ac", "2", "-c:a", "flac", *lib_meta("Harbour Song", 2, "chill"), os.path.join(owls, "harbour.flac"))
    ff("-f", "lavfi", "-i", tone(523, -8, 5), "-ac", "2", "-c:a", "libvorbis", "-q:a", "4", os.path.join(unsorted, "track07.ogg"))
    ff("-f", "lavfi", "-i", tone(349, -8, 4), "-ac", "2", "-c:a", "libmp3lame", "-b:a", "128k", "-metadata", "title=New Arrival",
       "-metadata", "artist=Night Owls", os.path.join(work, "new-arrival.mp3"))
    return {
        "root": os.path.join(work, "Music"),
        "owls": owls,
        "libraryCount": 9,
        "arrival": os.path.join(work, "new-arrival.mp3"),
        "arrivalTo": os.path.join(unsorted, "new arrival.mp3"),
        "untouched": [os.path.join(music, f) for f in sorted(os.listdir(music))],
        "music": music,
        "music2": music2,
        "titles": ["First", "Second", "Third", "Fourth"],
        "codecs": ["flac", "opus", "mp3", "mp3"],
        "titles2": ["Encore", "Outro"],
    }


def run_app(phase, script, cfg, env):
    # A script that doesn't parse never reports, and the app would wait for it.
    if shutil.which("node"):
        chk = subprocess.run(["node", "--check", os.path.join(ROOT, "tests", script)], capture_output=True, text=True)
        if chk.returncode != 0:
            record(f"app {phase}: {script} parses", False, chk.stderr.strip().splitlines()[-1] if chk.stderr.strip() else "")
            return {}
    e = dict(env, FATSTICK_SELFTEST=os.path.join(ROOT, "tests", script), FATSTICK_SELFTEST_CONFIG=json.dumps(cfg))
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
    if os.environ.get("FATSTICK_E2E_LOG"):
        with open(os.environ["FATSTICK_E2E_LOG"] + f".{phase.replace(' ', '')}", "w") as f:
            f.write(r.stdout + "\n--- stderr ---\n" + r.stderr)
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
    work = tempfile.mkdtemp(prefix="fatstick-app-e2e-")
    cfg = make_fixtures(work)
    img = os.path.join(work, "stick.img")
    with open(img, "wb") as f:
        f.write(b"\xa5" * (64 << 20))  # old junk where the stick's contents were
    # A second "device", which phase 1 excludes: it must never be written.
    spare = os.path.join(work, "spare.img")
    with open(spare, "wb") as f:
        f.write(b"\x5a" * (16 << 20))
    env = dict(os.environ, FATSTICK_DATA_DIR=os.path.join(work, "data"), FATSTICK_CACHE_DIR=os.path.join(work, "cache"),
               FATSTICK_TEST_STICKS=os.pathsep.join([img, spare]), FATSTICK_NULL_AUDIO="1")
    env.pop("FATSTICK_FFMPEG_DIR", None)  # use the bundled sidecar next to the debug build's sources

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
    record("the top level holds the three folders, in stick order", [os.path.basename(x.rstrip("/")) for x in top] == want_folders and len(want_folders) == 3, f"{top} vs {want_folders}")
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
    record("the track tag is the position in the folder", tracks == ["1/4", "2/4", "3/4", "4/4", "1/1", "1/1"], str(tracks))
    titles = [t.get("title") for *_, t, _ in probes]
    record("titles come from the source tags (and the joined track is named after its playlist)", titles == cfg["titles"] + ["Silence", "Live"], str(titles))
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
    # The first playlist evens out the volume (the padding's silence doesn't
    # count: the meter gates it out).
    # The fourth track fades in and out, so it's measured between its fades.
    r = subprocess.run(["ffmpeg", "-nostats", "-hide_banner", "-ss", "2.2", "-t", "2.6", "-i", probes[3][2], "-af", "ebur128", "-f", "null", "-"], capture_output=True, text=True)
    m = re.findall(r"I:\s+(-?[\d.]+) LUFS", r.stderr)
    louds[3] = float(m[-1]) if m else 0.0
    record("the first playlist's tracks measure -18 LUFS within 0.8 LU", all(abs(l + 18) <= 0.8 for l in louds[:4]), ", ".join(f"{l:.1f}" for l in louds[:4]))

    # Lengths and silences, measured with ffprobe and ffmpeg's volumedetect.
    def length(path):
        return float(json.loads(subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "json", path], capture_output=True, text=True).stdout)["format"]["duration"])

    def peak(path, start=0.0, dur=None):
        args = ["ffmpeg", "-hide_banner", "-nostats", "-ss", str(start)] + (["-t", str(dur)] if dur else []) + ["-i", path, "-af", "volumedetect", "-f", "null", "-"]
        m = re.search(r"max_volume: (-?[\d.]+|-inf) dB", subprocess.run(args, capture_output=True, text=True).stderr)
        return -200.0 if not m or m.group(1) == "-inf" else float(m.group(1))

    first, gap, joined = probes[0][2], probes[4][2], probes[5][2]
    lens = [length(x) for x in (first, gap, joined)]
    record("the first track: 2 s cut, 1 s of silence added (8 s became 7 s)", abs(lens[0] - 7.0) < 0.12, f"{lens[0]:.2f} s")
    record("it ends in a second of silence", peak(first, 6.15) < -70 and peak(first, 5.0, 0.8) > -40, f"{peak(first, 6.15):.0f} dB after 6.15 s")
    record("the silence between the playlists: 3 s, silent", abs(lens[1] - 3.0) < 0.12 and peak(gap) < -80, f"{lens[1]:.2f} s, peak {peak(gap):.0f} dB")
    record("the second playlist joined into one track of 16 s, both tracks audible", abs(lens[2] - 16.0) < 0.15 and peak(joined, 1, 6) > -30 and peak(joined, 9, 6) > -45,
           f"{lens[2]:.2f} s; {peak(joined, 1, 6):.0f} and {peak(joined, 9, 6):.0f} dB")
    def freq(path, start, dur):
        raw = subprocess.run(["ffmpeg", "-v", "error", "-ss", str(start), "-t", str(dur), "-i", path, "-ac", "1", "-ar", "44100", "-f", "f32le", "-"],
                             capture_output=True).stdout
        x = struct.unpack(f"<{len(raw) // 4}f", raw)
        ups = sum(1 for i in range(1, len(x)) if x[i - 1] < 0 <= x[i])
        return ups / (len(x) / 44100) if x else 0.0

    second = probes[1][2]
    f2, l2 = freq(second, 2, 4), length(second)
    record("the second track: 5 semitones up (550 Hz became 734 Hz), still 8 s", abs(f2 - 550 * 2 ** (5 / 12)) < 6 and abs(l2 - 8.0) < 0.12, f"{f2:.1f} Hz, {l2:.2f} s")
    fourth = probes[3][2]
    full, l4 = peak(fourth, 3.0, 1.0), length(fourth)
    # The smooth S curve is half way (-6 dB) at the middle of the fade in, 1 s in; the
    # late fade out holds up, sin(45°) (-3 dB) at its middle, 1.5 s before the end.
    pin, pmid_in, pmid_out, pout = peak(fourth, 0, 0.15), peak(fourth, 0.95, 0.1), peak(fourth, 6.45, 0.1), peak(fourth, 7.85)
    record("the fourth track fades in over 2 s and out over 3 s (late), still 8 s",
           abs(l4 - 8.0) < 0.12 and pin < full - 18 and abs(pmid_in - (full - 6)) < 1.5 and abs(pmid_out - (full - 3)) < 1.2 and pout < full - 18,
           f"{l4:.2f} s; {pin:.0f}, {pmid_in:.1f}, {full:.1f}, {pmid_out:.1f}, {pout:.0f} dB")
    record("the joined track takes the playlist's name and the shared artist", probes[5][4].get("album") == "Live" and probes[5][4].get("artist") == "Live Band", str(probes[5][4]))

    run_app("phase 2", "app_selftest2.js", cfg, env)

    # Phase 3: the music library. Once the app has renamed its two files, a
    # new file is copied into the folder; the app should notice by itself.
    def sums(paths):
        out = []
        for p in paths:
            with open(p, "rb") as f:
                out.append(hashlib.sha256(f.read()).hexdigest())
        return out
    untouched = sums(cfg["untouched"])
    stop = threading.Event()

    def drop_in():
        renamed = os.path.join(cfg["owls"], "01 - Lanterns.mp3")
        while not stop.is_set():
            if os.path.exists(renamed):
                time.sleep(1)
                shutil.copy(cfg["arrival"], cfg["arrivalTo"])
                return
            time.sleep(0.25)
    threading.Thread(target=drop_in, daemon=True).start()
    info3 = run_app("phase 3", "app_selftest3.js", cfg, env)
    stop.set()
    tagged = json.loads(info3.get("tagged", "[]"))
    record("the renamed files are on disk, and the old names are gone",
           sorted(os.listdir(cfg["owls"])) == ["01 - Lanterns.mp3", "02 - Harbour Song.flac"], str(sorted(os.listdir(cfg["owls"]))))
    for path in tagged:
        j = json.loads(subprocess.run(["ffprobe", "-v", "error", "-print_format", "json", "-show_format", path], capture_output=True, text=True).stdout or "{}")
        tags = {k.lower(): v for k, v in j.get("format", {}).get("tags", {}).items()}
        grouping = tags.get("grouping") or tags.get("tit1") or tags.get("contentgroup") or ""
        name = os.path.basename(path)
        record(f"{name}: the new genre is in the file (ffprobe)", tags.get("genre") == "Night Folk", str(tags))
        record(f"{name}: the labels are in its Grouping tag", grouping == "chill; kitchen", grouping)
        record(f"{name}: the other tags are kept", tags.get("title") in ("Lanterns", "Harbour Song") and tags.get("artist") == "Night Owls" and tags.get("album") == "Lanterns")
    record("files the library didn't edit are byte for byte the same", sums(cfg["untouched"]) == untouched)
    with open(spare, "rb") as f:
        record("the excluded image was never written", f.read() == b"\x5a" * (16 << 20))

    shutil.rmtree(work, ignore_errors=True)
    passed = sum(ok for _, ok in RESULTS)
    print(f"\n{passed}/{len(RESULTS)} checks passed")
    sys.exit(0 if passed == len(RESULTS) else 1)


if __name__ == "__main__":
    main()
