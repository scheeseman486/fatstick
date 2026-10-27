#!/usr/bin/env python3
"""`p2u write` against real block devices, then the real Linux vfat driver.

In a QEMU VM (Ubuntu kernel, busybox), for each virtual stick with a history
made on the host with mtools: run `p2u write` on the unmounted block device,
mount it with the kernel's vfat driver, check the listing order and every byte,
then let the kernel keep using the stick (add a file, delete a track) and
check the result on the host with fsck.fat and mshowfat.

    python3 tests/kernel_write.py --kroot <kernel dir> --p2u <static p2u>
"""
import argparse
import glob
import json
import os
import random
import re
import shutil
import subprocess
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def mt(img, *args, check=True):
    rc = img + ".mtoolsrc"
    with open(rc, "w") as f:
        f.write(f'drive x: file="{img}"\nmtools_skip_check=1\n')
    return subprocess.run(list(args), capture_output=True, text=True, env=dict(os.environ, MTOOLSRC=rc), check=check)


STICKS = [
    {"name": "fat32_used", "mkfs": ["-F", "32", "-s", "1"], "mb": 64, "args": '--folder "Road Trip"', "dir": "/Road Trip"},
    {"name": "fat16_used", "mkfs": ["-F", "16"], "mb": 64, "args": '--folder "Road Trip"', "dir": "/Road Trip"},
    {"name": "fat32_top", "mkfs": ["-F", "32", "-s", "8"], "mb": 300, "args": "--top-level", "dir": "/"},
    {"name": "fat32_replace", "mkfs": ["-F", "32", "-s", "1"], "mb": 64, "args": '--folder "Road Trip" --replace', "dir": "/Road Trip", "prewrite": True},
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kroot", required=True)
    ap.add_argument("--p2u", required=True)
    a = ap.parse_args()
    rng = random.Random(4242)
    work = tempfile.mkdtemp(prefix="p2u-kwrite-")
    ir = os.path.join(work, "ir")
    for d in ("bin", "dev", "proc", "sys", "mnt", "mods", "tmp", "music", "pl"):
        os.makedirs(os.path.join(ir, d), exist_ok=True)
    shutil.copy("/bin/busybox", os.path.join(ir, "bin", "busybox"))
    shutil.copy(a.p2u, os.path.join(ir, "bin", "p2u"))
    src = glob.glob(os.path.join(a.kroot, "lib/modules/*/kernel/fs/nls/nls_iso8859-1.ko.zst"))[0]
    sh(["zstd", "-q", "-d", "-f", src, "-o", os.path.join(ir, "mods", "nls.ko")])

    names = []
    for i in range(40):
        n = f"Track {i:02d} {'long title ' * (i % 3)}".strip() + ".mp3"
        with open(os.path.join(ir, "music", n), "wb") as f:
            f.write(rng.randbytes(rng.randint(4_000, 60_000)))
        names.append(n)
    order = names[:]
    rng.shuffle(order)
    with open(os.path.join(ir, "pl", "Road Trip.m3u8"), "w") as f:
        f.write("".join(f"../music/{n}\n" for n in order))
    numbered = [f"{i + 1:02d} - {n}" for i, n in enumerate(order)]
    with open(os.path.join(ir, "pl", "order.txt"), "w") as f:
        f.write("\n".join(numbered) + "\n")
    rev = order[::-1]
    with open(os.path.join(ir, "pl", "Earlier.m3u8"), "w") as f:
        f.write("".join(f"../music/{n}\n" for n in rev))

    # Sticks with a history, made on the host.
    imgs = []
    for st in STICKS:
        img = os.path.join(work, st["name"] + ".img")
        with open(img, "wb") as f:
            f.truncate(st["mb"] * 1024 * 1024)
        sh(["mkfs.fat", *st["mkfs"], "-n", "STICK", img])
        blob = os.path.join(work, "blob.bin")
        for i in range(60):
            with open(blob, "wb") as f:
                f.write(rng.randbytes(rng.randint(1_000, 9_000)))
            mt(img, "mcopy", "-o", blob, f"x:/old document {i:02d}.dat")
        for i in range(0, 60, 2):
            mt(img, "mdel", f"x:/old document {i:02d}.dat")
        imgs.append(img)

    lines = [
        "#!/bin/busybox sh",
        "/bin/busybox --install -s /bin",
        "export PATH=/bin",
        "mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev",
        "insmod /mods/nls.ko",
        "cd /pl",
    ]
    for i, st in enumerate(STICKS):
        dev = f"/dev/vd{chr(97 + i)}"
        d = st["dir"]
        md = "/mnt" + ("" if d == "/" else d)
        lines += [f'echo "=== {st["name"]}"']
        if st.get("prewrite"):
            lines += [f'p2u write {dev} --playlist /pl/Earlier.m3u8 --folder "Road Trip" --json --backup-dir /tmp > /tmp/pre.json 2>&1; echo "<<PRE"; cat /tmp/pre.json; echo ">>"']
        lines += [
            f'p2u write {dev} --playlist "/pl/Road Trip.m3u8" {st["args"]} --json --backup-dir /tmp > /tmp/w.json 2>&1; echo "<<WRITE"; cat /tmp/w.json; echo ">>"',
            f"mount -t vfat -o utf8 {dev} /mnt || echo MOUNTFAIL",
            f'p2u os-listing "{md}" --order-file /pl/order.txt > /tmp/l.json 2>/dev/null; echo "<<KERNEL_LIST"; cat /tmp/l.json; echo ">>"',
            f'cd "{md}"; bad=0; while read -r n; do s=$(echo "$n" | sed "s/^[0-9]* - //"); cmp -s "$n" "/music/$s" || bad=$((bad+1)); done < /pl/order.txt; echo "CONTENT_BAD $bad"; cd /pl',
            # The kernel keeps using the stick: add a file, delete a track.
            f'cp "/music/{names[0]}" "{md}/added by kernel.mp3"; rm "{md}/{numbered[5]}"; sync',
            "umount /mnt",
        ]
    lines += ["sync", "echo === done", "poweroff -f"]
    with open(os.path.join(ir, "init"), "w") as f:
        f.write("\n".join(lines) + "\n")
    os.chmod(os.path.join(ir, "init"), 0o755)
    cpio = os.path.join(work, "initrd.cpio")
    files = sh(["find", "."], cwd=ir).stdout
    with open(cpio, "wb") as out:
        subprocess.run(["cpio", "-o", "-H", "newc", "--quiet"], cwd=ir, input=files.encode(), stdout=out, check=True)
    kernel = glob.glob(os.path.join(a.kroot, "boot", "vmlinuz-*"))[0]
    cmd = ["qemu-system-x86_64", "-m", "768", "-nographic", "-no-reboot", "-kernel", kernel, "-initrd", cpio,
           "-append", "console=ttyS0 panic=-1 quiet loglevel=3"]
    for img in imgs:
        cmd += ["-drive", f"file={img},format=raw,if=virtio,cache=writethrough"]
    log = subprocess.run(cmd, capture_output=True, text=True, timeout=1800).stdout
    with open(os.path.join(ROOT, "tests", "kernel-write-console.log"), "w") as f:
        f.write(log)

    results, cur, block, buf = [], None, None, []
    for raw in log.splitlines():
        line = raw.strip()
        if block is not None:
            if line == ">>":
                try:
                    cur[block] = json.loads("\n".join(buf))
                except ValueError:
                    cur[block] = {"unparsed": "\n".join(buf)[-300:]}
                block, buf = None, []
            else:
                buf.append(raw)
            continue
        if "=== " in line and not line.endswith("=== done"):
            cur = {"name": line.split("=== ", 1)[1]}
            results.append(cur)
        elif cur is not None and line.startswith("<<"):
            block = line[2:]
        elif cur is not None and line.startswith("CONTENT_BAD "):
            cur["content_bad"] = int(line.split()[1])
        elif cur is not None and line == "MOUNTFAIL":
            cur["mountfail"] = True

    summary = []
    for st, r, img in zip(STICKS, results, imgs):
        w = r.get("WRITE", {})
        v = w.get("verification") or {}
        lay = (w.get("result") or {}).get("layout") or {}
        fsck = subprocess.run(["fsck.fat", "-n", img], capture_output=True, text=True)
        # Layout of the remaining tracks, by mtools.
        runs_ok, last = True, 0
        for n in numbered:
            if n == numbered[5]:
                continue
            out = mt(img, "mshowfat", f"x:{st['dir'].rstrip('/')}/{n}", check=False).stdout
            rs = re.findall(r"<(\d+)(?:-(\d+))?>", out)
            if len(rs) != 1 or int(rs[0][0]) <= last:
                runs_ok = False
            if rs:
                last = int(rs[0][1] or rs[0][0])
        row = {
            "stick": st["name"],
            "write_ok": w.get("ok"),
            "self_verified": bool(v) and all([v.get("order_ok"), v.get("layout_matches_plan"), v.get("fat_copies_identical"), not v.get("data_mismatches")]),
            "contiguous": lay.get("contiguous"),
            "kernel_lists_in_order": (r.get("KERNEL_LIST") or {}).get("in_order"),
            "kernel_reads_identical": r.get("content_bad") == 0,
            "fsck_clean_after_kernel_use": fsck.returncode == 0,
            "mshowfat_one_run_each_ascending": runs_ok,
            "error": w.get("error"),
        }
        summary.append(row)
    print(json.dumps(summary, indent=2))
    with open(os.path.join(ROOT, "tests", "kernel-write-results.json"), "w") as f:
        json.dump(summary, f, indent=2)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
