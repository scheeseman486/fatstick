#!/usr/bin/env python3
"""End-to-end tests of `p2u write` (erase the stick, write a fresh FAT32) on
disk images that start out with awkward layouts, checked with tools that share
no code with p2u: sfdisk, blkid, wipefs, fsck.fat, fatsort and mtools.

    python3 tests/stick_e2e.py [path/to/p2u]

Needs: dosfstools, mtools, fatsort, fdisk (sfdisk), util-linux (blkid, wipefs),
gdisk (sgdisk), exfatprogs, ntfs-3g, e2fsprogs.
"""
import hashlib
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
P2U = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "debug", "p2u")
MIB = 1 << 20
GIB = 1 << 30
RESULTS = []
PULLS = 0


def record(name, ok, detail=""):
    RESULTS.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)


def run(cmd, check=True, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if check and r.returncode != 0:
        raise RuntimeError(f"{cmd[0]} failed: {r.stderr.strip()[:300]}")
    return r


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(4 * MIB)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def sparse_image(path, size):
    with open(path, "wb") as f:
        f.truncate(size)


def put(img, offset, data):
    with open(img, "r+b") as f:
        f.seek(offset)
        f.write(data)


def copy_region(src, dst, offset, length):
    """Copy [offset, offset+length) of src into a new sparse file, skipping holes."""
    with open(src, "rb") as fi, open(dst, "wb") as fo:
        fo.truncate(length)
        pos = offset
        end = offset + length
        while pos < end:
            try:
                data_at = os.lseek(fi.fileno(), pos, os.SEEK_DATA)
            except OSError:
                break
            if data_at >= end:
                break
            hole_at = min(os.lseek(fi.fileno(), data_at, os.SEEK_HOLE), end)
            fi.seek(data_at)
            fo.seek(data_at - offset)
            left = hole_at - data_at
            while left:
                b = fi.read(min(left, 8 * MIB))
                fo.write(b)
                left -= len(b)
            pos = hole_at


def mtools_env(work):
    rc = os.path.join(work, "mtoolsrc")
    with open(rc, "w") as f:
        f.write("mtools_skip_check=1\n")
    return dict(os.environ, MTOOLSRC=rc, LC_ALL="C.UTF-8")


# ---------------------------------------------------------------------------
# Prior layouts: what might be on a stick before it is written.

def fill_fat(part, env, rng, n=40):
    blob = os.path.join(os.path.dirname(part), "blob.bin")
    for i in range(n):
        with open(blob, "wb") as f:
            f.write(rng.randbytes(rng.randint(1000, 40000)))
        run(["mcopy", "-i", part, "-o", blob, f"::old file {i:02d}.mp3"], env=env)
    for i in range(0, n, 3):
        run(["mdel", "-i", part, f"::old file {i:02d}.mp3"], env=env)


def make_partition(img, start, size, builder):
    """Build a file system in a scratch file, then place it at `start` in img."""
    part = img + ".part"
    sparse_image(part, size)
    builder(part)
    with open(part, "rb") as fi, open(img, "r+b") as fo:
        pos = 0
        while pos < size:
            try:
                d = os.lseek(fi.fileno(), pos, os.SEEK_DATA)
            except OSError:
                break
            h = os.lseek(fi.fileno(), d, os.SEEK_HOLE)
            fi.seek(d)
            fo.seek(start + d)
            left = h - d
            while left:
                b = fi.read(min(left, 8 * MIB))
                fo.write(b)
                left -= len(b)
            pos = h
    os.remove(part)


def layout_blank(img, size, env, rng):
    sparse_image(img, size)


def layout_junk(img, size, env, rng):
    sparse_image(img, size)
    put(img, 0, rng.randbytes(2 * MIB))
    put(img, size - 2 * MIB, rng.randbytes(2 * MIB))
    # An ISO 9660 descriptor at 32 KiB and an ext superblock magic at 1 KiB + 56.
    put(img, 32768 + 1, b"CD001")
    put(img, 1024 + 56, b"\x53\xef")


def layout_fat32_used(img, size, env, rng):
    sparse_image(img, size)
    run(["sfdisk", "-q", img], input="start=2048, type=c\n")
    def build(p):
        run(["mkfs.fat", "-F", "32", "-n", "OLDSTICK", p])
        fill_fat(p, env, rng)
    make_partition(img, 2048 * 512, size - 2048 * 512, build)


def layout_superfloppy_fat16(img, size, env, rng):
    sparse_image(img, size)
    run(["mkfs.fat", "-F", "16", "-n", "FLOPPY", img])
    fill_fat(img, env, rng, 20)


def layout_gpt_exfat(img, size, env, rng):
    sparse_image(img, size)
    run(["sgdisk", "-o", "-n", "1:2048:0", "-t", "1:0700", img])
    end = int(re.search(r"First sector: (\d+).*?Last sector: (\d+)", run(["sgdisk", "-i", "1", img]).stdout, re.S).group(2))
    make_partition(img, 2048 * 512, (end - 2048 + 1) * 512, lambda p: run(["mkfs.exfat", "-q", "-L", "PHOTOS", p]))


def layout_mbr_ntfs_ext4(img, size, env, rng):
    sparse_image(img, size)
    half = (size // 512 - 2048) // 2
    run(["sfdisk", "-q", img], input=f"start=8192, size={half}, type=7\ntype=83\n")
    parts = json.loads(run(["sfdisk", "-J", img]).stdout)["partitiontable"]["partitions"]
    p1, p2 = parts
    make_partition(img, p1["start"] * 512, p1["size"] * 512, lambda p: run(["mkfs.ntfs", "-q", "-F", "-Q", "-L", "WINDATA", p]))
    make_partition(img, p2["start"] * 512, p2["size"] * 512, lambda p: run(["mkfs.ext4", "-q", "-F", "-L", "linuxdata", p]))


def layout_ext4_whole(img, size, env, rng):
    sparse_image(img, size)
    run(["mkfs.ext4", "-q", "-F", "-L", "wholedisk", img])


LAYOUTS = {
    "blank": layout_blank,
    "junk": layout_junk,
    "fat32_used": layout_fat32_used,
    "superfloppy_fat16": layout_superfloppy_fat16,
    "gpt_exfat": layout_gpt_exfat,
    "mbr_ntfs_ext4": layout_mbr_ntfs_ext4,
    "ext4_whole": layout_ext4_whole,
}

# (label, layout, size, cluster bytes expected, folder or top level)
CASES = [
    ("64 MiB blank", "blank", 64 * MIB, 512, "folder"),
    ("300 MiB of junk with stale signatures", "junk", 300 * MIB, 4096, "folder"),
    ("1 GiB used FAT32 stick", "fat32_used", GIB, 4096, "folder"),
    ("256 MiB superfloppy FAT16", "superfloppy_fat16", 256 * MIB, 512, "top"),
    ("8 GiB GPT + exFAT", "gpt_exfat", 8 * GIB, 4096, "folder"),
    ("16.5 GiB MBR with NTFS and ext4", "mbr_ntfs_ext4", 16 * GIB + GIB // 2, 16384, "folder"),
    ("40 GiB whole-disk ext4", "ext4_whole", 40 * GIB, 32768, "top"),
]


# ---------------------------------------------------------------------------

def make_music(work, rng, n, tag):
    music = os.path.join(work, "music-" + tag)
    os.makedirs(music, exist_ok=True)
    names = []
    for i in range(n):
        name = f"{tag} {i:02d}{' with a long title' * (i % 3)}.mp3"
        size = [0, 512, 4096, 32768][i] if i < 4 else rng.randint(20_000, 400_000)
        with open(os.path.join(music, name), "wb") as f:
            f.write(rng.randbytes(size))
        names.append(name)
    order = names[:]
    rng.shuffle(order)
    pl = os.path.join(work, f"{tag}.m3u8")
    with open(pl, "w") as f:
        f.write("".join(f"music-{tag}/{n}\n" for n in order))
    return pl, order, music


def write(img, playlist, *extra, env_extra=None):
    env = dict(os.environ, **(env_extra or {}))
    r = run([P2U, "write", img, "--playlist", playlist, "--yes", "--json", *extra], check=False, env=env)
    try:
        out = json.loads(r.stdout)
    except ValueError:
        out = {"ok": False, "error": (r.stderr or r.stdout).strip()[:300]}
    out["_code"] = r.returncode
    return out


def blkid(img, offset=None):
    cmd = ["blkid", "-p", "-o", "export"] + (["-O", str(offset)] if offset else []) + [img]
    r = run(cmd, check=False)
    kv = dict(l.split("=", 1) for l in r.stdout.splitlines() if "=" in l)
    return r.returncode, kv, r.stderr.strip()


def check_case(work, env, rng, label, layout, size, cluster, where):
    img = os.path.join(work, layout + ".img")
    LAYOUTS[layout](img, size, env, rng)
    tag = re.sub(r"\W", "", layout)[:8].capitalize()
    pl, order, music = make_music(work, rng, 25, tag)
    expected = [f"{i + 1:02d} - {n}" for i, n in enumerate(order)]
    folder = "" if where == "top" else tag
    args = ["--top-level"] if where == "top" else ["--folder", folder]
    out = write(img, pl, *args)
    v = out.get("verification") or {}
    record(f"{label}: p2u write ok, read back from the image (partition, boot sectors, FATs, tables, order, placement, every byte)",
           out.get("ok") and all(v.get(k) for k in ("partition_table_ok", "boot_sectors_ok", "fats_ok", "tables_ok", "order_ok", "layout_ok"))
           and v.get("data_checked") and not v.get("data_mismatches"), out.get("error") or "")
    plan = (out.get("result") or {}).get("plan") or {}
    fmt = plan.get("format") or {}
    record(f"{label}: cluster size from the FAT specification's table", fmt.get("sectors_per_cluster", 0) * 512 == cluster,
           f"{fmt.get('sectors_per_cluster', 0) * 512} bytes")

    # Partition table, by sfdisk.
    pt = json.loads(run(["sfdisk", "-J", img]).stdout)["partitiontable"]
    parts = pt.get("partitions", [])
    want_size = size // 512 - 2048
    record(f"{label}: sfdisk sees one FAT32 (LBA) partition from 1 MiB to the end",
           pt.get("label") == "dos" and len(parts) == 1 and parts[0]["type"] == "c" and parts[0]["start"] == 2048 and parts[0]["size"] == want_size,
           json.dumps(parts))

    # Nothing left of the old layout: blkid and wipefs on the disk and the partition.
    code, kv, err = blkid(img)
    record(f"{label}: blkid on the whole disk finds only the MBR", code == 0 and kv.get("PTTYPE") == "dos" and "TYPE" not in kv, f"{kv} {err}")
    code, kv, err = blkid(img, 2048 * 512)
    record(f"{label}: blkid on the partition finds exactly one FAT32 file system",
           code == 0 and kv.get("TYPE") == "vfat" and kv.get("VERSION") == "FAT32" and "ambivalent" not in err, f"{kv} {err}")
    w = run(["wipefs", "-n", "-J", img], check=False)
    sigs = json.loads(w.stdout or '{"signatures": []}').get("signatures", [])
    record(f"{label}: wipefs finds no stale signature on the disk", [s["type"] for s in sigs] == ["dos"], json.dumps(sigs))
    with open(img, "rb") as f:
        head = f.read(MIB)
        f.seek(size - MIB)
        tail = f.read(MIB)
    record(f"{label}: first MiB is zero apart from the MBR; last MiB is zero",
           head[:440] == bytes(440) and head[512:] == bytes(MIB - 512) and tail == bytes(MIB))

    # The file system itself, by fsck.fat, fatsort and mtools on the partition.
    part = img + ".fs"
    copy_region(img, part, 2048 * 512, want_size * 512)
    fk = run(["fsck.fat", "-n", "-v", part], check=False)
    bad = [l for l in fk.stdout.splitlines()
           if not l.startswith("Checking") and re.search(r"differ|wrong|invalid|Reclaim|orphan|Auto|Bad|summary", l, re.I)]
    record(f"{label}: fsck.fat -n finds nothing to fix", fk.returncode == 0 and not bad, " | ".join(bad[:3]) or fk.stdout.strip().splitlines()[-1])
    fs = run(["fatsort", "-l", part], check=False, env=env).stdout.splitlines()
    sect = "/" if where == "top" else f"/{folder}/"
    try:
        i = fs.index(sect)
        listed = []
        for l in fs[i + 1:]:
            if not l.strip():
                break
            listed.append(l)
    except ValueError:
        listed = []
    if where == "top":
        record(f"{label}: fatsort -l lists the top level in playlist order, nothing else there", listed == expected, f"first {listed[:2]}")
    else:
        top = fs[fs.index("/") + 1:fs.index("/") + 3] if "/" in fs else []
        record(f"{label}: fatsort -l lists the folder in playlist order; the folder is alone at the top",
               listed == expected and top[:1] == [folder] and (len(top) < 2 or not top[1].strip()), f"first {listed[:2]}")
    mpath = "::" + ("/" if where == "top" else f"/{folder}/")
    runs, ok, prev_end = [], True, None
    for n in expected:
        out_ = run(["mshowfat", "-i", part, mpath + n], check=False, env=env).stdout
        rs = re.findall(r"<(\d+)(?:-(\d+))?>", out_)
        if not rs:
            runs.append(None)
            continue
        if len(rs) != 1:
            ok = False
        a, b = int(rs[0][0]), int(rs[0][1] or rs[0][0])
        if prev_end is not None and a != prev_end + 1:
            ok = False
        prev_end = b
    record(f"{label}: mshowfat: one unbroken run per track, back to back in playlist order", ok)
    same = True
    out_dir = os.path.join(work, "back-" + tag)
    os.makedirs(out_dir, exist_ok=True)
    for n, src in zip(expected, order):
        dst = os.path.join(out_dir, "x.bin")
        r = run(["mcopy", "-n", "-o", "-i", part, mpath + n, dst], check=False, env=env)
        if r.returncode != 0 or sha(dst) != sha(os.path.join(music, src)):
            same = False
            break
    record(f"{label}: every track reads back byte-identical (mcopy)", same)
    os.remove(part)
    os.remove(img)


def pull_sweep(work, env, rng):
    """Pull the stick after every write, starting from a used FAT32 stick."""
    global PULLS
    base = os.path.join(work, "pull-base.img")
    layout_fat32_used(base, 64 * MIB, env, rng)
    pl, order, music = make_music(work, rng, 12, "Pull")
    img = os.path.join(work, "pull.img")
    states = {}
    k = 0
    while True:
        shutil.copyfile(base, img)
        out = write(img, pl, "--folder", "Pull", env_extra={"P2U_FAIL_AFTER_WRITES": str(k)})
        if out.get("ok"):
            record(f"pull sweep: completes once no write fails (after {k} write points)", True)
            break
        PULLS += 1
        with open(img, "rb") as f:
            s0 = f.read(512)
        code, kv, err = blkid(img)
        # What an OS or a player sees: sector 0 and whatever blkid can find on the disk.
        blank = s0[510:512] != b"\x55\xaa" and code == 2 and not kv
        if k == 0:
            # Nothing was written yet: the old stick must be untouched.
            state = "nothing written" if sha(img) == sha(base) else "NOT blank"
        else:
            state = "blank" if blank else "NOT blank"
        states[state] = states.get(state, 0) + 1
        if state == "NOT blank":
            record(f"pull sweep: blank after a pull at write {k}", False, f"blkid {code} {kv}")
            return
        k += 1
        if k > 500:
            record("pull sweep: terminates", False)
            return
    record(f"pull sweep: after a pull at every one of {k} write points the stick reads as blank (no partition table, no file system)",
           True, ", ".join(f"{v}x {s}" for s, v in states.items()))
    # Writing again after a pull gives a complete, verified stick.
    shutil.copyfile(base, img)
    write(img, pl, "--folder", "Pull", env_extra={"P2U_FAIL_AFTER_WRITES": str(k // 2)})
    out = write(img, pl, "--folder", "Pull")
    record("pull sweep: writing again after a pull gives a complete, verified stick", out.get("ok"), out.get("error") or "")


def refusals(work, env, rng):
    pl, order, music = make_music(work, rng, 4, "Ref")
    small = os.path.join(work, "small.img")
    layout_superfloppy_fat16(small, 30 * MIB, env, rng)
    h = sha(small)
    out = write(small, pl)
    record("refusal: a 30 MiB stick is too small for FAT32, and nothing is written",
           not out.get("ok") and "smaller" in (out.get("error") or "") and sha(small) == h, out.get("error") or "")
    big = os.path.join(music, "huge.mp3")
    with open(big, "wb") as f:
        f.truncate(80 * MIB)
    plb = os.path.join(work, "huge.m3u8")
    with open(plb, "w") as f:
        f.write(f"{big}\n")
    img = os.path.join(work, "fit.img")
    layout_fat32_used(img, 64 * MIB, env, rng)
    h = sha(img)
    out = write(img, plb)
    record("refusal: a playlist that doesn't fit is refused before anything is written",
           not out.get("ok") and "needs" in (out.get("error") or "") and sha(img) == h, out.get("error") or "")
    r = run([P2U, "write", img, "--playlist", pl, "--dry-run"], check=False)
    record("dry run writes nothing", r.returncode == 0 and sha(img) == h)
    r = run([P2U, "write", img, "--playlist", pl], check=False, stdin=subprocess.DEVNULL)
    record("without --yes and no terminal to ask on, nothing is written", r.returncode != 0 and sha(img) == h, r.stderr.strip()[:120])


def main():
    if not os.path.exists(P2U):
        sys.exit(f"build first: {P2U} not found")
    rng = random.Random(1029)
    work = tempfile.mkdtemp(prefix="p2u-stick-")
    env = mtools_env(work)
    try:
        for c in CASES:
            check_case(work, env, rng, *c)
        pull_sweep(work, env, rng)
        refusals(work, env, rng)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    passed = sum(1 for _, ok, _ in RESULTS if ok)
    print(f"\n{passed}/{len(RESULTS)} checks passed ({PULLS} simulated pulls)")
    with open(os.path.join(ROOT, "tests", "stick-e2e-results.json"), "w") as f:
        json.dump({"checks": [{"name": n, "ok": o, "detail": d} for n, o, d in RESULTS], "simulated_pulls": PULLS}, f, indent=2)
    sys.exit(0 if passed == len(RESULTS) else 1)


if __name__ == "__main__":
    main()
