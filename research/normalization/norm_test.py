#!/usr/bin/env python3
"""Lossless MP3 gain: mp3rgain vs mp3gain, measured with ffmpeg (BS.1770 / EBU R128)."""
import hashlib, json, os, re, shutil, subprocess, sys
W = sys.argv[1]
R = sys.argv[2]  # mp3rgain binary
os.chdir(W)
def sh(*a, check=True):
    r = subprocess.run(a, capture_output=True, text=True)
    if check and r.returncode:
        raise SystemExit(f"{a[0]}: {r.stderr[-500:]}")
    return r
def pcm(f):
    return subprocess.run(["ffmpeg", "-v", "error", "-i", f, "-f", "f32le", "-ac", "2", "-"], capture_output=True).stdout
def md5pcm(f):
    return hashlib.md5(subprocess.run(["ffmpeg", "-v", "error", "-i", f, "-f", "s16le", "-"], capture_output=True).stdout).hexdigest()
def frames_md5(f):
    # audio frames only: strip ID3/APE tags with ffmpeg stream copy
    return hashlib.md5(subprocess.run(["ffmpeg", "-v", "error", "-i", f, "-map", "0:a", "-c", "copy", "-f", "mp3", "-write_xing", "0", "-id3v2_version", "0", "-"], capture_output=True).stdout).hexdigest()
def loud(f):
    e = sh("ffmpeg", "-hide_banner", "-nostats", "-i", f, "-af", "ebur128=peak=true", "-f", "null", "-").stderr
    I = float(re.findall(r"I:\s+(-?[\d.]+) LUFS", e)[-1])
    tp = float(re.findall(r"Peak:\s+(-?[\d.]+) dBFS", e)[-1])
    return I, tp
def sha(f):
    return hashlib.sha256(open(f, "rb").read()).hexdigest()
def errs(f):
    return sh("ffmpeg", "-v", "error", "-i", f, "-f", "null", "-", check=False).stderr.strip()

out = {}
# Music-like test signal: chords with an envelope plus filtered noise, 60 s stereo.
expr = ("0.25*sin(2*PI*220*t)*(0.6+0.4*sin(2*PI*0.5*t))+0.18*sin(2*PI*277.18*t)+0.15*sin(2*PI*329.63*t)"
        "+0.08*sin(2*PI*55*t)*(1+sin(2*PI*2*t))")
sh("ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", f"aevalsrc='{expr}|{expr.replace('220','221')}':s=44100:d=60",
   "-f", "lavfi", "-i", "anoisesrc=d=60:c=pink:a=0.08:r=44100", "-filter_complex", "[0][1]amix=inputs=2:weights=1 1,volume=-6dB", "-c:a", "pcm_s16le", "src.wav")
sh("ffmpeg", "-y", "-v", "error", "-i", "src.wav", "-af", "volume=+9dB,alimiter=limit=0.98", "-c:a", "pcm_s16le", "loud.wav")
sh("ffmpeg", "-y", "-v", "error", "-i", "src.wav", "-af", "volume=-14dB", "-c:a", "pcm_s16le", "quiet.wav")
sh("ffmpeg", "-y", "-v", "error", "-i", "src.wav", "-ac", "1", "-ar", "22050", "-c:a", "pcm_s16le", "mono22.wav")
enc = {
    "cbr320": ["lame", "-S", "-b", "320", "src.wav", "cbr320.mp3"],
    "cbr256": ["lame", "-S", "-b", "256", "src.wav", "cbr256.mp3"],
    "vbr_v0": ["lame", "-S", "-V", "0", "src.wav", "vbr_v0.mp3"],
    "loud320": ["lame", "-S", "-b", "320", "loud.wav", "loud320.mp3"],
    "quiet320": ["lame", "-S", "-b", "320", "quiet.wav", "quiet320.mp3"],
    "mpeg2_mono": ["lame", "-S", "-b", "64", "mono22.wav", "mpeg2_mono.mp3"],
}
for k, cmd in enc.items():
    sh(*cmd)

# 1. Fixed gain: +2 steps (+3.0 dB) and -2 steps, with both tools; compare decoded audio and loudness.
rows = []
for k in enc:
    f = f"{k}.mp3"
    I0, tp0 = loud(f)
    row = {"file": k, "I_before": I0, "truepeak_before": tp0}
    for tool in ("mp3rgain", "mp3gain"):
        g = f"{k}.{tool}.plus2.mp3"
        shutil.copy(f, g)
        if tool == "mp3rgain":
            sh(R, "-g", "2", "-c", g)
        else:
            sh("mp3gain", "-q", "-g", "2", "-c", g)
        I1, tp1 = loud(g)
        row[f"{tool}_dI"] = round(I1 - I0, 3)
        row[f"{tool}_pcm_md5"] = md5pcm(g)
        row[f"{tool}_decode_errors"] = errs(g)
        # undo
        u = f"{k}.{tool}.undo.mp3"
        shutil.copy(g, u)
        if tool == "mp3rgain":
            sh(R, "-u", u)
        else:
            sh("mp3gain", "-q", "-u", u)
        row[f"{tool}_undo_pcm_identical"] = md5pcm(u) == md5pcm(f)
        row[f"{tool}_undo_file_identical"] = sha(u) == sha(f)
        row[f"{tool}_undo_frames_identical"] = frames_md5(u) == frames_md5(f)
    row["tools_agree_pcm"] = row["mp3rgain_pcm_md5"] == row["mp3gain_pcm_md5"]
    # Exact scale check: decoded samples after +2 steps vs 2^(2/4) * before
    import array
    a = array.array("f"); a.frombytes(pcm(f))
    b = array.array("f"); b.frombytes(pcm(f"{k}.mp3rgain.plus2.mp3"))
    n = min(len(a), len(b))
    num = sum(x * y for x, y in zip(a[:n:7], b[:n:7])); den = sum(x * x for x in a[:n:7])
    row["amplitude_ratio"] = round(num / den, 5)
    row["expected_ratio"] = round(2 ** 0.5, 5)
    rows.append(row)
out["fixed_gain"] = rows

# 2. Normalise to ReplayGain 2.0 (-18 LUFS) in track mode with clipping prevention.
rg = []
for k in ("cbr320", "loud320", "quiet320", "vbr_v0", "mpeg2_mono"):
    f = f"{k}.mp3"
    g = f"{k}.rg2.mp3"
    shutil.copy(f, g)
    I0, tp0 = loud(f)
    r = sh(R, "-r", "--rg2", "-k", "-c", g, check=False)
    I1, tp1 = loud(g)
    rg.append({"file": k, "I_before": I0, "I_after": I1, "truepeak_after": tp1, "steps_dB": round(I1 - I0, 2),
               "off_target_dB": round(I1 + 18, 2), "rc": r.returncode, "out": (r.stdout + r.stderr).strip().splitlines()[-2:]})
out["rg2_track"] = rg

# 3. The re-encode path for lossless sources: exact gain in PCM, then LAME 256k CBR.
I0, _ = loud("src.wav")
gain = -18 - I0
sh("ffmpeg", "-y", "-v", "error", "-i", "src.wav", "-af", f"volume={gain:.2f}dB", "-c:a", "pcm_s16le", "src_norm.wav")
sh("lame", "-S", "-b", "256", "src_norm.wav", "enc_norm256.mp3")
I1, tp1 = loud("enc_norm256.mp3")
out["reencode_path"] = {"I_source": I0, "gain_applied": round(gain, 2), "I_after_encode": I1, "off_target_dB": round(I1 + 18, 2), "truepeak": tp1}
print(json.dumps(out, indent=1))
