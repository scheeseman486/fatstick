#!/usr/bin/env python3
"""Stands in for yt-dlp in the tests, so they don't depend on YouTube.

Answers the way the real one does, for what the app asks: --version, a
`ytsearchN:` search, one video's description, and a download, which makes a real
audio file with ffmpeg in the format the --format selector asks for. It only
answers a download whose selector names audio-only formats, as the real one would
pick whatever else was asked for. Every command line is logged, as one JSON line,
to the file in FAKE_YTDLP_LOG, for the tests to look at.

Some video ids behave differently:

    noaudiostrm  has no audio-only stream, so no format is available
    blockedsong  refused, as an out-of-date yt-dlp is
    hasvideo000  sends a file with a video stream in it, whatever was asked for
    slowsongslo  takes a long time, for cancelling
"""
import json
import os
import re
import subprocess
import sys
import time

CATALOGUE = [
    ("aaaaaaaaaa1", "The Band – First Song (Official Video)", "The Band VEVO", 4),
    ("aaaaaaaaaa2", "Second Song (Lyrics)", "Some Channel", 5),
    ("aaaaaaaaaa3", "Third Song", "The Band - Topic", 3),
    ("noaudiostrm", "No Audio Stream", "Some Channel", 3),
    ("blockedsong", "Blocked Song", "Some Channel", 3),
    ("hasvideo000", "Has Video", "Some Channel", 3),
    ("slowsongslo", "Slow Song", "Some Channel", 3),
]
BY_ID = {c[0]: c for c in CATALOGUE}


def describe(video):
    vid, title, channel, secs = video
    d = {"_type": "url", "id": vid, "title": title, "channel": channel, "uploader": channel, "duration": secs, "view_count": 12345}
    if vid == "aaaaaaaaaa3":
        d.update({"track": "Third Song Proper", "artists": ["The Band"], "album": "The Album", "release_year": 1975})
    return d


def opt(argv, *names):
    for i, a in enumerate(argv):
        if a in names and i + 1 < len(argv):
            return argv[i + 1]
    return None


def fail(message):
    print(f"ERROR: [youtube] {message}", file=sys.stderr)
    sys.exit(1)


def make_audio(path, codec, secs, video=False):
    src = ["-f", "lavfi", "-i", f"sine=frequency=440:duration={secs}"]
    if video:
        src += ["-f", "lavfi", "-i", f"color=c=black:s=64x64:d={secs}"]
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *src, "-ac", "2", "-c:a", codec, *(["-c:v", "libvpx-vp9"] if video else []), *(["-f", "webm"] if path.endswith(".webm") else ["-f", "mp4", "-movflags", "empty_moov+default_base_moof+frag_keyframe"]), path], check=True)


def main():
    argv = sys.argv[1:]
    log = os.environ.get("FAKE_YTDLP_LOG")
    if log:
        with open(log, "a", encoding="utf-8") as f:
            f.write(json.dumps(argv) + "\n")
    if argv == ["--version"]:
        print("2026.10.26")
        return
    if "--" not in argv:
        fail("no URL")
    target = argv[argv.index("--") + 1]

    m = re.fullmatch(r"ytsearch(\d+):(.*)", target, re.S)
    if m:
        hits = [describe(v) for v in CATALOGUE][: int(m.group(1))]
        if m.group(2) == "nothing":
            hits = []
        print(json.dumps({"_type": "playlist", "id": m.group(2), "entries": hits}))
        return

    m = re.fullmatch(r"https://www\.youtube\.com/watch\?v=([\w-]{11})", target)
    if not m or m.group(1) not in BY_ID:
        fail(f"{target}: Video unavailable")
    video = BY_ID[m.group(1)]
    if "--dump-single-json" in argv:
        print(json.dumps(describe(video)))
        return

    chosen = opt(argv, "-f", "--format") or "best"
    if any(not alt.startswith("bestaudio") for alt in chosen.split("/")):
        fail(f"{video[0]}: this stand-in only fetches audio-only formats, not {chosen}")
    vid = video[0]
    if vid == "noaudiostrm":
        fail(f"{vid}: Requested format is not available. Use --list-formats for a list of available formats")
    if vid == "blockedsong":
        fail(f"{vid}: unable to download video data: HTTP Error 403: Forbidden")
    ext = "webm" if chosen.startswith("bestaudio[acodec^=opus]") else "m4a"
    print(json.dumps({**describe(video), "ext": ext, "acodec": "opus" if ext == "webm" else "mp4a.40.2", "vcodec": "none"}), flush=True)
    out = opt(argv, "-o", "--output").replace("%(ext)s", ext)
    total = 400000
    for done in range(0, total, 100000):
        print(f"fatstick {done} {total} NA", flush=True)
        time.sleep(1.0 if vid == "slowsongslo" else 0.05)
    if vid == "slowsongslo":
        time.sleep(60)
    make_audio(out, "libopus" if ext == "webm" else "aac", video[3], video=vid == "hasvideo000")
    print(f"fatstick {total} {total} NA", flush=True)


main()
