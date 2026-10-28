// What a sync will do with each track (copy or encode, and the gain), how big
// the result is, and whether it fits a FAT32 stick: the rules from the design
// doc, so the window can show them as you edit. The app's sync checks every
// copy against every frame header again and encodes anything the profile
// can't take. Pure functions, no DOM.
"use strict";
(function (P2U) {
  const LOSSLESS = new Set(["flac", "alac", "wav", "aiff", "wavpack", "ape", "wmalossless"]);
  const CODEC_NAME = {
    mp3: "MP3", flac: "FLAC", alac: "ALAC", wav: "WAV", aiff: "AIFF", wavpack: "WavPack", ape: "APE",
    aac: "AAC", vorbis: "Vorbis", opus: "Opus", wma: "WMA", wmapro: "WMA Pro", wmalossless: "WMA Lossless",
  };
  const STEP = 1.5; // dB per global_gain step
  const TAG_BYTES = 4 * 1024; // ID3v2.3 text frames, padded
  const COVER_BYTES = 60 * 1024; // a small baseline JPEG, typical

  function formatLabel(t) {
    if (!t.codec) return t.exists === false ? "" : "Not read";
    const name = CODEC_NAME[t.codec] || t.codec.toUpperCase();
    if (LOSSLESS.has(t.codec)) return `${name} ${t.bits || 16}/${trimKHz(t.sampleRate)}`;
    if (t.codec === "mp3" && t.vbr) return `MP3 VBR ${t.bitrate}`;
    return `${name} ${t.bitrate}`;
  }
  function trimKHz(hz) {
    const k = hz / 1000;
    return Number.isInteger(k) ? String(k) : k.toFixed(1);
  }

  /** Copy or encode, and why. */
  function decide(t, profile) {
    if (!t.exists) return { action: "skip", reason: "File not found" };
    if (t.drm) return { action: "skip", reason: "Copy-protected, can't be decoded" };
    if (!t.codec) return { action: "skip", reason: t.error ? `Couldn't be read: ${t.error}` : "Not read yet" };
    const cap = profile.bitrateCap;
    const notes = [];
    if (t.channels > 2) notes.push(`downmixed from ${t.channels} channels`);
    if (t.sampleRate > 48000) notes.push(`resampled from ${trimKHz(t.sampleRate)} kHz`);
    if (t.codec === "mp3") {
      const rateOk = profile.sampleRates.includes(t.sampleRate);
      const peak = t.peakBitrate || t.bitrate;
      if (rateOk && peak <= cap) return { action: "copy", reason: "MP3 within the limits, copied bit for bit", notes };
      if (!rateOk) return { action: "encode", reason: `${trimKHz(t.sampleRate)} kHz MP3 isn't allowed by this profile`, notes };
      return { action: "encode", reason: t.vbr ? `VBR frames reach ${peak} kbps, over the ${cap} cap` : `${peak} kbps is over the ${cap} kbps cap`, notes };
    }
    if (LOSSLESS.has(t.codec)) return { action: "encode", reason: `Lossless, encoded once at ${cap} kbps`, notes };
    return { action: "encode", lossy: true, reason: `${CODEC_NAME[t.codec]} isn't played by the player: lossy to lossy at ${cap} kbps`, notes };
  }

  /**
   * Gain per entry for a playlist's normalization setting.
   * Copies get whole 1.5 dB steps, stopped short of full scale; encodes get the
   * exact gain, limited to a -1 dBTP true peak.
   */
  function gains(tracks, decisions, norm) {
    if (!norm || !norm.on) return tracks.map(() => null);
    const target = norm.target;
    const loud = tracks.map((t) => t.lufs);
    if (norm.mode === "album") {
      // Consecutive tracks from one album share one gain.
      let i = 0;
      while (i < tracks.length) {
        let j = i + 1;
        while (j < tracks.length && tracks[j].album && tracks[j].album === tracks[i].album) j++;
        const known = tracks.slice(i, j).filter((t) => t.lufs != null);
        if (known.length) {
          const avg = 10 * Math.log10(known.reduce((a, t) => a + Math.pow(10, t.lufs / 10), 0) / known.length);
          for (let k = i; k < j; k++) if (loud[k] != null) loud[k] = avg;
        }
        i = j;
      }
    }
    return tracks.map((t, i) => {
      const d = decisions[i];
      if (d.action === "skip") return null;
      if (loud[i] == null) return { db: null, unknown: true };
      const want = target - loud[i];
      if (d.action === "copy") {
        let steps = Math.round(want / STEP);
        let limited = false;
        while (steps > 0 && t.truePeak + steps * STEP >= 0) { steps--; limited = true; }
        return { db: steps * STEP, exact: false, limited, want };
      }
      let db = want;
      let limited = false;
      if (t.truePeak + db > -1) { db = -1 - t.truePeak; limited = true; }
      return { db: Math.round(db * 10) / 10, exact: true, limited, want };
    });
  }

  /** Output sample rate for an encode: kept if allowed and MPEG-1 can carry it, else 44.1 kHz. */
  function outputRate(source, allowed) {
    if ([44100, 48000, 32000].includes(source) && allowed.includes(source)) return source;
    return allowed.includes(44100) || !allowed.length ? 44100 : allowed[0];
  }

  /** A track's cut and silence at either end on the stick (seconds), from its playlist entry. */
  function edges(e) {
    const n = (x) => (Number.isFinite(+x) && +x > 0 ? Math.round(+x * 1000) / 1000 : 0);
    return { cutStart: n(e && e.cutStart), cutEnd: n(e && e.cutEnd), padStart: n(e && e.padStart), padEnd: n(e && e.padEnd) };
  }
  function hasEdges(e) {
    const x = edges(e);
    return !!(x.cutStart || x.cutEnd || x.padStart || x.padEnd);
  }
  /** A track's key on the stick: semitones up or down (cents as hundredths), and
   * whether voices keep their natural character (formants), from its entry. */
  function key(e) {
    const p = Number(e && e.pitch);
    const pitch = Number.isFinite(p) ? Math.round(Math.max(-24, Math.min(24, p)) * 100) / 100 : 0;
    return { pitch, keepFormants: !(e && e.keepFormants === false) };
  }
  function hasKey(e) {
    return key(e).pitch !== 0;
  }
  /** An entry whose track is rendered, not copied: cut, padded or in another key. */
  function reworks(e) {
    return hasEdges(e) || hasKey(e);
  }
  /** How long a track is on the stick: cut at its ends, then padded. */
  function stickSeconds(t, e) {
    const x = edges(e);
    return Math.max(0, (t.duration || 0) - x.cutStart - x.cutEnd) + x.padStart + x.padEnd;
  }
  /** One part of a rendered file: a track with its gain, cuts and silences. */
  function part(t, e, g) {
    const k = key(e);
    return { trackId: t.id, gainDb: g && g.db ? Math.round(g.db * 10) / 10 : 0, ...edges(e), ...(k.pitch ? k : {}) };
  }
  /** A file rendered from parts: a cut or padded track, silence, a joined playlist. */
  function render(parts, profile, rate) {
    return { kind: "render", cap: profile.bitrateCap, vbr: profile.mode === "vbr", sampleRate: rate || outputRate(44100, profile.sampleRates), parts };
  }

  /**
   * The recipe the app's preparation and sync take: a plain copy, a copy with
   * lossless gain steps, an encode (with the exact gain), or, for a track cut
   * or padded at its ends or in another key (entry `e`), a render.
   */
  function recipe(t, d, g, profile, e) {
    if (d.action === "skip") return null;
    if (d.action === "copy") {
      const steps = g && g.db ? Math.round(g.db / STEP) : 0;
      return steps ? { kind: "gain", steps } : { kind: "copy" };
    }
    const rate = outputRate(t.sampleRate, profile.sampleRates);
    if (e && reworks(e)) return render([part(t, e, g)], profile, rate);
    return {
      kind: "encode", cap: profile.bitrateCap, vbr: profile.mode === "vbr",
      sampleRate: rate, gainDb: g && g.db ? Math.round(g.db * 10) / 10 : 0,
    };
  }
  function describe(r) {
    if (!r) return "";
    if (r.kind === "copy") return "copy";
    if (r.kind === "gain") return `gain${r.steps > 0 ? "+" : ""}${r.steps}`;
    if (r.kind === "render") {
      const ps = r.parts.map((x) => `${x.trackId || "-"}:${x.gainDb || 0}:${x.cutStart || 0}:${x.cutEnd || 0}:${x.padStart || 0}:${x.padEnd || 0}${x.pitch ? `:k${x.pitch}${x.keepFormants ? "f" : ""}` : ""}`).join(",");
      return `render/${r.vbr ? "vbr" : "cbr"}${r.cap}/${r.sampleRate}/${hash(ps)}`;
    }
    return `${r.vbr ? "vbr" : "cbr"}${r.cap}/${r.sampleRate}/${r.gainDb}`;
  }
  /** A short, stable hash of a string (cyrb53), for keys. */
  function hash(str) {
    let h1 = 0xdeadbeef;
    let h2 = 0x41c6ce57;
    for (let i = 0; i < str.length; i++) {
      const ch = str.charCodeAt(i);
      h1 = Math.imul(h1 ^ ch, 2654435761);
      h2 = Math.imul(h2 ^ ch, 1597334677);
    }
    h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
    h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
    return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36);
  }

  /** Bytes the file takes on the stick: tag, cover, audio (`secs`: its length on the stick). */
  function outBytes(t, d, profile, secs) {
    const tag = TAG_BYTES + (profile.cover === "none" || !t.cover ? 0 : COVER_BYTES) + (profile.id3v1 ? 128 : 0);
    if (d.action === "skip") return 0;
    if (d.action === "copy") return Math.round(t.size * 0.985) + tag; // the source's own tags are dropped (estimate)
    return Math.round((profile.bitrateCap * 1000 / 8) * (secs == null ? t.duration : secs)) + tag;
  }

  // FAT32 geometry, as the stick writer lays it out (Microsoft's cluster table,
  // first data cluster aligned to 4 MiB, or 1 MiB under 2 GiB).
  function geometry(diskBytes) {
    const MIB = 1 << 20;
    const partStart = MIB;
    const sectors = (diskBytes - partStart) / 512;
    const spc = sectors <= 532480 ? 1 : sectors <= 16777216 ? 8 : sectors <= 33554432 ? 16 : sectors <= 67108864 ? 32 : 64;
    const cluster = spc * 512;
    const clusters = Math.floor((diskBytes - partStart) / cluster);
    const fatBytes = Math.ceil(((clusters + 2) * 4) / 512) * 512;
    const align = diskBytes >= 2 * 1024 * MIB ? 4 * MIB : MIB;
    const dataStart = Math.ceil((partStart + 32 * 512 + 2 * fatBytes) / align) * align;
    const dataBytes = Math.floor((diskBytes - dataStart) / cluster) * cluster;
    return { cluster, dataBytes, overhead: dataStart };
  }

  /** Layout of the stick: folders and files back to back, in stick order. */
  function layout(diskBytes, folders) {
    const g = geometry(diskBytes);
    const round = (b) => Math.max(g.cluster, Math.ceil(b / g.cluster) * g.cluster);
    let used = round(64 * 32); // root folder table
    const runs = [];
    for (const f of folders) {
      used += f.top ? 0 : round((f.files.length + 2) * 32 * 4); // folder table with long names
      for (const file of f.files) {
        const size = round(file.bytes);
        runs.push({ start: used, size, folder: f.index, entry: file.entry });
        used += size;
      }
    }
    return { ...g, used, free: g.dataBytes - used, fits: used <= g.dataBytes, runs };
  }

  P2U.rules = {
    LOSSLESS, CODEC_NAME, formatLabel, decide, gains, outBytes, geometry, layout, outputRate, recipe, describe, STEP,
    edges, hasEdges, key, hasKey, reworks, stickSeconds, part, render, hash,
  };
})(typeof window !== "undefined" ? (window.P2U = window.P2U || {}) : (globalThis.P2U = globalThis.P2U || {}));
