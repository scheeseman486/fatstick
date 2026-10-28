// Reads tags and stream facts from audio files dropped into the browser
// version of the prototype: ID3v2 and the first MP3 frames, FLAC STREAMINFO and
// Vorbis comments. The app proper gets all of this from ffprobe; this only
// makes the browser demo show your own files with their own tags.
"use strict";
(function (Fatstick) {
  const EXT = {
    mp3: "mp3", flac: "flac", m4a: "aac", aac: "aac", mp4: "aac", ogg: "vorbis", oga: "vorbis", opus: "opus",
    wma: "wma", wav: "wav", aif: "aiff", aiff: "aiff", wv: "wavpack", ape: "ape",
  };
  const PLAYLIST = /\.(m3u8?|xspf|pls)$/i;
  const BR1 = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
  const BR2 = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
  const SR = { 3: [44100, 48000, 32000], 2: [22050, 24000, 16000], 0: [11025, 12000, 8000] };

  function ext(name) {
    const m = /\.([a-z0-9]+)$/i.exec(name);
    return m ? m[1].toLowerCase() : "";
  }
  function stem(name) {
    return name.replace(/^.*[\\/]/, "").replace(/\.[^.]+$/, "");
  }

  function text(bytes, enc) {
    try {
      if (enc === 0) return new TextDecoder("latin1").decode(bytes);
      if (enc === 1) return new TextDecoder(bytes[0] === 0xfe ? "utf-16be" : "utf-16le").decode(bytes.subarray(2));
      if (enc === 2) return new TextDecoder("utf-16be").decode(bytes);
      return new TextDecoder("utf-8").decode(bytes);
    } catch {
      return "";
    }
  }
  const clean = (s) => s.replace(/\u0000.*$/s, "").trim();

  function id3(buf, tags) {
    const b = new Uint8Array(buf);
    if (b.length < 10 || b[0] !== 0x49 || b[1] !== 0x44 || b[2] !== 0x33) return 0;
    const ver = b[3];
    const size = ((b[6] & 0x7f) << 21) | ((b[7] & 0x7f) << 14) | ((b[8] & 0x7f) << 7) | (b[9] & 0x7f);
    const end = Math.min(b.length, 10 + size);
    const MAP = ver === 2
      ? { TT2: "title", TP1: "artist", TAL: "album", TP2: "albumartist", TRK: "track", TPA: "disc", TYE: "year", TCO: "genre" }
      : { TIT2: "title", TPE1: "artist", TALB: "album", TPE2: "albumartist", TRCK: "track", TPOS: "disc", TYER: "year", TDRC: "year", TCON: "genre" };
    let p = 10;
    while (p + (ver === 2 ? 6 : 10) <= end) {
      let id, len;
      if (ver === 2) {
        id = String.fromCharCode(b[p], b[p + 1], b[p + 2]);
        len = (b[p + 3] << 16) | (b[p + 4] << 8) | b[p + 5];
        p += 6;
      } else {
        id = String.fromCharCode(b[p], b[p + 1], b[p + 2], b[p + 3]);
        len = ver === 4
          ? ((b[p + 4] & 0x7f) << 21) | ((b[p + 5] & 0x7f) << 14) | ((b[p + 6] & 0x7f) << 7) | (b[p + 7] & 0x7f)
          : (b[p + 4] << 24) | (b[p + 5] << 16) | (b[p + 6] << 8) | b[p + 7];
        p += 10;
      }
      if (!/^[A-Z0-9]+$/.test(id) || len <= 0 || p + len > end) break;
      if (id === "APIC" || id === "PIC") tags.cover = true;
      const key = MAP[id];
      if (key) {
        const v = clean(text(b.subarray(p + 1, p + len), b[p]));
        if (v) tags[key] = key === "track" || key === "disc" ? parseInt(v, 10) || null : key === "year" ? parseInt(v.slice(0, 4), 10) || null : v;
      }
      p += len;
    }
    return 10 + size;
  }

  function mp3Frames(buf, start, info) {
    const b = new Uint8Array(buf);
    let p = start;
    let frames = 0;
    let max = 0;
    let first = null;
    while (p + 4 < b.length && frames < 3000) {
      if (b[p] !== 0xff || (b[p + 1] & 0xe0) !== 0xe0) {
        if (frames === 0) { p++; continue; }
        break;
      }
      const verBits = (b[p + 1] >> 3) & 3;
      const layer = (b[p + 1] >> 1) & 3;
      const bri = b[p + 2] >> 4;
      const sri = (b[p + 2] >> 2) & 3;
      if (verBits === 1 || layer !== 1 || bri === 0 || bri === 15 || sri === 3) { p++; continue; }
      const br = (verBits === 3 ? BR1 : BR2)[bri];
      const sr = SR[verBits][sri];
      const pad = (b[p + 2] >> 1) & 1;
      const len = Math.floor(((verBits === 3 ? 144 : 72) * br * 1000) / sr) + pad;
      if (!first) {
        first = { br, sr, ch: (b[p + 3] >> 6) === 3 ? 1 : 2 };
        const side = verBits === 3 ? (first.ch === 1 ? 17 : 32) : first.ch === 1 ? 9 : 17;
        const tag = String.fromCharCode(...b.subarray(p + 4 + side, p + 8 + side));
        if (tag === "Xing") info.vbr = true;
        if (tag === "Xing" || tag === "Info") { p += len; continue; }
      }
      max = Math.max(max, br);
      frames++;
      p += len;
    }
    if (first) {
      info.sampleRate = first.sr;
      info.channels = first.ch;
      info.peakBitrate = max || first.br;
      if (!info.vbr) info.bitrate = first.br;
    }
  }

  function flac(buf, tags, info) {
    const b = new Uint8Array(buf);
    if (String.fromCharCode(b[0], b[1], b[2], b[3]) !== "fLaC") return;
    let p = 4;
    let last = false;
    const dv = new DataView(buf);
    while (!last && p + 4 <= b.length) {
      last = (b[p] & 0x80) !== 0;
      const type = b[p] & 0x7f;
      const len = (b[p + 1] << 16) | (b[p + 2] << 8) | b[p + 3];
      p += 4;
      if (type === 0 && p + 18 <= b.length) {
        info.sampleRate = (b[p + 10] << 12) | (b[p + 11] << 4) | (b[p + 12] >> 4);
        info.channels = ((b[p + 12] >> 1) & 7) + 1;
        info.bits = (((b[p + 12] & 1) << 4) | (b[p + 13] >> 4)) + 1;
        const total = (b[p + 13] & 0x0f) * 2 ** 32 + dv.getUint32(p + 14);
        if (total && info.sampleRate) info.duration = total / info.sampleRate;
      } else if (type === 4 && p + len <= b.length) {
        let q = p + 4 + dv.getUint32(p, true);
        const count = dv.getUint32(q, true);
        q += 4;
        const MAP = { TITLE: "title", ARTIST: "artist", ALBUM: "album", ALBUMARTIST: "albumartist", TRACKNUMBER: "track", DISCNUMBER: "disc", DATE: "year", GENRE: "genre" };
        for (let k = 0; k < count && q + 4 <= p + len; k++) {
          const l = dv.getUint32(q, true);
          const s = new TextDecoder().decode(b.subarray(q + 4, q + 4 + l));
          q += 4 + l;
          const eq = s.indexOf("=");
          const key = MAP[s.slice(0, eq).toUpperCase()];
          if (key && !tags[key]) {
            const v = s.slice(eq + 1).trim();
            tags[key] = key === "track" || key === "disc" || key === "year" ? parseInt(v, 10) || null : v;
          }
        }
      } else if (type === 6) tags.cover = true;
      p += len;
    }
  }

  function duration(url) {
    return new Promise((resolve) => {
      const a = new Audio();
      a.preload = "metadata";
      const done = (v) => { a.removeAttribute("src"); resolve(v); };
      a.onloadedmetadata = () => done(Number.isFinite(a.duration) ? a.duration : null);
      a.onerror = () => done(null);
      setTimeout(() => done(null), 4000);
      a.src = url;
    });
  }

  /** A track record (same shape as the demo tracks) for a dropped File. */
  async function fromFile(file, id) {
    const codec = EXT[ext(file.name)];
    if (!codec) return null;
    const tags = {};
    const info = { codec, bitrate: 0, peakBitrate: 0, vbr: false, sampleRate: 44100, bits: 16, channels: 2, duration: null };
    const head = await file.slice(0, 1024 * 1024).arrayBuffer();
    if (codec === "mp3") mp3Frames(head, id3(head, tags), info);
    else if (codec === "flac") flac(head, tags, info);
    else id3(head, tags);
    const url = URL.createObjectURL(file);
    if (!info.duration) info.duration = await duration(url);
    if (!info.duration && info.bitrate) info.duration = (file.size * 8) / (info.bitrate * 1000);
    if (info.vbr && info.duration) info.bitrate = Math.round((file.size * 8) / info.duration / 1000);
    if (!info.bitrate && ["aac", "opus", "vorbis", "wma"].includes(codec) && info.duration) {
      info.bitrate = Math.round((file.size * 8) / info.duration / 1000 / 32) * 32 || 128;
      info.peakBitrate = info.bitrate;
    }
    const path = file.webkitRelativePath || file.name;
    return {
      id, title: tags.title || "", artist: tags.artist || "", album: tags.album || "", albumartist: tags.albumartist || tags.artist || "",
      year: tags.year || null, track: tags.track || null, disc: tags.disc || null, genre: tags.genre || "",
      ...info, duration: info.duration || 0, size: file.size, cover: !!tags.cover, exists: true, drm: false,
      lufs: null, truePeak: null, filename: stem(file.name), path, url, local: true,
    };
  }

  Fatstick.sniff = { fromFile, isPlaylist: (n) => PLAYLIST.test(n), isAudio: (n) => !!EXT[ext(n)], codecFor: (n) => EXT[ext(n)], stem };
})(window.Fatstick = window.Fatstick || {});
