#!/usr/bin/env python3
"""`fatstick write` on whole virtual disks, then the real Linux vfat driver.

In a QEMU VM (Ubuntu kernel, busybox), for each virtual stick that starts out
with an awkward layout (made on the host): run `fatstick write` on the whole block
device, have the kernel re-read the partition table, mount the new partition
with the kernel's vfat driver, check the order the kernel lists and every
byte, then let the kernel keep using the stick (add a file, delete a track)
and check the result on the host with fsck.fat, mshowfat and blkid.

    python3 tests/kernel_stick.py --kroot <kernel dir> --fatstick <static fatstick>
"""
import argparse
import glob
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tests"))
import stick_e2e as e2e  # noqa: E402

MIB = 1 << 20
GIB = 1 << 30

STICKS = [
    {"name": "gpt_exfat_1g", "layout": "gpt_exfat", "size": GIB, "args": '--folder "Road Trip"', "dir": "/Road Trip"},
    {"name": "mbr_ntfs_ext4_2g", "layout": "mbr_ntfs_ext4", "size": 2 * GIB, "args": '--folder "Road Trip"', "dir": "/Road Trip"},
    {"name": "superfloppy_fat16_top", "layout": "superfloppy_fat16", "size": 256 * MIB, "args": "--top-level", "dir": "/"},
    {"name": "used_fat32_junk_tail_512m", "layout": "fat32_used", "size": 512 * MIB, "args": '--folder "Road Trip"', "dir": "/Road Trip"},
]


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kroot", required=True)
    ap.add_argument("--fatstick", required=True)
    a = ap.parse_args()
    rng = random.Random(4243)
    work = tempfile.mkdtemp(prefix="fatstick-kstick-")
    env = e2e.mtools_env(work)
    ir = os.path.join(work, "ir")
    for d in ("bin", "dev", "proc", "sys", "mnt", "mods", "tmp", "music", "pl"):
        os.makedirs(os.path.join(ir, d), exist_ok=True)
    shutil.copy("/bin/busybox", os.path.join(ir, "bin", "busybox"))
    shutil.copy(a.fatstick, os.path.join(ir, "bin", "fatstick"))
    src = glob.glob(os.path.join(a.kroot, "lib/modules/*/kernel/fs/nls/nls_iso8859-1.ko.zst"))[0]
    sh(["zstd", "-q", "-d", "-f", src, "-o", os.path.join(ir, "mods", "nls.ko")])

    names = []
    for i in range(30):
        n = f"Track {i:02d} {'long title ' * (i % 3)}".strip() + ".mp3"
        with open(os.path.join(ir, "music", n), "wb") as f:
            f.write(rng.randbytes(rng.randint(4_000, 120_000)))
        names.append(n)
    order = names[:]
    rng.shuffle(order)
    with open(os.path.join(ir, "pl", "Road Trip.m3u8"), "w") as f:
        f.write("".join(f"../music/{n}\n" for n in order))
    numbered = [f"{i + 1:02d} - {n}" for i, n in enumerate(order)]
    with open(os.path.join(ir, "pl", "order.txt"), "w") as f:
        f.write("\n".join(numbered) + "\n")

    imgs = []
    for st in STICKS:
        img = os.path.join(work, st["name"] + ".img")
        e2e.LAYOUTS[st["layout"]](img, st["size"], env, rng)
        if st["name"].startswith("used_fat32_junk"):
            # Junk where an old GPT backup header or RAID superblock would sit.
            e2e.put(img, st["size"] - MIB, rng.randbytes(MIB))
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
        lines += [
            f'echo "=== {st["name"]}"',
            f'fatstick write {dev} --playlist "/pl/Road Trip.m3u8" {st["args"]} --yes --json > /tmp/w.json 2>&1; echo "<<WRITE"; cat /tmp/w.json; echo ">>"',
            f"blockdev --rereadpt {dev}; sleep 1",
            f'echo "PARTS $(ls {dev}* | tr "\\n" " ")"',
            f"mount -t vfat -o utf8 {dev}1 /mnt || echo MOUNTFAIL",
            # find does not sort: this is the order the kernel's readdir returns.
            f'echo "<<KLIST"; find "{md}" -mindepth 1 -maxdepth 1 -type f | sed "s#^{md}/##"; echo ">>"',
            f'cd "{md}"; bad=0; while read -r n; do s=$(echo "$n" | sed "s/^[0-9]* - //"); cmp -s "$n" "/music/$s" || bad=$((bad+1)); done < /pl/order.txt; echo "CONTENT_BAD $bad"; cd /pl',
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
    cmd = ["qemu-system-x86_64", "-m", "1024", "-nographic", "-no-reboot", "-kernel", kernel, "-initrd", cpio,
           "-append", "console=ttyS0 panic=-1 quiet loglevel=3"]
    for img in imgs:
        cmd += ["-drive", f"file={img},format=raw,if=virtio,cache=writethrough"]
    log = subprocess.run(cmd, capture_output=True, text=True, timeout=1800).stdout
    with open(os.path.join(ROOT, "tests", "kernel-stick-console.log"), "w") as f:
        f.write(log)

    results, cur, block, buf = [], None, None, []
    for raw in log.splitlines():
        line = raw.strip()
        if block is not None:
            if line == ">>":
                text = "\n".join(buf)
                if block == "KLIST":
                    cur[block] = [l.strip() for l in buf if l.strip()]
                else:
                    try:
                        cur[block] = json.loads(text)
                    except ValueError:
                        cur[block] = {"unparsed": text[-300:]}
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
        elif cur is not None and line.startswith("PARTS "):
            cur["parts"] = line[6:].split()
        elif cur is not None and line == "MOUNTFAIL":
            cur["mountfail"] = True

    summary = []
    for st, r, img in zip(STICKS, results, imgs):
        w = r.get("WRITE", {})
        v = w.get("verification") or {}
        part = img + ".fs"
        e2e.copy_region(img, part, 2048 * 512, st["size"] - 2048 * 512)
        fk = subprocess.run(["fsck.fat", "-n", part], capture_output=True, text=True)
        code, kv, err = e2e.blkid(img)
        runs_ok, last = True, 0
        prefix = "::" + ("/" if st["dir"] == "/" else st["dir"] + "/")
        for n in numbered:
            if n == numbered[5]:
                continue
            out = subprocess.run(["mshowfat", "-i", part, prefix + n], capture_output=True, text=True, env=env).stdout
            rs = re.findall(r"<(\d+)(?:-(\d+))?>", out)
            if len(rs) != 1 or int(rs[0][0]) <= last:
                runs_ok = False
            if rs:
                last = int(rs[0][1] or rs[0][0])
        klist = r.get("KLIST", [])
        row = {
            "stick": st["name"],
            "write_ok": w.get("ok"),
            "self_verified": bool(v) and all(v.get(k) for k in ("partition_table_ok", "boot_sectors_ok", "fats_ok", "tables_ok", "order_ok", "layout_ok"))
            and not v.get("data_mismatches"),
            "kernel_found_one_partition": r.get("parts") == [f"/dev/vd{chr(97 + STICKS.index(st))}", f"/dev/vd{chr(97 + STICKS.index(st))}1"],
            "kernel_lists_in_order": klist == numbered,
            "kernel_reads_identical": r.get("content_bad") == 0,
            "fsck_clean_after_kernel_use": fk.returncode == 0,
            "mshowfat_one_run_each_ascending": runs_ok,
            "blkid_only_mbr": code == 0 and kv.get("PTTYPE") == "dos" and "TYPE" not in kv,
            "error": w.get("error"),
        }
        os.remove(part)
        summary.append(row)
    print(json.dumps(summary, indent=2))
    with open(os.path.join(ROOT, "tests", "kernel-stick-results.json"), "w") as f:
        json.dump(summary, f, indent=2)
    shutil.rmtree(work, ignore_errors=True)
    ok = len(summary) == len(STICKS) and all(all(v for k, v in row.items() if k not in ("stick", "error")) for row in summary)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
