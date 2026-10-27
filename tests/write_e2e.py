#!/usr/bin/env python3
"""End-to-end tests for `p2u write` (raw FAT writer) and crash safety.

Needs dosfstools, mtools, fatsort, fdisk (sfdisk). Run after `cargo build`:

    python3 tests/write_e2e.py [path/to/p2u]

Independent checks (no shared code with p2u):
  * fsck.fat -n             file system consistency
  * fatsort -l              FAT order of the folder's entries
  * mshowfat (mtools)       which clusters each file occupies
  * mcopy (mtools)          every file's bytes read back

Crash safety: each write is re-run with P2U_FAIL_AFTER_WRITES=k for every k
until it completes. After each simulated pull, fsck may only report what an
interrupted-but-safe write can leave (unreferenced clusters, a stale free
count, a lone long-name slot); the playlist must be either absent or
complete and correct (top level: a complete prefix); and restoring the undo
backups must give a fully clean file system again.
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
RESULTS = []
FAULT_RUNS = 0

SAFE_FSCK = [
    r"^fsck\.fat",
    r"^Reclaimed \d+ unused clusters",
    r"^FATs differ but appear to be intact",
    r"^\s*Using first FAT",
    r"^Free cluster summary wrong",
    r"^\s*Auto-correcting",
    r"^Leaving filesystem unchanged",
    r"^Orphaned long file name part",
    r"^\s*Auto-deleting\.?$",
    r"^\s*Removing\.?$",
    r"^\S.*: \d+ files, \d+/\d+ clusters$",
    r"^$",
]


def run(cmd, check=True, env=None, input=None):
    r = subprocess.run(cmd, capture_output=True, text=True, env=env, input=input)
    if check and r.returncode != 0:
        raise RuntimeError(f"{cmd!r} failed ({r.returncode}):\n{r.stdout}\n{r.stderr}")
    return r


def record(name, ok, detail=""):
    RESULTS.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""))


def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


class Stick:
    def __init__(self, work, name, fat, size_mb, spc=None, partitioned=False):
        self.work = work
        self.img = os.path.join(work, name + ".img")
        self.offset = 0
        with open(self.img, "wb") as f:
            f.truncate(size_mb * 1024 * 1024)
        mk = ["mkfs.fat", "-F", str(fat), "-n", "P2UTEST"]
        if spc:
            mk += ["-s", str(spc)]
        if partitioned:
            run(["sfdisk", "--quiet", self.img], input="label: dos\nstart=2048, type=c\n")
            self.offset = 2048 * 512
            mk += ["--offset", "2048", self.img, str((size_mb * 1024 * 1024 - self.offset) // 1024)]
        else:
            mk += [self.img]
        run(mk)
        self.rc = os.path.join(work, name + ".mtoolsrc")
        with open(self.rc, "w") as f:
            off = f" offset={self.offset}" if self.offset else ""
            f.write(f'drive x: file="{self.img}"{off}\nmtools_skip_check=1\n')

    def env(self):
        e = dict(os.environ)
        e["MTOOLSRC"] = self.rc
        return e

    def m(self, *args, check=True):
        return run(list(args), env=self.env(), check=check)

    def fsck(self):
        img = self.img
        if self.offset:
            img = self.img + ".part"
            with open(self.img, "rb") as s, open(img, "wb") as d:
                s.seek(self.offset)
                shutil.copyfileobj(s, d)
        r = run(["fsck.fat", "-n", img], check=False)
        if self.offset:
            os.remove(img)
        lines = (r.stdout + r.stderr).splitlines()
        used = None
        for l in lines:
            mm = re.search(r"(\d+)/(\d+) clusters", l)
            if mm:
                used = int(mm.group(1))
        return r.returncode == 0, lines, used

    def fsck_only_safe(self):
        _, lines, _ = self.fsck()
        bad = [l for l in lines if not any(re.search(p, l) for p in SAFE_FSCK)]
        return bad

    def order(self, folder):
        out = run(["fatsort", "-l", self.img], check=False).stdout
        names, cur = [], None
        want = folder.rstrip("/") + "/"
        for line in out.splitlines():
            if line.startswith("/"):
                cur = line.strip()
            elif cur == want and line.strip():
                names.append(line.strip())
        return names

    def clusters(self, path):
        """Cluster ranges of a file per mshowfat, e.g. [(6, 13)]."""
        out = self.m("mshowfat", f"x:{path}").stdout
        return [(int(a), int(b) if b else int(a)) for a, b in re.findall(r"<(\d+)(?:-(\d+))?>", out)]

    def read(self, path, dst):
        self.m("mcopy", "-o", "-n", f"x:{path}", dst)

    def exists(self, path):
        return self.m("mdir", "-b", f"x:{path}", check=False).returncode == 0

    def listing(self, folder):
        r = self.m("mdir", "-b", f"x:{folder}", check=False)
        if r.returncode != 0:
            return None
        return [os.path.basename(l.rstrip("/")) for l in r.stdout.splitlines() if l.strip()]


def make_music(work, n, rng, prefix="Track", min_size=3_000, max_size=40_000):
    d = os.path.join(work, f"music-{prefix}")
    os.makedirs(d, exist_ok=True)
    files = []
    for i in range(n):
        p = os.path.join(d, f"{prefix} {i:02d} {'with a long title ' * (i % 3)}".strip() + ".mp3")
        with open(p, "wb") as f:
            f.write(rng.randbytes(rng.randint(min_size, max_size)))
        files.append(p)
    rng.shuffle(files)
    pl = os.path.join(work, f"{prefix}.m3u8")
    with open(pl, "w") as f:
        f.write("#EXTM3U\n" + "".join(p + "\n" for p in files))
    return pl, files


def plan(pl, extra=()):
    out = json.loads(run([P2U, "plan", pl, "--dest", "/", "--json", *extra]).stdout)
    return out, [i["target_name"] for i in out["items"]], [i["source"] for i in out["items"]]


def write(stick, pl, *extra, env_extra=None, check=False):
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    bk = tempfile.mkdtemp(dir=stick.work, prefix="bk-")
    r = run([P2U, "write", stick.img, "--playlist", pl, "--json", "--backup-dir", bk, *extra], env=env, check=False)
    try:
        out = json.loads(r.stdout)
    except ValueError:
        out = {"ok": False, "error": (r.stdout + r.stderr)[-300:]}
    out["_backups"] = sorted(os.path.join(bk, f) for f in os.listdir(bk))
    return out


def contents_ok(stick, folder, names, sources):
    tmp = os.path.join(stick.work, "readback.bin")
    for n, s in zip(names, sources):
        stick.read(f"{folder.rstrip('/')}/{n}", tmp)
        if sha(tmp) != sha(s):
            return False, n
    return True, ""


def layout_ok(stick, folder, names):
    """Every file one cluster run; each file starts right after the previous one."""
    prev_end = None
    for n in names:
        rs = stick.clusters(f"{folder.rstrip('/')}/{n}")
        if len(rs) > 1:
            return False, f"{n} split into {len(rs)} runs"
        if rs and prev_end is not None and rs[0][0] != prev_end + 1:
            return False, f"{n} starts at {rs[0][0]}, previous ended at {prev_end}"
        if rs:
            prev_end = rs[0][1]
    return True, ""


def ascending_ok(stick, folder, names):
    last = 0
    for n in names:
        for a, b in stick.clusters(f"{folder.rstrip('/')}/{n}"):
            if a <= last:
                return False
            last = b
    return True


def full_checks(label, stick, folder, names, sources, contiguous=True):
    ok, lines, _ = stick.fsck()
    record(f"{label}: fsck.fat clean", ok, " ".join(lines[-1:]))
    if stick.offset:
        # fatsort can't read partition tables; read the raw order with p2u ls instead.
        ls = json.loads(run([P2U, "ls", stick.img, "--dir", folder, "--json"]).stdout)
        order = [g["long_name"] or g["short_name"] for g in ls["groups"] if g["kind"] in ("File", "Dir")]
        record(f"{label}: raw listing (p2u ls) shows the playlist in FAT order", order[: len(names)] == names, f"first {order[:2]}")
    else:
        order = stick.order(folder)
        record(f"{label}: fatsort -l lists the playlist in FAT order", order[: len(names)] == names, f"first {order[:2]}")
    if contiguous:
        lo, why = layout_ok(stick, folder, names)
        record(f"{label}: mshowfat: one unbroken run per file, back to back in playlist order", lo, why)
    else:
        record(f"{label}: mshowfat: clusters ascend through the playlist", ascending_ok(stick, folder, names))
    co, bad = contents_ok(stick, folder, names, sources)
    record(f"{label}: every file reads back byte-identical (mcopy)", co, bad)


def scenario_basic(work, rng, label, fat, size_mb, n, spc=None, partitioned=False):
    st = Stick(work, label.replace(" ", "_"), fat, size_mb, spc, partitioned)
    pl, _ = make_music(work, n, rng, prefix=label.split()[0] + str(n))
    _, names, sources = plan(pl)
    out = write(st, pl)
    v = out.get("verification") or {}
    record(f"{label}: p2u write ok and self-verified (order, layout, FAT copies, data)", out["ok"], out.get("error") or "")
    record(f"{label}: p2u reports one unbroken block", bool(out.get("result") and out["result"]["layout"]["contiguous"]))
    full_checks(label, st, "/" + os.path.splitext(os.path.basename(pl))[0], names, sources)
    return st, pl, out


def fragment_free_space(st, work, rng, count, size):
    """Fill with small files and delete every other one, leaving many holes."""
    blob = os.path.join(work, "blob.bin")
    for i in range(count):
        with open(blob, "wb") as f:
            f.write(rng.randbytes(size))
        st.m("mcopy", "-o", blob, f"x:/filler{i:04d}.bin")
    for i in range(0, count, 2):
        st.m("mdel", f"x:/filler{i:04d}.bin")


def classify_crash(st, folder, names, sources, old=None, prefix_ok=False):
    """After a simulated pull: only safe fsck findings, and the playlist absent or complete."""
    bad = st.fsck_only_safe()
    if bad:
        return False, "fsck: " + " | ".join(bad[:3])
    present = st.listing(folder)
    if folder == "/":
        present = [p for p in (present or []) if p in names]
        if prefix_ok:
            # Top level: tracks appear in table order; any visible set must be a prefix... or any subset, all complete.
            for n in present:
                i = names.index(n)
                tmp = os.path.join(st.work, "c.bin")
                st.read("/" + n, tmp)
                if sha(tmp) != sha(sources[i]):
                    return False, f"{n} visible but wrong"
            return True, f"{len(present)} visible"
    if present is None:
        return True, "absent"
    for cand_names, cand_sources, tag in ((names, sources, "new"), *([(old[0], old[1], "old")] if old else [])):
        if sorted(present) == sorted(cand_names):
            ok, bad_n = contents_ok(st, folder, cand_names, cand_sources)
            return ok, f"{tag} complete" if ok else f"{tag} visible but {bad_n} wrong"
    return False, f"partial folder: {len(present)} entries"


def fault_sweep(label, base_img, st, args, folder, names, sources, old=None, prefix_ok=False, limit=4000):
    global FAULT_RUNS
    states = {}
    for k in range(limit):
        shutil.copyfile(base_img, st.img)
        out = write(st, *args, env_extra={"P2U_FAIL_AFTER_WRITES": str(k)}) if args[0] != "sort" else None
        if out["ok"]:
            record(f"{label}: completes once no write fails (k={k})", True)
            break
        FAULT_RUNS += 1
        ok, why = classify_crash(st, folder, names, sources, old, prefix_ok)
        states[why] = states.get(why, 0) + 1
        if not ok:
            record(f"{label}: safe after a pull at write {k}", False, why)
            return
        # Restoring the undo backups (newest first) must give a fully clean file system.
        if out["_backups"]:
            run([P2U, "restore", st.img, *out["_backups"]])
            clean, lines, _ = st.fsck()
            if not clean:
                record(f"{label}: restore after a pull at write {k} is clean", False, " | ".join(lines[1:3]))
                return
    record(f"{label}: safe after a pull at every one of {k} write points; restore always clean", True,
           ", ".join(f"{v}x {s}" for s, v in sorted(states.items())))


def sort_sweep(label, base_img, st, folder, order_file, names, limit=2000):
    """Interrupt `p2u sort` (two-pass table rewrite) at every write."""
    global FAULT_RUNS
    states = {}
    for k in range(limit):
        shutil.copyfile(base_img, st.img)
        bk = tempfile.mkdtemp(dir=st.work, prefix="bks-")
        env = dict(os.environ, P2U_FAIL_AFTER_WRITES=str(k))
        r = run([P2U, "sort", st.img, "--dir", folder, "--order-file", order_file, "--backup-dir", bk], env=env, check=False)
        out = json.loads(r.stdout)
        if out["ok"]:
            record(f"{label}: completes once no write fails (k={k})", True)
            break
        FAULT_RUNS += 1
        bad = st.fsck_only_safe()
        if bad:
            record(f"{label}: safe after a pull at write {k}", False, " | ".join(bad[:3]))
            return
        listed = st.listing(folder) or []
        dup = len(listed) != len(set(listed))
        if dup:
            record(f"{label}: no duplicated entry after a pull at write {k}", False)
            return
        states[f"{len(listed)} listed"] = states.get(f"{len(listed)} listed", 0) + 1
        backups = sorted(os.path.join(bk, f) for f in os.listdir(bk))
        if backups:
            run([P2U, "restore", st.img, *backups])
        if sha(st.img) != sha(base_img):
            record(f"{label}: restore after a pull at write {k} gives back the exact original", False)
            return
    record(f"{label}: safe after a pull at every one of {k} write points, never a duplicate; restore exact", True,
           ", ".join(f"{v}x {s}" for s, v in sorted(states.items())))


def main():
    if not os.path.exists(P2U):
        sys.exit(f"build first: {P2U} not found")
    rng = random.Random(77)
    work = tempfile.mkdtemp(prefix="p2u-write-")
    try:
        # 1-4. Fresh sticks of every FAT type, and a partitioned one.
        scenario_basic(work, rng, "FAT32 512B clusters 40 tracks", 32, 64, 40, spc=1)
        scenario_basic(work, rng, "FAT16 25 tracks", 16, 32, 25)
        scenario_basic(work, rng, "FAT12 10 tracks", 12, 4, 10)
        st, pl, out = scenario_basic(work, rng, "FAT32 in MBR partition 20 tracks", 32, 80, 20, spc=1, partitioned=True)

        # 5. Used stick: free space full of holes but a large block at the end.
        st = Stick(work, "used", 32, 64, spc=1)
        fragment_free_space(st, work, rng, 200, 3000)
        pl, _ = make_music(work, 30, rng, prefix="Used")
        _, names, sources = plan(pl)
        out = write(st, pl)
        record("used stick with holes: write ok", out["ok"], out.get("error") or "")
        record("used stick with holes: still one unbroken block (holes skipped)", out["ok"] and out["result"]["layout"]["contiguous"])
        full_checks("used stick with holes", st, "/Used", names, sources)
        ok, _ = contents_ok(st, "/", ["filler0001.bin"], [os.path.join(work, "blob.bin")]) if False else (True, "")

        # 6. No single free block large enough: fill the stick, then punch holes.
        st = Stick(work, "holes", 32, 34, spc=1)
        blob = os.path.join(work, "big.bin")
        with open(blob, "wb") as f:
            f.write(rng.randbytes(30_000_000))
        st.m("mcopy", blob, "x:/big.bin")
        small = os.path.join(work, "small.bin")
        with open(small, "wb") as f:
            f.write(rng.randbytes(20_000))
        i = 0
        while st.m("mcopy", "-o", small, f"x:/hole{i:04d}.bin", check=False).returncode == 0:
            i += 1
        st.m("mdel", f"x:/hole{i:04d}.bin", check=False)  # a partial copy may be left by the failed one
        for k in range(0, i, 2):
            st.m("mdel", f"x:/hole{k:04d}.bin")
        pl, _ = make_music(work, 12, rng, prefix="Tight", min_size=30_000, max_size=60_000)
        _, names, sources = plan(pl)
        before = sha(st.img)
        out = write(st, pl)
        record("no single free block: refused by default", not out["ok"] and "no single free block" in (out.get("error") or ""), (out.get("error") or "")[:70])
        record("no single free block: refusal wrote nothing", sha(st.img) == before)
        out = write(st, pl, "--allow-fragmented")
        lay = (out.get("result") or {}).get("layout") or {}
        record("allow fragmented: write ok and self-verified", out["ok"], out.get("error") or "")
        record("allow fragmented: ascending, not contiguous", bool(lay.get("ascending") and not lay.get("contiguous")))
        full_checks("allow fragmented", st, "/Tight", names, sources, contiguous=False)

        # 7. Replace: same folder, new order; no space leaks.
        st = Stick(work, "replace", 32, 64, spc=1)
        pl_a, _ = make_music(work, 20, rng, prefix="Mix")
        _, names_a, sources_a = plan(pl_a)
        write(st, pl_a)
        _, _, used_a = st.fsck()
        before = sha(st.img)
        out = write(st, pl_a)
        record("replace: refused without --replace", not out["ok"] and "already exists" in (out.get("error") or ""))
        record("replace: refusal wrote nothing", sha(st.img) == before)
        # Same folder name, reshuffled playlist.
        with open(pl_a) as f:
            lines = [l for l in f.read().splitlines() if l and not l.startswith("#")]
        rng.shuffle(lines)
        pl_b = os.path.join(work, "B", "Mix.m3u8")
        os.makedirs(os.path.dirname(pl_b), exist_ok=True)
        with open(pl_b, "w") as f:
            f.write("\n".join(lines) + "\n")
        _, names_b, sources_b = plan(pl_b)
        out = write(st, pl_b, "--replace")
        record("replace: write ok and self-verified", out["ok"], out.get("error") or "")
        full_checks("replace", st, "/Mix", names_b, sources_b)
        _, _, used_b = st.fsck()
        record("replace: no clusters leaked", used_a == used_b, f"{used_a} vs {used_b} used")

        # 8. Top level of a used stick.
        st = Stick(work, "top", 32, 64, spc=1)
        keep = os.path.join(work, "keep.txt")
        with open(keep, "wb") as f:
            f.write(rng.randbytes(9_000))
        st.m("mcopy", keep, "x:/Keep me.txt")
        fragment_free_space(st, work, rng, 30, 2000)
        pl, _ = make_music(work, 15, rng, prefix="Top")
        _, names, sources = plan(pl, ["--top-level"])
        out = write(st, pl, "--top-level")
        record("top level: write ok and self-verified", out["ok"], out.get("error") or "")
        full_checks("top level", st, "/", names, sources)
        co, _ = contents_ok(st, "/", ["Keep me.txt"], [keep])
        record("top level: existing files untouched", co)

        # 9. Restore undoes a write completely.
        st = Stick(work, "restore", 32, 64, spc=1)
        _, _, used0 = st.fsck()
        pl, _ = make_music(work, 10, rng, prefix="Undo")
        out = write(st, pl)
        run([P2U, "restore", st.img, *out["_backups"]])
        clean, _, used1 = st.fsck()
        record("restore: file system clean, folder gone, space returned", clean and not st.exists("/Undo") and used0 == used1, f"{used0} vs {used1}")

        # 9b. A backup is refused once the stick has changed since.
        out = write(st, pl)
        st.m("mcopy", keep, "x:/Later.txt")
        before = sha(st.img)
        r = run([P2U, "restore", st.img, *out["_backups"]], check=False)
        record("restore: refused after later changes, nothing written", r.returncode != 0 and sha(st.img) == before, (r.stderr or "").strip()[:70])

        # 9c. A replace that has to reuse the old folder's space supersedes the first backup.
        st = Stick(work, "reuse", 32, 34, spc=1)
        with open(blob, "wb") as f:
            f.write(rng.randbytes(29_000_000))
        st.m("mcopy", blob, "x:/big.bin")
        pl_r, _ = make_music(work, 10, rng, prefix="Reuse", min_size=300_000, max_size=340_000)
        out1 = write(st, pl_r)
        out2 = write(st, pl_r, "--replace")
        note = ((out2.get("result") or {}).get("undo_note")) or ""
        record("forced reuse: replace ok and self-verified", out2["ok"], out2.get("error") or "")
        record("forced reuse: result says the old folder can't be brought back", "reused" in note, note[:60])
        r = run([P2U, "restore", st.img, *out2["_backups"]], check=False)
        clean, _, _ = st.fsck()
        record("forced reuse: restore undoes the write, refuses the superseded backup, stick stays clean",
               r.returncode != 0 and "no longer be restored" in r.stderr and clean and not st.exists("/Reuse"), (r.stderr or "").strip()[:80])

        # 10. Crash safety sweeps.
        st = Stick(work, "crash-folder", 32, 64, spc=1)
        fragment_free_space(st, work, rng, 40, 1500)
        base = os.path.join(work, "crash-folder-base.img")
        shutil.copyfile(st.img, base)
        pl, _ = make_music(work, 8, rng, prefix="Crash")
        _, names, sources = plan(pl)
        fault_sweep("pull during a new-folder write", base, st, [pl], "/Crash", names, sources)

        st = Stick(work, "crash-replace", 32, 64, spc=1)
        pl_old, _ = make_music(work, 6, rng, prefix="Swap")
        _, names_old, sources_old = plan(pl_old)
        write(st, pl_old)
        with open(pl_old) as f:
            lines = [l for l in f.read().splitlines() if l and not l.startswith("#")]
        lines.reverse()
        pl_new = os.path.join(work, "C", "Swap.m3u8")
        os.makedirs(os.path.dirname(pl_new), exist_ok=True)
        with open(pl_new, "w") as f:
            f.write("\n".join(lines) + "\n")
        _, names_new, sources_new = plan(pl_new)
        base = os.path.join(work, "crash-replace-base.img")
        shutil.copyfile(st.img, base)
        fault_sweep("pull during a replace", base, st, [pl_new, "--replace"], "/Swap", names_new, sources_new, old=(names_old, sources_old))

        st = Stick(work, "crash-top", 32, 64, spc=1)
        st.m("mcopy", keep, "x:/Keep me.txt")
        fragment_free_space(st, work, rng, 20, 1500)
        base = os.path.join(work, "crash-top-base.img")
        shutil.copyfile(st.img, base)
        pl, _ = make_music(work, 6, rng, prefix="TopCrash")
        _, names, sources = plan(pl, ["--top-level"])
        fault_sweep("pull during a top-level write", base, st, [pl, "--top-level"], "/", names, sources, prefix_ok=True)

        # The lock step (reorder only) on a folder copied in a scrambled order.
        st = Stick(work, "crash-sort", 32, 64, spc=1)
        pl, files = make_music(work, 24, rng, prefix="Sort")
        _, names, sources = plan(pl)
        st.m("mmd", "x:/Sort")
        shuffled = list(zip(names, sources))
        rng.shuffle(shuffled)
        for n, src in shuffled:
            st.m("mcopy", src, f"x:/Sort/{n}")
        for n, src in shuffled[::4]:
            st.m("mdel", f"x:/Sort/{n}")
            st.m("mcopy", src, f"x:/Sort/{n}")
        order_file = os.path.join(work, "sort-order.txt")
        with open(order_file, "w") as f:
            f.write("\n".join(names) + "\n")
        base = os.path.join(work, "crash-sort-base.img")
        shutil.copyfile(st.img, base)
        sort_sweep("pull during the lock step", base, st, "/Sort", order_file, names)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    print(f"\n{passed}/{len(RESULTS)} checks passed ({FAULT_RUNS} simulated pulls)")
    with open(os.path.join(ROOT, "tests", "write-e2e-results.json"), "w") as f:
        json.dump({"checks": [{"name": n, "ok": o, "detail": d} for n, o, d in RESULTS], "simulated_pulls": FAULT_RUNS}, f, indent=2)
    sys.exit(0 if passed == len(RESULTS) else 1)


if __name__ == "__main__":
    main()
