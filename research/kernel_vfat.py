#!/usr/bin/env python3
"""Copy-order tests through the real Linux vfat driver, in a QEMU VM.

Why: the e2e tests drive FAT through mtools; this drives it through the kernel
driver a real user's machine uses, on sticks with a history (deleted files,
stale clusters, earlier copies, overwrites), then measures the raw on-disk
order from the image bytes.

Needs: qemu-system-x86, busybox-static, an Ubuntu kernel + its modules
(apt-get download linux-image-<ver>-generic linux-modules-<ver>-generic;
dpkg -x both into one folder), a static p2u (cargo build --release -p p2u-cli
--target x86_64-unknown-linux-musl), dosfstools, fatsort.

    python3 tests/kernel_vfat.py --kroot <extracted kernel dir> --p2u <static p2u>
"""
import argparse
import glob
import gzip
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Each scenario gets its own virtual disk (vda, vdb, ...). "setup" recreates a
# stick's history through the kernel driver; "copy" is how the playlist lands.
# Shell runs under busybox ash inside the VM; $M is the mount point.
SCENARIOS = [
    {
        "name": "fresh",
        "label": "Freshly formatted stick, p2u copy into a new folder",
        "mkfs": ["-F", "32", "-s", "1"],
        "size_mb": 48,
        "setup": "",
        "copy": 'p2u copy /pl/A.m3u8 --dest $M --folder "Road Trip"',
        "names": "A_numbered",
    },
    {
        "name": "used",
        "label": "Used, nearly full stick: old folder and half the root files deleted, then p2u copy",
        "mkfs": ["-F", "32", "-s", "1"],
        "size_mb": 40,
        "setup": """
mkdir "$M/Old Mix"; for f in /old/*; do cp "$f" "$M/Old Mix/"; done
for f in /root_files/*; do cp "$f" $M/; done
dd if=/dev/urandom of=$M/filler.bin bs=64k count=$FILLER_BLOCKS 2>/dev/null
sync
rm -r "$M/Old Mix"
i=0; for f in $M/*.dat; do i=$((i+1)); [ $((i % 2)) -eq 0 ] && rm "$f"; done
sync
""",
        "copy": 'p2u copy /pl/A.m3u8 --dest $M --folder "Road Trip"',
        "names": "A_numbered",
    },
    {
        "name": "overwrite",
        "label": "Playlist copied before, reordered, copied again over the top (same names)",
        "mkfs": ["-F", "32", "-s", "8"],
        "size_mb": 300,
        "setup": 'mkdir "$M/Road Trip"; while read -r f; do cp "/music/$(basename "$f")" "$M/Road Trip/"; done < /pl/B.list; sync',
        "copy": 'while read -r f; do cp "/music/$(basename "$f")" "$M/Road Trip/"; done < /pl/A.list',
        "names": "A_plain",
    },
    {
        "name": "existing",
        "label": "Existing folder with some tracks deleted, new tracks copied in, in order",
        "mkfs": ["-F", "32", "-s", "8"],
        "size_mb": 300,
        "setup": """
mkdir "$M/Road Trip"; for f in /old/*; do cp "$f" "$M/Road Trip/"; done
sync
i=0; for f in "$M/Road Trip"/*; do i=$((i+1)); [ $((i % 3)) -ne 0 ] && rm "$f"; done
sync
""",
        "copy": 'while read -r f; do cp "/music/$(basename "$f")" "$M/Road Trip/"; done < /pl/A.list',
        "names": "A_plain",
    },
    {
        "name": "toplevel",
        "label": "Top level of a used stick (no folder), copied in order",
        "mkfs": ["-F", "32", "-s", "8"],
        "size_mb": 300,
        "setup": """
for f in /root_files/*; do cp "$f" $M/; done
sync
i=0; for f in $M/*.dat; do i=$((i+1)); [ $((i % 2)) -eq 0 ] && rm "$f"; done
sync
""",
        "copy": 'while read -r f; do cp "/music/$(basename "$f")" $M/; done < /pl/A.list',
        "names": "A_plain_top",
        "dir": "/",
    },
    {
        "name": "toplevel_p2u",
        "label": "Top level of a used stick via p2u copy --top-level",
        "mkfs": ["-F", "32", "-s", "8"],
        "size_mb": 300,
        "setup": """
for f in /root_files/*; do cp "$f" $M/; done
sync
i=0; for f in $M/*.dat; do i=$((i+1)); [ $((i % 2)) -eq 0 ] && rm "$f"; done
sync
""",
        "copy": "p2u copy /pl/A.m3u8 --dest $M --top-level",
        "names": "A_numbered_top",
        "dir": "/",
    },
    {
        "name": "fat16_used",
        "label": "Used FAT16 stick (fixed-size root): old folder deleted, then p2u copy",
        "mkfs": ["-F", "16"],
        "size_mb": 64,
        "setup": """
mkdir "$M/Old Mix"; for f in /old/*; do cp "$f" "$M/Old Mix/"; done
for f in /root_files/*; do cp "$f" $M/; done
sync
rm -r "$M/Old Mix"; i=0; for f in $M/*.dat; do i=$((i+1)); [ $((i % 2)) -eq 0 ] && rm "$f"; done
sync
""",
        "copy": 'p2u copy /pl/A.m3u8 --dest $M --folder "Road Trip"',
        "names": "A_numbered",
    },
]

TRACKS = 60

# How the VM's p2u derives names (and expected sizes) from the playlist.
SORT_ARGS = {
    "A_numbered": '--playlist /pl/A.m3u8 --folder "Road Trip"',
    "A_plain": '--playlist /pl/A.m3u8 --folder "Road Trip" --no-number',
    "A_plain_top": "--playlist /pl/A.m3u8 --top-level --no-number",
    "A_numbered_top": "--playlist /pl/A.m3u8 --top-level",
}


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def build_payload(work, rng):
    """Music, playlists and history files that go into the initramfs."""
    music = os.path.join(work, "music")
    os.makedirs(music)
    titles = [f"Track {chr(65 + i % 26)}{i:02d} {'long title ' * (i % 4)}".strip() for i in range(TRACKS)]
    files = []
    for t in titles:
        p = os.path.join(music, t + ".mp3")
        with open(p, "wb") as f:
            f.write(rng.randbytes(rng.randint(3_000, 30_000)))
        files.append(os.path.basename(p))
    order_a = files[:]
    rng.shuffle(order_a)
    order_b = files[:]
    rng.shuffle(order_b)
    pl = os.path.join(work, "pl")
    os.makedirs(pl)
    for name, order in (("A", order_a), ("B", order_b)):
        with open(os.path.join(pl, f"{name}.m3u8"), "w") as f:
            f.write("#EXTM3U\n" + "".join(f"../music/{n}\n" for n in order))
        with open(os.path.join(pl, f"{name}.list"), "w") as f:
            f.write("".join(f"{n}\n" for n in order))
    old = os.path.join(work, "old")
    os.makedirs(old)
    for i in range(70):
        with open(os.path.join(old, f"old track {i:02d} with a longer name.mp3"), "wb") as f:
            f.write(rng.randbytes(rng.randint(2_000, 12_000)))
    rootf = os.path.join(work, "root_files")
    os.makedirs(rootf)
    for i in range(40):
        with open(os.path.join(rootf, f"document {i:02d} from last year.dat"), "wb") as f:
            f.write(rng.randbytes(rng.randint(1_000, 8_000)))
    return order_a, order_b


def build_initramfs(work, kroot, p2u, scenarios):
    ir = os.path.join(work, "ir")
    for d in ("bin", "dev", "proc", "sys", "mnt", "mods", "tmp"):
        os.makedirs(os.path.join(ir, d), exist_ok=True)
    shutil.copy("/bin/busybox", os.path.join(ir, "bin", "busybox"))
    shutil.copy(p2u, os.path.join(ir, "bin", "p2u"))
    for sub in ("music", "pl", "old", "root_files"):
        shutil.copytree(os.path.join(work, sub), os.path.join(ir, sub))
    for mod in ("nls_iso8859-1",):
        src = glob.glob(os.path.join(kroot, "lib/modules/*/kernel/fs/nls", mod + ".ko.zst"))[0]
        sh(["zstd", "-q", "-d", "-f", src, "-o", os.path.join(ir, "mods", mod + ".ko")])
    lines = [
        "#!/bin/busybox sh",
        "/bin/busybox --install -s /bin",
        "export PATH=/bin",
        "mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev",
        "insmod /mods/nls_iso8859-1.ko",
        "M=/mnt",
    ]
    for i, sc in enumerate(scenarios):
        dev = f"/dev/vd{chr(97 + i)}"
        d = sc.get("dir", "/Road Trip")
        lines += [
            f'echo "=== {sc["name"]}"',
            f"FILLER_BLOCKS={sc.get('filler_blocks', 0)}",
            f"mount -t vfat -o utf8 {dev} $M || echo MOUNTFAIL",
            sc["setup"],
            sc["copy"] + " >/dev/null 2>&1 || echo COPYFAIL",
            "sync; umount $M",
            # Raw order as the copy left it (read straight from the block device).
            f'p2u verify {dev} {SORT_ARGS[sc["names"]]} > /tmp/before.json 2>&1; echo "<<BEFORE"; cat /tmp/before.json; echo ">>"',
            # FAT-level reorder + verify, then let the kernel mount and read it.
            f'p2u sort {dev} {SORT_ARGS[sc["names"]]} --backup-dir /tmp > /tmp/sort.json 2>&1; echo "<<SORT"; cat /tmp/sort.json; echo ">>"',
            f"mount -t vfat -o utf8 {dev} $M || echo MOUNTFAIL2",
            f'p2u os-listing "$M{d if d != "/" else ""}" {SORT_ARGS[sc["names"]]} > /tmp/after.json 2>/dev/null; echo "<<KERNEL_READDIR"; cat /tmp/after.json; echo ">>"',
            f'cd "$M{d if d != "/" else ""}"; bad=0; while read -r n; do [ -z "$n" ] && continue; case "$n" in \\#*) continue;; esac; f="$n"; s=$(echo "$f" | sed "s/^[0-9]* - //"); cmp -s "$f" "/music/$s" || bad=$((bad+1)); done < /pl/{sc["names"].replace("_top", "")}.order; echo "CONTENT_MISMATCHES $bad"; cd /',
            "umount $M",
        ]
    lines += ["sync", "echo === done", "poweroff -f"]
    with open(os.path.join(ir, "init"), "w") as f:
        f.write("\n".join(lines) + "\n")
    os.chmod(os.path.join(ir, "init"), 0o755)
    cpio = os.path.join(work, "initrd.cpio")
    files = sh(["find", "."], cwd=ir).stdout
    with open(cpio, "wb") as out:
        subprocess.run(["cpio", "-o", "-H", "newc", "--quiet"], cwd=ir, input=files.encode(), stdout=out, check=True)
    return cpio


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kroot", required=True)
    ap.add_argument("--p2u", required=True)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    rng = random.Random(2026)
    work = tempfile.mkdtemp(prefix="p2u-kvfat-")
    order_a, order_b = build_payload(work, rng)
    width = max(2, len(str(TRACKS)))
    numbered = [f"{i + 1:0{width}d} - {n}" for i, n in enumerate(order_a)]
    with open(os.path.join(work, "pl", "A_numbered.order"), "w") as f:
        f.write("\n".join(numbered) + "\n")
    with open(os.path.join(work, "pl", "A_plain.order"), "w") as f:
        f.write("\n".join(order_a) + "\n")

    disks = []
    for sc in SCENARIOS:
        img = os.path.join(work, sc["name"] + ".img")
        with open(img, "wb") as f:
            f.truncate(sc["size_mb"] * 1024 * 1024)
        sh(["mkfs.fat", *sc["mkfs"], "-n", "STICK", img])
        if sc["name"] == "used":
            # Leave ~1.6 MB free after the history so the copy must reuse freed clusters.
            free = int(sh(["fsck.fat", "-n", img]).stdout.split("/")[-1].split()[0]) * 512
            sc["filler_blocks"] = max(0, (free - 1_000_000 - 70 * 8_000 - 40 * 5_000) // 65536)
        disks.append(img)

    cpio = build_initramfs(work, a.kroot, a.p2u, SCENARIOS)
    kernel = glob.glob(os.path.join(a.kroot, "boot", "vmlinuz-*"))[0]
    cmd = ["qemu-system-x86_64", "-m", "768", "-nographic", "-no-reboot", "-kernel", kernel, "-initrd", cpio,
           "-append", "console=ttyS0 panic=-1 quiet loglevel=3"]
    for d in disks:
        cmd += ["-drive", f"file={d},format=raw,if=virtio,cache=writethrough"]
    log = subprocess.run(cmd, capture_output=True, text=True, timeout=1800).stdout
    with open(os.path.join(ROOT, "tests", "kernel-vfat-console.log"), "w") as f:
        f.write(log)

    # Parse the console and add host-side measurements from the raw images.
    results = []
    cur = None
    block = None
    buf = []
    for raw in log.splitlines():
        line = raw.strip()
        if block is not None:
            if line == ">>":
                try:
                    cur[block] = json.loads("\n".join(buf))
                except ValueError:
                    cur[block] = {"unparsed": "\n".join(buf)[-400:]}
                block, buf = None, []
            else:
                buf.append(raw)
            continue
        if "=== " in line and not line.endswith("=== done"):
            cur = {"name": line.split("=== ", 1)[1]}
            results.append(cur)
        elif cur is not None and line.startswith("<<"):
            block = line[2:]
        elif cur is not None and line.startswith("CONTENT_MISMATCHES "):
            cur["content_mismatches"] = int(line.split()[1])
        elif cur is not None and line in ("MOUNTFAIL", "COPYFAIL", "MOUNTFAIL2"):
            cur.setdefault("errors", []).append(line)
    p2u_host = os.path.join(ROOT, "target", "debug", "p2u")
    for sc, r, img in zip(SCENARIOS, results, disks):
        r["label"] = sc["label"]
        fsck = subprocess.run(["fsck.fat", "-n", img], capture_output=True, text=True)
        r["fsck_clean"] = fsck.returncode == 0
        # Physical order: are the files' first clusters ascending in playlist order?
        d = sc.get("dir", "/Road Trip")
        ls = json.loads(sh([p2u_host, "ls", img, "--dir", d, "--json"]).stdout)
        want = numbered if sc["names"].startswith("A_numbered") else order_a
        clusters = {g["long_name"] or g["short_name"]: g["first_cluster"] for g in ls["groups"]}
        seq = [clusters.get(n) for n in want if n in clusters]
        r["clusters_ascending_in_playlist_order"] = all(x < y for x, y in zip(seq, seq[1:]))
        r["cluster_order_breaks"] = sum(1 for x, y in zip(seq, seq[1:]) if x >= y)
        root = json.loads(sh([p2u_host, "ls", img, "--json"]).stdout)
        r["root_slot_of_folder"] = next((g["first_slot"] for g in root["groups"] if g["long_name"] == "Road Trip"), None)
        r["root_entries"] = len(root["groups"])
    print(json.dumps(results, indent=2))
    with open(os.path.join(ROOT, "tests", "kernel-vfat-results.json"), "w") as f:
        json.dump(results, f, indent=2)
    if not a.keep:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print("kept", work)


if __name__ == "__main__":
    main()
