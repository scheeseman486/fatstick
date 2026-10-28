// File and folder names on the stick: the template language from the design
// doc, the FAT clean-up rules, and the notes the preview shows. Pure functions,
// no DOM, so the same code runs in node for tests (tests/ui_names.js).
"use strict";
(function (Fatstick) {
  const FIELDS = ["n", "title", "artist", "album", "albumartist", "year", "genre", "track", "disc", "filename", "playlist"];
  const FORBIDDEN = /["*/:<>?\\|\u0000-\u001f\u007f]/g;
  const DEVICE = /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/i;
  const FOLD = {
    "ß": "ss", "æ": "ae", "Æ": "AE", "ø": "o", "Ø": "O", "œ": "oe", "Œ": "OE", "ł": "l", "Ł": "L",
    "đ": "d", "Đ": "D", "ð": "d", "Ð": "D", "þ": "th", "Þ": "Th", "ı": "i",
    "‘": "'", "’": "'", "‚": "'", "“": '"', "”": '"', "„": '"', "–": "-", "—": "-", "…": "...", "×": "x",
  };
  const LABEL = { filename: "the file name", n: "the position" };

  /** Parse a template into nodes: text, field (with alternatives) and [optional] groups. */
  function parse(tpl) {
    const errors = [];
    let i = 0;
    function seq(inGroup) {
      const nodes = [];
      let text = "";
      const flush = () => { if (text) nodes.push({ t: "text", v: text }); text = ""; };
      while (i < tpl.length) {
        const c = tpl[i];
        if (c === "]" && inGroup) { i++; flush(); return nodes; }
        if (c === "{") {
          const j = tpl.indexOf("}", i);
          if (j < 0) { errors.push("A { has no closing }."); text += tpl.slice(i); i = tpl.length; break; }
          flush();
          const alts = tpl.slice(i + 1, j).split("|").map((a) => {
            const [name, pad] = a.trim().split(":");
            const key = (name || "").toLowerCase();
            if (!FIELDS.includes(key)) errors.push(`{${a.trim()}} isn't a field.`);
            const p = pad === undefined ? null : parseInt(pad, 10);
            return { name: key, pad: Number.isFinite(p) ? Math.min(Math.max(p, 1), 6) : null };
          });
          nodes.push({ t: "field", alts });
          i = j + 1;
          continue;
        }
        if (c === "[") { i++; flush(); nodes.push({ t: "group", kids: seq(true) }); continue; }
        text += c;
        i++;
      }
      if (inGroup) errors.push("A [ has no closing ].");
      flush();
      return nodes;
    }
    const nodes = seq(false);
    return { nodes, errors: [...new Set(errors)] };
  }

  function usesField(nodes, name) {
    return nodes.some((n) => (n.t === "field" && n.alts.some((a) => a.name === name)) || (n.t === "group" && usesField(n.kids, name)));
  }

  function value(alt, ctx) {
    const v = ctx[alt.name];
    if (v === undefined || v === null || v === "") return "";
    if (alt.name === "n") return String(v).padStart(alt.pad || ctx.nPad || 2, "0");
    if (alt.name === "track" || alt.name === "disc") return String(v).padStart(alt.pad || 2, "0");
    return String(v).trim();
  }

  function evaluate(nodes, ctx, notes) {
    let out = "";
    let missing = false;
    for (const n of nodes) {
      if (n.t === "text") out += n.v;
      else if (n.t === "field") {
        let got = "";
        for (let k = 0; k < n.alts.length; k++) {
          got = value(n.alts[k], ctx);
          if (got) {
            if (k > 0) notes.push(`no ${n.alts[0].name} tag, used ${LABEL[n.alts[k].name] || n.alts[k].name}`);
            break;
          }
        }
        if (!got) missing = true;
        out += got;
      } else {
        const inner = [];
        const r = evaluate(n.kids, ctx, inner);
        if (!r.missing) { out += r.text; notes.push(...inner); }
      }
    }
    return { text: out, missing };
  }

  function asciiFold(s) {
    let note = null;
    let out = "";
    for (const ch of s) {
      let f = FOLD[ch] ?? ch.normalize("NFD").replace(/[̀-ͯ]/g, "");
      if (/[^\x20-\x7e]/.test(f)) f = "_";
      if (f !== ch && !note) note = `${ch} became ${f}`;
      out += f;
    }
    return { text: out, note };
  }

  function shorten(base, limit) {
    if (base.length <= limit) return base;
    let cut = base.slice(0, limit);
    const sp = cut.lastIndexOf(" ");
    if (base[limit] !== " " && sp >= limit * 0.6) cut = cut.slice(0, sp);
    return cut.replace(/[\s\-_.,(&]+$/, "");
  }

  /** Clean one name: FAT-forbidden characters, ASCII, spaces, device names, length. */
  function clean(raw, opts, notes) {
    let s = raw;
    if (opts.ascii) {
      const f = asciiFold(s);
      if (f.note) notes.push(f.note);
      s = f.text;
    }
    const r = s.replace(FORBIDDEN, "_");
    if (r !== s) notes.push("characters FAT can't store became _");
    s = r.replace(/\s+/g, " ").replace(/^ +/, "").replace(/[. ]+$/, "");
    const stem = s.split(".")[0];
    if (DEVICE.test(stem)) {
      s = stem + "_" + s.slice(stem.length);
      notes.push(`${stem} is reserved on Windows, added _`);
    }
    const ext = opts.ext || "";
    if (opts.maxLen && s.length + ext.length > opts.maxLen) {
      s = shorten(s, Math.max(4, opts.maxLen - ext.length));
      notes.push("shortened");
    }
    return s;
  }

  /**
   * Names for one folder (or the top level) of the stick.
   * entries: [{ tags: {title, artist, ...}, filename }]; returns [{ name, notes }].
   */
  function fileNames(entries, opts) {
    const { nodes, errors } = parse(opts.template || "");
    const nPad = Math.max(2, String(entries.length).length);
    const seen = new Map();
    const out = entries.map((e, i) => {
      const notes = [];
      const ctx = { ...e.tags, filename: e.filename, n: i + 1, nPad, playlist: opts.playlist || "" };
      let base = clean(evaluate(nodes, ctx, notes).text, { ...opts, ext: ".mp3" }, notes);
      if (!base) {
        base = "Track " + String(i + 1).padStart(nPad, "0");
        notes.push("the template gave an empty name, used the position");
      }
      let name = base + ".mp3";
      const key = name.toLowerCase();
      if (seen.has(key)) {
        let k = 2;
        while (seen.has(`${base} (${k}).mp3`.toLowerCase())) k++;
        name = `${base} (${k}).mp3`;
        notes.push(`renamed: same name as track ${seen.get(key) + 1}`);
      }
      seen.set(name.toLowerCase(), i);
      return { name, notes: [...new Set(notes)] };
    });
    return { names: out, errors, numbered: usesField(nodes, "n") };
  }

  /**
   * New names for files in the music folder (the library's file name clean-up):
   * the same template language and clean-up rules, each file keeping its own
   * extension and folder. {n} is the track number here.
   * entries: [{ tags, filename, ext: ".flac", dir }]; returns [{ name, notes }].
   */
  function libraryNames(entries, opts) {
    const { nodes, errors } = parse(opts.template || "");
    const seen = new Set();
    const names = entries.map((e) => {
      const notes = [];
      const ctx = { ...e.tags, filename: e.filename, n: e.tags.track || "", nPad: 2, playlist: "" };
      let base = clean(evaluate(nodes, ctx, notes).text, { ...opts, ext: e.ext }, notes);
      if (!base) {
        base = e.filename;
        notes.push("the template gave an empty name, kept the old one");
      }
      let name = base + e.ext;
      const key = (n) => `${e.dir}\u0000${n.toLowerCase()}`;
      if (seen.has(key(name))) {
        let k = 2;
        while (seen.has(key(`${base} (${k})${e.ext}`))) k++;
        name = `${base} (${k})${e.ext}`;
        notes.push("another file in its folder gets the same name, added a number");
      }
      seen.add(key(name));
      return { name, notes: [...new Set(notes)] };
    });
    return { names, errors };
  }

  /** Folder names for the playlists on one stick, in stick order. */
  function folderNames(playlists, opts) {
    const { nodes, errors } = parse(opts.template || "{playlist}");
    const nPad = Math.max(2, String(playlists.length).length);
    const seen = new Set();
    const names = playlists.map((p, i) => {
      const notes = [];
      let name = clean(evaluate(nodes, { playlist: p.name, n: i + 1, nPad }, notes).text, opts, notes) || `Playlist ${i + 1}`;
      let k = 2;
      const base = name;
      while (seen.has(name.toLowerCase())) name = `${base} (${k++})`;
      seen.add(name.toLowerCase());
      return { name, notes };
    });
    return { names, errors };
  }

  /** 8.3-safe volume label: up to 11 characters, A-Z 0-9 and a few symbols. */
  function volumeLabel(s) {
    return asciiFold(s || "").text.toUpperCase().replace(/[^A-Z0-9 _\-!#$%&'()@^`{}~]/g, "").trim().slice(0, 11);
  }

  Fatstick.names = { FIELDS, parse, fileNames, folderNames, libraryNames, volumeLabel, asciiFold };
})(typeof window !== "undefined" ? (window.Fatstick = window.Fatstick || {}) : (globalThis.Fatstick = globalThis.Fatstick || {}));
