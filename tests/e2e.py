#!/usr/bin/env python3
"""End-to-end tests for p2u against real FAT images.

Needs: dosfstools (mkfs.fat, fsck.fat), mtools, fatsort (independent order check),
sfdisk (util-linux). Run from the repo root after `cargo build`:

    python3 tests/e2e.py [path/to/p2u]

Each scenario builds an image with mkfs.fat, fills a folder through mtools in a
deliberately scrambled way (out-of-order copies, deletions that leave 0xE5
holes, refills that land in those holes, interleaved writes that fragment the
directory's cluster chain), then runs `p2u sort` and checks the result with
tools that share no code with p2u:
  * fatsort -l   lists the raw directory order
  * fsck.fat -n  checks the file system is consistent
  * mcopy        reads every file back; contents must be byte-identical
"""
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import unicodedata

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
P2U = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "debug", "p2u")
RESULTS = []


def run(cmd, check=True, env=None, input=None):
    r = subprocess.run(cmd, capture_output=True, text=True, env=env, input=input)
    if check and r.returncode != 0:
        raise RuntimeError(f"{cmd!r} failed ({r.returncode}):\n{r.stdout}\n{r.stderr}")
    return r


def mtools_env(img, offset=0):
    env = dict(os.environ)
    rc = os.path.join(os.path.dirname(img), "mtoolsrc")
    with open(rc, "w") as f:
        off = f" offset={offset}" if offset else ""
        f.write(f'drive x: file="{img}"{off}\nmtools_skip_check=1\n')
    env["MTOOLSRC"] = rc
    env["MTOOLS_LOWER_CASE"] = "0"
    return env


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def fatsort_order(img, folder):
    """Names in raw order according to fatsort (independent implementation)."""
    out = run(["fatsort", "-l", img], check=False).stdout
    names, cur = [], None
    target = folder.rstrip("/") + "/"
    for line in out.splitlines():
        if line.startswith("/"):
            cur = line.strip()
            continue
        if cur == target and line.strip():
            names.append(line.strip())
    return names


def fsck_clean(img, offset=0):
    if offset:
        # fsck.fat has no offset option; check a copy of the partition.
        part = img + ".part"
        with open(img, "rb") as s, open(part, "wb") as d:
            s.seek(offset)
            shutil.copyfileobj(s, d)
        r = run(["fsck.fat", "-n", part], check=False)
        os.remove(part)
    else:
        r = run(["fsck.fat", "-n", img], check=False)
    return r.returncode == 0, (r.stdout + r.stderr).strip().splitlines()[-1:]


def record(name, ok, detail=""):
    RESULTS.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""))


def make_sources(work, n, rng):
    src = os.path.join(work, "music")
    os.makedirs(src)
    titles = [
        "Zebra Crossing", "Äpfel und Birnen", "a", "B side", "Café del Mar", "Night Drive",
        "日本の歌", "Long " + "x" * 120, "Mixed CASE Track", "Track with  two spaces",
        "Résumé", "Over The Hills", "Ünïcödé", "Bridge", "Coda", "Dawn", "Echo", "Fable",
        "Gale", "Harbor", "Isle", "Jetty", "Kite", "Lagoon", "Meadow", "Nimbus", "Orbit",
        "Prairie", "Quarry", "Ridge",
    ]
    files = []
    for i in range(n):
        t = titles[i % len(titles)] + ("" if i < len(titles) else f" {i}")
        if i == 4:
            t = unicodedata.normalize("NFD", t)  # decomposed, as macOS may supply
        p = os.path.join(src, t + ".mp3")
        with open(p, "wb") as f:
            f.write(rng.randbytes(rng.randint(3_000, 40_000)))
        files.append(p)
    order = files[:]
    rng.shuffle(order)
    return order


def write_playlists(work, order):
    m3u = os.path.join(work, "Road Trip.m3u8")
    with open(m3u, "w", encoding="utf-8") as f:
        f.write("#EXTM3U\n")
        for p in order:
            f.write(f"#EXTINF:-1,{os.path.basename(p)[:-4]}\n")
            f.write(os.path.relpath(p, work) + "\n")
    from urllib.parse import quote
    xspf = os.path.join(work, "Road Trip.xspf")
    with open(xspf, "w", encoding="utf-8") as f:
        f.write('<?xml version="1.0" encoding="UTF-8"?>\n<playlist xmlns="http://xspf.org/ns/0/" version="1"><trackList>\n')
        for p in order:
            f.write(f"<track><location>file://{quote(p)}</location></track>\n")
        f.write("</trackList></playlist>\n")
    return m3u, xspf


def plan_names(playlist):
    r = run([P2U, "plan", playlist, "--dest", "/", "--json"])
    plan = json.loads(r.stdout)
    return plan, [i["target_name"] for i in plan["items"]]


def scramble_copy(env, plan, folder, rng, fragment=True):
    """Copy into the image through mtools in a messy order with deletions."""
    run(["mmd", f"x:/{folder}"], env=env)
    items = plan["items"][:]
    rng.shuffle(items)
    half = len(items) // 2
    # First pass: copy some, plus decoys that get deleted later (leaving holes).
    for k, it in enumerate(items[:half]):
        run(["mcopy", "-o", it["source"], f"x:/{folder}/{it['target_name']}"], env=env)
        if k % 3 == 0:
            run(["mcopy", "-o", it["source"], f"x:/{folder}/decoy {k} with a long name.tmp"], env=env)
        if fragment and k % 4 == 0:
            # Writes elsewhere between directory growth fragment the dir's chain.
            run(["mcopy", "-o", it["source"], f"x:/filler{k}.bin"], env=env)
    for k in range(0, half, 3):
        run(["mdel", f"x:/{folder}/decoy {k} with a long name.tmp"], env=env)
    # Second pass fills the holes first, scrambling order further.
    for it in items[half:]:
        run(["mcopy", "-o", it["source"], f"x:/{folder}/{it['target_name']}"], env=env)


def check_contents(env, plan, folder, work):
    out = os.path.join(work, "readback")
    os.makedirs(out, exist_ok=True)
    for it in plan["items"]:
        dst = os.path.join(out, "f")
        run(["mcopy", "-o", "-n", f"x:/{folder}/{it['target_name']}", dst], env=env)
        if sha(dst) != sha(it["source"]):
            return False, it["target_name"]
    return True, ""


def scenario(label, fat_bits, size_mb, n, sectors_per_cluster=None, partitioned=False, playlist_kind="m3u"):
    rng = random.Random(fat_bits * 1000 + n)
    work = tempfile.mkdtemp(prefix=f"p2u-{label}-")
    try:
        order = make_sources(work, n, rng)
        m3u, xspf = write_playlists(work, order)
        playlist = m3u if playlist_kind == "m3u" else xspf
        plan, names = plan_names(playlist)
        assert len(names) == n, f"plan has {len(names)} items"
        img = os.path.join(work, "stick.img")
        offset = 0
        with open(img, "wb") as f:
            f.truncate(size_mb * 1024 * 1024)
        mk = ["mkfs.fat", "-F", str(fat_bits), "-n", "P2UTEST"]
        if sectors_per_cluster:
            mk += ["-s", str(sectors_per_cluster)]
        if partitioned:
            run(["sfdisk", "--quiet", img], input="label: dos\nstart=2048, type=c\n")
            offset = 2048 * 512
            part_sectors = (size_mb * 1024 * 1024 - offset) // 512
            mk += ["--offset", "2048", img, str(part_sectors // 2)]
        else:
            mk += [img]
        run(mk)
        env = mtools_env(img, offset)
        folder = "Road Trip"
        scramble_copy(env, plan, folder, rng, fragment=True)

        before = fatsort_order(img, f"/{folder}") if not partitioned else None
        img_before = os.path.join(work, "before.img")
        shutil.copyfile(img, img_before)

        if before is not None:
            record(f"{label}: scramble produced a wrong order", before[: len(names)] != names)

        # Dry run must not change a byte.
        r = run([P2U, "sort", img, "--playlist", playlist, "--dry-run", "--backup-dir", work])
        record(f"{label}: dry run leaves image untouched", sha(img) == sha(img_before))

        r = run([P2U, "sort", img, "--playlist", playlist, "--backup-dir", work], check=False)
        out = json.loads(r.stdout)
        record(f"{label}: p2u sort reports success", r.returncode == 0 and out["ok"], out.get("error") or "")
        rep = out.get("report") or {}

        # Independent checks.
        if not partitioned:
            after = fatsort_order(img, f"/{folder}")
            files_after = [x for x in after if not x.lower().startswith("decoy")]
            record(f"{label}: fatsort -l sees playlist order", files_after[: len(names)] == names,
                   f"first {files_after[:2]}")
        ok, tail = fsck_clean(img, offset)
        record(f"{label}: fsck.fat -n clean after sort", ok, " ".join(tail))
        ok, bad = check_contents(env, plan, folder, work)
        record(f"{label}: every file reads back byte-identical", ok, bad)

        r = run([P2U, "verify", img, "--playlist", playlist], check=False)
        record(f"{label}: p2u verify (raw) passes", r.returncode == 0)

        r = run([P2U, "sort", img, "--playlist", playlist, "--backup-dir", work], check=False)
        again = json.loads(r.stdout)
        record(f"{label}: second sort is a no-op", again["ok"] and not again["report"]["changed"])

        # Undo restores the exact bytes.
        if out.get("backup_file"):
            run([P2U, "restore", img, out["backup_file"]])
            record(f"{label}: restore returns image to pre-sort bytes", sha(img) == sha(img_before))
        return rep
    finally:
        shutil.rmtree(work, ignore_errors=True)


def safety_tests():
    work = tempfile.mkdtemp(prefix="p2u-safety-")
    try:
        img = os.path.join(work, "s.img")
        with open(img, "wb") as f:
            f.truncate(48 * 1024 * 1024)
        run(["mkfs.fat", "-F", "32", "-s", "1", img])
        env = mtools_env(img)
        run(["mmd", "x:/A"], env=env)
        for n in ["Track One.mp3", "Track Two.mp3", "Track Three.mp3"]:
            p = os.path.join(work, n)
            open(p, "wb").write(b"x" * 1000)
            run(["mcopy", p, f"x:/A/{n}"], env=env)
        order = os.path.join(work, "order.txt")
        open(order, "w").write("Track Three.mp3\nTrack Two.mp3\nTrack One.mp3\n")

        # 1. A volume flagged as mounted (Linux dirty bit at 0x41) is refused.
        data = bytearray(open(img, "rb").read(512))
        data[0x41] |= 1
        with open(img, "r+b") as f:
            f.write(data)
        h = sha(img)
        r = run([P2U, "sort", img, "--dir", "/A", "--order-file", order, "--backup-dir", work], check=False)
        record("safety: refuses a volume marked in use", r.returncode != 0 and sha(img) == h,
               (json.loads(r.stdout).get("error") or "")[:60])
        data[0x41] &= 0xFE
        with open(img, "r+b") as f:
            f.write(data)

        # 2. Orphaned long-name entries are refused (corrupt the first LFN checksum in /A).
        ls = json.loads(run([P2U, "ls", img, "--dir", "/A", "--json"]).stdout)
        geo = json.loads(run([P2U, "inspect", img]).stdout)["geometry"]
        cl = [g for g in json.loads(run([P2U, "ls", img, "--json"]).stdout)["groups"] if g["long_name"] == "A" or g["short_name"] == "A"][0]["first_cluster"]
        off = (geo["first_data_sector"] + (cl - 2) * geo["sectors_per_cluster"]) * geo["bytes_per_sector"]
        first_lfn_slot = [g for g in ls["groups"] if g["kind"] == "File" and g["long_name"]][0]["first_slot"]
        with open(img, "r+b") as f:
            f.seek(off + first_lfn_slot * 32 + 13)
            b = f.read(1)
            f.seek(off + first_lfn_slot * 32 + 13)
            f.write(bytes([b[0] ^ 0xFF]))
        h = sha(img)
        r = run([P2U, "sort", img, "--dir", "/A", "--order-file", order, "--backup-dir", work], check=False)
        record("safety: refuses a directory with orphaned LFN entries", r.returncode != 0 and sha(img) == h)

        # 3. The FAT and FSInfo are never written: only directory clusters change.
        with open(img, "r+b") as f:  # undo corruption
            f.seek(off + first_lfn_slot * 32 + 13)
            f.write(b)
        before = open(img, "rb").read()
        r = run([P2U, "sort", img, "--dir", "/A", "--order-file", order, "--backup-dir", work], check=False)
        after = open(img, "rb").read()
        changed = [i for i in range(0, len(before), 512) if before[i:i + 512] != after[i:i + 512]]
        cs = geo["bytes_per_sector"] * geo["sectors_per_cluster"]
        dir_lo, dir_hi = off, off + cs
        record("safety: only the folder's directory sectors changed",
               r.returncode == 0 and changed and all(dir_lo <= c < dir_hi for c in changed),
               f"{len(changed)} sector(s) changed")
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    if not os.path.exists(P2U):
        sys.exit(f"build first: {P2U} not found")
    reps = {}
    reps["FAT32 small clusters"] = scenario("FAT32 512B clusters, 60 tracks", 32, 64, 60, sectors_per_cluster=1)
    reps["FAT32 xspf"] = scenario("FAT32 4K clusters, XSPF playlist", 32, 300, 30, sectors_per_cluster=8, playlist_kind="xspf")
    reps["FAT16"] = scenario("FAT16, 40 tracks", 16, 32, 40)
    reps["FAT12"] = scenario("FAT12, 12 tracks", 12, 4, 12)
    reps["FAT32 MBR"] = scenario("FAT32 inside an MBR partition", 32, 80, 25, sectors_per_cluster=1, partitioned=True)
    safety_tests()
    passed = sum(1 for _, ok, _ in RESULTS if ok)
    print(f"\n{passed}/{len(RESULTS)} checks passed")
    summary = {
        "checks": [{"name": n, "ok": ok, "detail": d} for n, ok, d in RESULTS],
        "reorder_reports": {k: {kk: v.get(kk) for kk in ("deleted_slots_reclaimed", "slots_used", "capacity_slots", "changed")} for k, v in reps.items() if v},
    }
    with open(os.path.join(ROOT, "tests", "e2e-results.json"), "w") as f:
        json.dump(summary, f, indent=2, ensure_ascii=False)
    sys.exit(0 if passed == len(RESULTS) else 1)


if __name__ == "__main__":
    main()
