// The music library: your music folder, searched, browsed by tag and tagged.
//
// * The library view (the Library button, top right) takes the middle of the
//   window: the folder and its scan, tags to browse by (your labels, genres,
//   artists, albums, decades, and what's missing), the tracks as a table to
//   filter, sort and select, and a tag editor for the selection. Tags are
//   written into the files after a preview, and file names can be cleaned up
//   from the tags the same way. Drag tracks onto a playlist on the left.
// * The search panel (the search button, bottom left) widens the sidebar:
//   type a title, an artist, a label or anything else; exact matches come
//   first, then a divider, then close ones. Drag songs into the playlist.
// * A playlist row shows its tags when clicked: tagChips() draws them.
//
// Nothing here writes a file outside the music folder, and the app's Rust
// side checks that again before it touches one.
"use strict";
(function (Fatstick) {
  const { h, fill, icon, clock, length, plural, ago, menu, modal, closeModal, toast, showTip, hideTip } = Fatstick.ui;
  const SR = Fatstick.search;
  const N = Fatstick.names;

  /** Library table row height, px (the table only draws the rows in view). */
  const RH = 30;
  const FIELD_LABEL = {
    title: "Title", artist: "Artist", albumartist: "Album artist", album: "Album", track: "Track", disc: "Disc",
    year: "Year", genre: "Genre", composer: "Composer",
  };
  /** The tag editor's fields, in order. */
  const EDIT_FIELDS = ["title", "artist", "album", "albumartist", "genre", "year", "track", "disc", "composer"];
  const NUMERIC = { year: 9999, track: 999, disc: 99 };
  const NAME_PRESETS = [
    { label: "01 - Title", tpl: "[{track} - ]{title|filename}" },
    { label: "01 - Artist - Title", tpl: "[{track} - ][{artist} - ]{title|filename}" },
    { label: "Artist - Title", tpl: "[{artist} - ]{title|filename}" },
    { label: "Title", tpl: "{title|filename}" },
  ];
  const MISSING = [
    { key: "title", label: "No title", test: (t) => !t.title },
    { key: "artist", label: "No artist", test: (t) => !t.artist },
    { key: "album", label: "No album", test: (t) => !t.album },
    { key: "genre", label: "No genre", test: (t) => !t.genre },
    { key: "year", label: "No year", test: (t) => !t.year },
    { key: "labels", label: "No labels", test: (t) => !(t.labels && t.labels.length) },
    { key: "file", label: "File not found", test: (t) => !t.exists },
  ];
  const GROUPS = [
    { field: "labels", title: "Labels", show: 14 },
    { field: "genre", title: "Genres", show: 8 },
    { field: "artist", title: "Artists", show: 8 },
    { field: "album", title: "Albums", show: 6 },
    { field: "decade", title: "Decades", show: 10, byValue: true },
    { field: "missing", title: "Missing tags", show: 10, fixed: true },
  ];
  /** Fields where every chosen tag must match (a track has many); the others match any. */
  const EVERY = new Set(["labels", "missing"]);
  const FILTER_NAME = { labels: "Label", genre: "Genre", artist: "Artist", album: "Album", decade: "Decade", missing: "" };

  const LCOLS = {
    title: { label: "Title", w: "minmax(8em, 2fr)" },
    artist: { label: "Artist", w: "minmax(6em, 1.3fr)" },
    album: { label: "Album", w: "minmax(6em, 1.2fr)" },
    track: { label: "#", w: "2.3em", right: true },
    year: { label: "Year", w: "3.2em", right: true },
    genre: { label: "Genre", w: "minmax(5em, 0.9fr)" },
    labels: { label: "Labels", w: "minmax(6em, 1.3fr)" },
    time: { label: "Time", w: "3.4em", right: true },
  };
  const LCOL_ORDER = ["title", "artist", "album", "track", "year", "genre", "labels", "time"];

  const lkey = (l) => String(l).trim().toLowerCase();
  const titleOf = (t) => t.title || t.filename || "";
  /** A field as text, the way the editor shows it ("" for none). */
  const val = (t, f) => (f in NUMERIC ? (t[f] ? String(t[f]) : "") : String(t[f] || ""));
  const decadeOf = (y) => `${Math.floor(y / 10) * 10}s`;
  /** Each label keeps its colour wherever it shows: one of eight, from its name. */
  function labelHue(l) {
    let x = 7;
    for (const c of lkey(l)) x = (Math.imul(x, 31) + c.codePointAt(0)) >>> 0;
    return x % 8;
  }
  const sameSet = (a, b) => a.size === b.size && [...a].every((x) => b.has(x));
  function splitPath(p) {
    const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
    const file = p.slice(i + 1);
    const dot = file.lastIndexOf(".");
    return { dir: p.slice(0, i), sep: p[i] || "/", file, ext: dot > 0 ? file.slice(dot) : "" };
  }
  let uid = 0;

  /** The colour-coded boxes: a label is one coloured box; a tag has its name in colour. */
  function labelChip(l, small) {
    return h("span", { class: `tag lbl lab-${labelHue(l)}${small ? " sm" : ""}`, text: l });
  }
  function fieldChip(f, v) {
    return h("span", { class: `tag fld f-${f}` }, h("span", { class: "k", text: FIELD_LABEL[f] }), h("span", { class: "v", text: v }));
  }
  function tagChips(t) {
    const out = (t.labels || []).map((l) => labelChip(l));
    if (t.genre) out.push(fieldChip("genre", t.genre));
    if (t.year) out.push(fieldChip("year", String(t.year)));
    if (t.artist) out.push(fieldChip("artist", t.artist));
    if (t.albumartist && t.albumartist !== t.artist) out.push(fieldChip("albumartist", t.albumartist));
    if (t.album) out.push(fieldChip("album", t.album));
    if (t.track) out.push(fieldChip("track", t.disc ? `${t.track}, disc ${t.disc}` : String(t.track)));
    else if (t.disc) out.push(fieldChip("disc", String(t.disc)));
    if (t.composer) out.push(fieldChip("composer", t.composer));
    return out;
  }

  Fatstick.librarySetup = function (core) {
    const { S, api, track } = core;
    const $ = (id) => document.getElementById(id);

    // ---- what's in the library ----------------------------------------------------------
    const root = () => (S.music && S.music.root) || "";
    function inMusic(t) {
      const r = root().replace(/[\\/]+$/, "");
      if (!r || !t.path || t.path.length <= r.length + 1 || !t.path.startsWith(r)) return false;
      const c = t.path[r.length];
      return c === "/" || c === "\\";
    }
    /** Why a track's file can't be changed here, or "" if it can. */
    function whyNot(t, renaming) {
      if (!root()) return "the music folder isn't chosen yet";
      if (!inMusic(t)) return "it isn't in your music folder";
      if (!t.exists) return "its file isn't there any more";
      if (!renaming && (String(t.codec).toLowerCase() === "wma" || /\.(wma|asf)$/i.test(t.path))) return "WMA files can't take new tags";
      return "";
    }
    function whyText(ts, renaming) {
      const by = new Map();
      for (const t of ts) {
        const w = whyNot(t, renaming);
        if (w) by.set(w, (by.get(w) || 0) + 1);
      }
      return [...by].map(([w, n]) => `${plural(n, "track")}: ${w}`).join("; ");
    }
    const rel = (p) => {
      const r = root().replace(/[\\/]+$/, "");
      return r && p.startsWith(r) ? p.slice(r.length + 1) : p;
    };

    let LC = { v: -1, root: null, list: [], index: null };
    function libList() {
      if (LC.v !== S.tracksVersion || LC.root !== root()) LC = { v: S.tracksVersion, root: root(), list: [...S.tracks.values()].filter((t) => inMusic(t) && !t.forgotten), index: null };
      return LC;
    }
    function libIndex() {
      const c = libList();
      if (!c.index) c.index = SR.build(c.list);
      return c.index;
    }
    let AC = { v: -1, index: null, n: 0 };
    /** Search covers every track the app can play: the folder's and the playlists'. */
    function allIndex() {
      if (AC.v !== S.tracksVersion) {
        const ts = [...S.tracks.values()].filter((t) => t.exists && t.codec);
        AC = { v: S.tracksVersion, index: SR.build(ts), n: ts.length };
      }
      return AC.index;
    }
    function knownValues(field, max = 400) {
      const m = new Map();
      for (const t of S.tracks.values()) {
        const vs = field === "labels" ? t.labels || [] : t[field] ? [t[field]] : [];
        for (const v of vs) {
          const k = lkey(v);
          const e = m.get(k) || { v, n: 0 };
          e.n++;
          m.set(k, e);
        }
      }
      return [...m.values()].sort((a, b) => b.n - a.n).slice(0, max).map((e) => e.v);
    }
    function datalist(field) {
      const id = `dl-${field}-${++uid}`;
      return { id, el: h("datalist", { id }, knownValues(field).map((v) => h("option", { value: v }))) };
    }

    // ---- writing tags -----------------------------------------------------------------------
    // Tag edits are written as they're made. Each write can be undone: from
    // the editor's line saying what was saved, or Ctrl+Z in the library.
    const tagUndo = [];
    let lastSave = null; // { key, text, undo } for the editor of those tracks
    async function writeTags(jobs, back, undoing, quiet) {
      let r;
      try {
        r = await api.libraryWriteTags(jobs);
      } catch (e) {
        toast(`Couldn't write the tags: ${e}`);
        return null;
      }
      core.putTracks(r.tracks);
      const okIds = new Set(r.tracks.map((t) => t.id));
      const undoJobs = back ? back.filter((j) => okIds.has(j.trackId)) : [];
      const undoIt = undoJobs.length ? () => writeTags(undoJobs, null, true) : null;
      r.undo = undoIt;
      if (r.failed.length) {
        modal({
          title: r.tracks.length ? `${plural(r.failed.length, "file")} kept ${r.failed.length === 1 ? "its" : "their"} old tags` : "The tags weren't written",
          body: [
            r.tracks.length ? h("p", { text: `${undoing ? "Put the old tags back into" : "Wrote tags into"} ${plural(r.tracks.length, "file")}. These ones couldn't be changed:` }) : null,
            h("ul", { class: "fail-list" }, r.failed.map((f) => h("li", null, h("b", { text: rel(track(f.trackId).path) }), h("span", { text: f.error })))),
          ],
          buttons: [
            undoIt && !undoing ? h("button", { type: "button", class: "btn left", text: "Undo the others", onclick: () => { closeModal(); undoIt(); } }) : null,
            h("button", { type: "button", class: "btn primary", text: "Close", onclick: closeModal }),
          ],
        });
      } else if (undoing) toast(`Put the old tags back into ${plural(r.tracks.length, "file")}.`);
      else if (!quiet) toast(`Wrote tags into ${plural(r.tracks.length, "file")}.`, undoIt ? { label: "Undo", run: undoIt } : null);
      return r;
    }
    /** Ctrl+Z in the library: the last tag edit, put back. */
    function undoTags() {
      const u = tagUndo.pop();
      if (!u) return false;
      lastSave = null;
      u.run();
      return true;
    }

    // ---- files that have gone ------------------------------------------------------------------
    /**
     * Take tracks whose files have gone out of the library. One a playlist
     * still has stays for that playlist (where Locate can find it again),
     * just not listed here; the rest are dropped. If a file comes back, the
     * next scan puts it back. Tracks whose files are there are left alone.
     */
    async function forgetGone(ids) {
      const gone = ids.filter((id) => { const t = track(id); return t && !t.exists; });
      if (!gone.length) return;
      const used = new Set(S.playlists.flatMap((p) => p.entries.map((e) => e.trackId)));
      let r;
      try {
        r = await api.libraryForget(gone.filter((id) => !used.has(id)), gone.filter((id) => used.has(id)));
      } catch (e) {
        toast(`Couldn't take them out: ${e}`);
        return;
      }
      const kept = new Map(r.kept.map((t) => [t.id, t]));
      for (const id of r.removed) {
        if (kept.has(id)) S.tracks.set(id, kept.get(id));
        else S.tracks.delete(id);
      }
      S.tracksVersion++;
      L.sel = new Set([...L.sel].filter((x) => !r.removed.includes(x)));
      if (L.focus && r.removed.includes(L.focus)) L.focus = null;
      refresh();
      const n = r.removed.length;
      toast(`Took ${n === 1 ? "a track whose file has gone" : `${n} tracks whose files have gone`} out of the library.${kept.size ? ` ${kept.size === 1 ? "One is" : `${kept.size} are`} still in a playlist.` : ""}`);
    }
    const goneOf = (ids) => ids.filter((id) => !track(id).exists);

    // ---- the tag editor ------------------------------------------------------------------
    // One component for the library's side panel and the playlist's "Edit
    // tags…" dialog. Fields show the tracks' common value, or "Various" when
    // they differ (left as they are unless typed over). Labels show on how
    // many of the tracks they are, and can be added to or taken off all.
    function makeEditor(ids, opts = {}) {
      const ED = { ids: [...new Set(ids)], draft: {}, add: [], remove: new Map(), err: {}, v: S.tracksVersion, sending: [] };
      const all = () => ED.ids.map(track);
      const ok = () => all().filter((t) => !whyNot(t));
      const pool = () => { const o = ok(); return o.length ? o : all(); };
      function base(f) {
        const vs = new Set(pool().map((t) => val(t, f)));
        return vs.size === 1 ? [...vs][0] : null;
      }
      const n0 = ok().length;
      const readOnly = !n0;
      const id = ++uid;
      const errLine = h("p", { class: "ted-err", role: "alert" });
      const lblBox = h("div", { class: "ted-labels tags" });
      const lists = { artist: datalist("artist"), albumartist: datalist("albumartist"), album: datalist("album"), genre: datalist("genre"), composer: datalist("composer"), labels: datalist("labels") };
      const lblIn = h("input", {
        class: "input", id: `ted-lbl-${id}`, placeholder: "Add a label", autocomplete: "off", spellcheck: "false", list: lists.labels.id, disabled: readOnly,
        onkeydown: (e) => {
          if (e.key === "Enter" || e.key === "," || e.key === ";") {
            e.preventDefault();
            if (lblIn.value.trim()) { addLabel(lblIn.value); lblIn.value = ""; }
          }
        },
        onchange: () => { if (lblIn.value.trim() && knownValues("labels").some((x) => lkey(x) === lkey(lblIn.value))) { addLabel(lblIn.value); lblIn.value = ""; } },
        onblur: () => { if (lblIn.value.trim()) { addLabel(lblIn.value); lblIn.value = ""; } },
      });
      const statusLine = h("p", { class: "ted-status", "aria-live": "polite" });
      const key = ED.ids.join(",");
      const inputs = {};

      function head() {
        const ts = all();
        if (ts.length === 1) {
          const t = ts[0];
          return h("div", { class: "ted-head" },
            h("h3", { class: t.title ? null : "untagged", text: titleOf(t) }),
            h("p", { class: "ted-sub", text: [t.artist, t.album].filter(Boolean).join(", ") || "No artist or album" }),
            h("p", { class: "ted-path", text: rel(t.path) }));
        }
        const artists = new Set(ts.map((t) => t.artist).filter(Boolean));
        const dur = ts.reduce((a, t) => a + (t.duration || 0), 0);
        return h("div", { class: "ted-head" },
          h("h3", { text: plural(ts.length, "track") }),
          h("p", { class: "ted-sub", text: `${artists.size === 1 ? [...artists][0] : artists.size ? plural(artists.size, "artist") : "No artist"}, ${length(dur)}` }));
      }
      function fieldRow(f) {
        const b = base(f);
        const num = f in NUMERIC;
        const input = h("input", {
          class: `input${num ? " num-in" : ""}`, id: `ted-${f}-${id}`, value: b == null ? "" : b, placeholder: b === null ? "Various" : "",
          inputmode: num ? "numeric" : null, autocomplete: "off", spellcheck: "false", list: lists[f] ? lists[f].id : null, disabled: readOnly,
          oninput: () => onField(f),
          // Saved when it's entered: Enter, or leaving the field. Esc puts it back.
          onkeydown: (e) => {
            if (e.key === "Enter") { e.preventDefault(); commit(); }
            if (e.key === "Escape" && f in ED.draft) { e.preventDefault(); e.stopPropagation(); const b = base(f); input.value = b == null ? "" : b; onField(f); }
          },
          onblur: () => commit(),
        });
        inputs[f] = input;
        return [h("label", { class: "lbl", for: input.id, text: FIELD_LABEL[f] }), input];
      }
      function onField(f) {
        const input = inputs[f];
        const b = base(f);
        const v = input.value;
        if ((b === null && v.trim() === "") || (b !== null && v.trim() === b.trim())) delete ED.draft[f];
        else ED.draft[f] = v;
        delete ED.err[f];
        if (f in NUMERIC && v.trim()) {
          const m = v.trim().match(/^(\d+)(\/\d+)?$/);
          const n = m ? parseInt(m[1], 10) : NaN;
          if (!(n >= 1 && n <= NUMERIC[f])) ED.err[f] = `${FIELD_LABEL[f]} must be a number from 1 to ${NUMERIC[f]}.`;
        }
        input.classList.toggle("changed", f in ED.draft);
        input.classList.toggle("bad", !!ED.err[f]);
        update();
      }

      function labelRows() {
        const P = pool();
        const m = new Map();
        for (const t of P) {
          for (const l of t.labels || []) {
            const k = lkey(l);
            const e = m.get(k) || { label: l, n: 0 };
            e.n++;
            m.set(k, e);
          }
        }
        const adding = [...ED.add, ...ED.sending.flatMap((x) => x.add)];
        const removing = [...ED.remove.keys(), ...ED.sending.flatMap((x) => [...x.remove.keys()])];
        for (const l of adding) m.set(lkey(l), { ...(m.get(lkey(l)) || { label: l }), n: P.length, added: true });
        for (const k of removing) m.delete(k);
        return [...m.values()].sort((a, b) => b.n - a.n || a.label.localeCompare(b.label));
      }
      function renderLabels() {
        const n = pool().length;
        const rows = labelRows();
        fill(lblBox, rows.length ? rows.map((r) => {
          const partial = r.n < n;
          return h("span", { class: `tag lbl edit lab-${labelHue(r.label)}${partial ? " partial" : ""}${r.added ? " added" : ""}` },
            h("span", { class: "v", text: r.label }),
            partial ? h("span", { class: "of", text: `${r.n} of ${n}` }) : null,
            readOnly ? null : [
              partial ? h("button", { type: "button", class: "x", title: `Add “${r.label}” to all ${n}`, "aria-label": `Add ${r.label} to all ${n}`, onclick: () => addLabel(r.label) }, icon("plus")) : null,
              h("button", { type: "button", class: "x", title: `Take “${r.label}” off`, "aria-label": `Remove the label ${r.label}`, onclick: () => removeLabel(r.label) }, icon("close")),
            ]);
        }) : h("span", { class: "tags-none", text: "No labels yet" }));
      }
      function addLabel(raw) {
        const l = raw.trim().replace(/;/g, ",");
        if (!l) return;
        const k = lkey(l);
        ED.remove.delete(k);
        const spelled = [...pool().flatMap((t) => t.labels || []), ...knownValues("labels")].find((x) => lkey(x) === k) || l;
        ED.add = ED.add.filter((x) => lkey(x) !== k);
        if (pool().some((t) => !(t.labels || []).some((x) => lkey(x) === k))) ED.add.push(spelled);
        renderLabels();
        update();
        commit();
      }
      function removeLabel(l) {
        const k = lkey(l);
        ED.add = ED.add.filter((x) => lkey(x) !== k);
        if (pool().some((t) => (t.labels || []).some((x) => lkey(x) === k))) ED.remove.set(k, l);
        renderLabels();
        update();
        commit();
      }

      const dirty = () => Object.keys(ED.draft).length > 0 || ED.add.length > 0 || ED.remove.size > 0;
      const valid = () => !Object.values(ED.err).some(Boolean);
      function update() {
        errLine.textContent = Object.values(ED.err).filter(Boolean).join(" ");
      }
      function showStatus(text, undo) {
        fill(statusLine, text ? h("span", { text }) : null,
          undo ? h("button", { type: "button", class: "btn quiet small", text: "Undo", onclick: () => { const i = tagUndo.indexOf(undo); if (i >= 0) tagUndo.splice(i, 1); lastSave = null; showStatus(""); undo.run(); } }) : null);
      }
      function reset() {
        ED.draft = {};
        ED.add = [];
        ED.remove = new Map();
        ED.err = {};
        for (const f of EDIT_FIELDS) {
          const b = base(f);
          inputs[f].value = b == null ? "" : b;
          inputs[f].classList.remove("changed", "bad");
        }
        lblIn.value = "";
        renderLabels();
        update();
      }
      function nextLabels(t, x) {
        const ls = (t.labels || []).filter((l) => !x.remove.has(lkey(l)));
        for (const a of x.add) if (!ls.some((y) => lkey(y) === lkey(a))) ls.push(a);
        return ls;
      }
      /** The edit for each file, and the one that undoes it, from the files
       * as they are now (an earlier edit may just have been written). */
      function plan(x) {
        const jobs = [];
        const back = [];
        for (const t of ok()) {
          const edit = {};
          const old = {};
          for (const [f, v] of Object.entries(x.draft)) {
            const nv = f in NUMERIC ? (v.trim() ? String(parseInt(v.trim(), 10)) : "") : v.trim();
            if (nv !== val(t, f).trim()) { edit[f] = nv; old[f] = val(t, f); }
          }
          if (x.add.length || x.remove.size) {
            const nl = nextLabels(t, x);
            if (nl.join("\u0000") !== (t.labels || []).join("\u0000")) { edit.labels = nl; old.labels = [...(t.labels || [])]; }
          }
          if (Object.keys(edit).length) { jobs.push({ trackId: t.id, edit }); back.push({ trackId: t.id, edit: old }); }
        }
        return { jobs, back };
      }
      /** "Genre “Folk”", "the label “kitchen”", "Year cleared": what a write changes. */
      function summary(jobs, x) {
        const q = (x) => `“${x}”`;
        const out = [];
        for (const f of EDIT_FIELDS) {
          const js = jobs.filter((j) => f in j.edit);
          if (js.length) out.push(js[0].edit[f] === "" ? `${FIELD_LABEL[f]} cleared` : `${FIELD_LABEL[f]} ${q(js[0].edit[f])}`);
        }
        const adds = x.add.filter((l) => jobs.some((j) => j.edit.labels && j.edit.labels.some((y) => lkey(y) === lkey(l))));
        const rems = [...x.remove.values()];
        if (adds.length) out.push(`${adds.length === 1 ? "the label" : "the labels"} ${adds.map(q).join(", ")} added`);
        if (rems.length) out.push(`${rems.length === 1 ? "the label" : "the labels"} ${rems.map(q).join(", ")} taken off`);
        const t = out.join(", ");
        return t.charAt(0).toUpperCase() + t.slice(1);
      }
      /** Write what's changed into the files, now. One write at a time, in order. */
      let writing = Promise.resolve();
      const done = (x) => { ED.sending = ED.sending.filter((y) => y !== x); };
      function commit() {
        if (readOnly || !dirty() || !valid()) return writing;
        // What's being written isn't a change any more; what's typed next is.
        const x = { draft: { ...ED.draft }, add: [...ED.add], remove: new Map(ED.remove) };
        ED.draft = {};
        ED.add = [];
        ED.remove = new Map();
        ED.sending.push(x);
        showStatus("Saving…");
        writing = writing.then(async () => {
          const { jobs, back } = plan(x);
          if (!jobs.length) { done(x); settle(); showStatus(""); return; }
          const what = summary(jobs, x);
          const n = jobs.length;
          const r = await writeTags(jobs, back, false, true);
          done(x);
          if (!r) { showStatus("Not saved."); return; }
          if (r.undo) {
            const u = { label: what, run: r.undo };
            tagUndo.push(u);
            if (tagUndo.length > 50) tagUndo.shift();
            lastSave = { key, text: `Saved: ${what}${n > 1 ? `, in ${plural(n, "file")}` : ""}.`, undo: u };
            showStatus(lastSave.text, u);
          } else showStatus("");
          settle();
        });
        return writing;
      }
      /** After a write: the fields measured against the files again. */
      function settle() {
        for (const f of EDIT_FIELDS) if (document.activeElement !== inputs[f] || !(f in ED.draft)) onField(f);
        renderLabels();
        update();
      }

      const grid = h("div", { class: "ted-grid" }, EDIT_FIELDS.map(fieldRow));
      const reason = whyText(all());
      const el = h("div", { class: `ted${opts.modal ? " in-modal" : ""}` },
        head(),
        all().length > 1 && !readOnly ? h("p", { class: "hint ted-hint", text: "“Various” means the tracks differ there; they keep their own unless you type over it." }) : null,
        reason ? h("p", { class: "ted-note", text: readOnly ? `These tags can't be changed here (${reason}). Only files in your music folder can be changed.` : `Left as they are: ${reason}.` }) : null,
        grid,
        h("div", { class: "ted-lbl" },
          h("label", { class: "lbl", for: lblIn.id, text: "Labels" }),
          lblBox,
          lblIn,
          h("p", { class: "hint", text: "Your own tags, like “road trip” or “kids”. They're saved in each file's Grouping field." })),
        errLine,
        Object.values(lists).map((l) => l.el),
        statusLine);
      renderLabels();
      update();
      if (lastSave && lastSave.key === key && tagUndo.includes(lastSave.undo)) showStatus(lastSave.text, lastSave.undo);
      else showStatus(readOnly ? "" : "Changes are saved into the files as you make them.");
      return { el, ids: ED.ids, v: ED.v, dirty, valid, commit, reset, busy: () => ED.sending.length > 0, count: () => ok().length };
    }

    /** "Edit tags…" from a playlist: the editor in a dialog. */
    function editTagsDialog(ids) {
      const ts = ids.map(track);
      if (!root()) {
        modal({
          title: "Choose your music folder first",
          body: [h("p", { text: "Tags can be written into files in your music folder. Choose the folder your music is in, in the library, and the files there can be retagged and renamed." })],
          buttons: [
            h("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal }),
            h("button", { type: "button", class: "btn primary", text: "Open the library", onclick: () => { closeModal(); core.setView("library"); } }),
          ],
        });
        return;
      }
      if (!ts.some((t) => !whyNot(t))) {
        toast(`Those tags can't be changed here: ${whyText(ts)}.`);
        return;
      }
      const ed = makeEditor(ids, { modal: true });
      modal({
        title: ts.length === 1 ? "Edit tags" : `Edit the tags of ${ts.length} tracks`, body: [ed.el], wide: false,
        buttons: [h("button", { type: "button", class: "btn primary", text: "Done", onclick: closeModal })],
        // Whatever's typed and not yet entered is saved on the way out.
        onClose: () => ed.commit(),
      });
      const first = ed.el.querySelector("input:not(:disabled)");
      if (first) first.focus();
    }

    // ---- cleaning up file names -------------------------------------------------------------
    async function runRenames(items) {
      // A new name that's another of these files' current name: everything
      // goes through a temporary name first, so swaps work.
      const cur = new Map(items.map((x) => [x.t.path.toLowerCase(), x.t.id]));
      const target = (x) => { const s = splitPath(x.t.path); return `${s.dir}${s.sep}${x.name}`.toLowerCase(); };
      const failed = [];
      let done = [];
      try {
        if (items.some((x) => { const o = cur.get(target(x)); return o && o !== x.t.id; })) {
          const r1 = await api.libraryRename(items.map((x) => ({ trackId: x.t.id, name: `fatstick-renaming-${x.t.id}${splitPath(x.t.path).ext}` })));
          core.putTracks(r1.tracks);
          failed.push(...r1.failed);
          const okIds = new Set(r1.tracks.map((t) => t.id));
          items = items.filter((x) => okIds.has(x.t.id));
        }
        const r2 = await api.libraryRename(items.map((x) => ({ trackId: x.t.id, name: x.name })));
        core.putTracks(r2.tracks);
        failed.push(...r2.failed);
        done = r2.tracks;
      } catch (e) {
        toast(`Couldn't rename the files: ${e}`);
        return { done, failed };
      }
      return { done, failed };
    }
    function renameDialog(ids) {
      let cfg = { template: NAME_PRESETS[0].tpl, ascii: false, maxLen: 0 };
      try { cfg = { ...cfg, ...JSON.parse(S.settings.libraryNames || "{}") }; } catch { /* the defaults */ }
      const ts = ids.map(track);
      const ok = ts.filter((t) => !whyNot(t, true));
      const skipped = whyText(ts, true);
      const tplIn = h("input", { class: "input mono", value: cfg.template, spellcheck: "false", autocomplete: "off", "aria-label": "File name template", oninput: () => { cfg.template = tplIn.value; refresh(); } });
      const presets = h("select", {
        class: "input", "aria-label": "Templates",
        onchange: () => { if (presets.value) { tplIn.value = cfg.template = presets.value; presets.value = ""; refresh(); } },
      }, h("option", { value: "", text: "Templates" }), NAME_PRESETS.map((p) => h("option", { value: p.tpl, text: p.label })));
      const ascii = h("input", { type: "checkbox", checked: !!cfg.ascii, onchange: () => { cfg.ascii = ascii.checked; refresh(); } });
      const maxIn = h("input", { class: "input secs", type: "number", min: "0", max: "200", value: String(cfg.maxLen || ""), placeholder: "none", "aria-label": "Longest name", oninput: () => { cfg.maxLen = parseInt(maxIn.value, 10) || 0; refresh(); } });
      const errs = h("p", { class: "ted-err", role: "alert" });
      const sum = h("p", { class: "ren-sum" });
      const table = h("div", { class: "ren-list" });
      const go = h("button", { type: "button", class: "btn primary", text: "Rename", disabled: true });
      let rows = [];
      function refresh() {
        const entries = ok.map((t) => { const s = splitPath(t.path); return { t, s, tags: { ...t }, filename: t.filename, ext: s.ext, dir: s.dir }; });
        const r = N.libraryNames(entries, { template: cfg.template, ascii: cfg.ascii, maxLen: cfg.maxLen });
        errs.textContent = r.errors.join(" ");
        tplIn.classList.toggle("bad", r.errors.length > 0);
        const mine = new Set(ok.map((t) => t.id));
        const taken = new Set();
        for (const t of S.tracks.values()) if (!mine.has(t.id) && t.exists) taken.add(t.path.toLowerCase());
        rows = entries.map((e, i) => {
          const name = r.names[i].name;
          const same = name === e.s.file;
          const clash = !same && taken.has(`${e.s.dir}${e.s.sep}${name}`.toLowerCase());
          return { t: e.t, old: e.s.file, name, notes: r.names[i].notes, same, clash };
        });
        const change = rows.filter((x) => !x.same && !x.clash);
        const clashes = rows.filter((x) => x.clash).length;
        sum.textContent = `${change.length ? `${plural(change.length, "file")} get${change.length === 1 ? "s" : ""} a new name` : "No file needs a new name"}${rows.length - change.length - clashes ? `, ${rows.length - change.length - clashes} already match` : ""}${clashes ? `, ${clashes} would take the name of a file that's already there and ${clashes === 1 ? "is" : "are"} left out` : ""}.`;
        go.disabled = !change.length || r.errors.length > 0;
        go.textContent = change.length ? `Rename ${plural(change.length, "file")}` : "Rename";
        const show = [...rows.filter((x) => !x.same), ...rows.filter((x) => x.same)].slice(0, 400);
        fill(table, show.map((x) => h("div", { class: `ren-row${x.same ? " same" : ""}${x.clash ? " clash" : ""}` },
          h("span", { class: "old", text: x.old }),
          h("span", { class: "arrow", "aria-hidden": "true", text: x.same ? "" : "→" }),
          h("span", { class: "new", title: x.notes.join("; ") || null }, x.same ? "unchanged" : x.name, x.clash ? h("small", { text: " (a file with this name is already there)" }) : x.notes.length ? h("small", { text: ` (${x.notes.join("; ")})` }) : null))),
        rows.length > 400 ? h("p", { class: "st-note", text: `And ${rows.length - 400} more.` }) : null);
      }
      go.onclick = async () => {
        const change = rows.filter((x) => !x.same && !x.clash);
        go.disabled = true;
        core.saveSetting("libraryNames", cfg);
        S.settings.libraryNames = JSON.stringify(cfg);
        closeModal();
        const back = change.map((x) => ({ id: x.t.id, name: x.old }));
        const r = await runRenames(change.map((x) => ({ t: x.t, name: x.name })));
        const okIds = new Set(r.done.map((t) => t.id));
        const undoIt = () => runRenames(back.filter((b) => okIds.has(b.id)).map((b) => ({ t: track(b.id), name: b.name }))).then((u) => toast(u.failed.length ? `Put back ${plural(u.done.length, "name")}; ${u.failed.length} couldn't be: ${u.failed[0].error}` : `Put back the old names of ${plural(u.done.length, "file")}.`));
        if (r.failed.length) toast(`Renamed ${plural(r.done.length, "file")}. ${plural(r.failed.length, "file")} kept ${r.failed.length === 1 ? "its" : "their"} name: ${r.failed[0].error}`, r.done.length ? { label: "Undo", run: undoIt } : null);
        else if (r.done.length) toast(`Renamed ${plural(r.done.length, "file")}.`, { label: "Undo", run: undoIt });
      };
      modal({
        title: ok.length === 1 ? "Clean up the file name" : `Clean up ${plural(ok.length, "file name")}`,
        wide: true, tall: true,
        body: [
          h("p", { text: "New names from the tags, with the same clean-up as names on a stick. Each file stays in its folder and keeps its type; playlists follow the files." }),
          h("div", { class: "form" },
            h("span", { class: "lbl", text: "Name" }),
            h("div", { class: "opts" }, h("div", { class: "tpl-row" }, tplIn, presets), errs,
              h("span", { class: "hint", text: "Tags: {title} {artist} {album} {albumartist} {track} {disc} {year} {genre} {filename}. {a|b} uses b when a is empty; [ ] leaves out a part whose tag is empty." })),
            h("span", { class: "lbl", text: "Clean-up" }),
            h("div", { class: "opts row-opts" },
              h("label", { class: "check" }, ascii, h("span", { text: "Plain letters only (é becomes e)" })),
              h("label", { class: "check" }, h("span", { text: "Longest name" }), maxIn))),
          skipped ? h("p", { class: "st-note", text: `Left as they are: ${skipped}.` }) : null,
          sum,
          table,
        ],
        buttons: [h("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal }), go],
      });
      refresh();
    }

    // ---- dragging tracks and adding them ----------------------------------------------------
    function addToCurrent(ids) {
      if (!ids.length) return;
      if (!core.pl()) { core.newPlaylist("From the library", ids); return; }
      core.insertTracks(ids, { kind: "playlist", id: S.current });
    }
    /** Pointer down on a row of a track list (search results, library table). */
    function rowsDown(e, X) {
      if (e.button !== 0) return;
      const r = e.target.closest("[data-tid]");
      if (!r || e.target.closest("button, input, a")) return;
      if (e.pointerType !== "touch") e.preventDefault();
      X.box.focus({ preventScroll: true });
      const id = r.dataset.tid;
      const mod = e.ctrlKey || e.metaKey;
      let next = new Set(X.sel);
      let after = null;
      if (e.shiftKey) {
        next = X.range(X.anchor || id, id, mod ? next : new Set());
      } else if (mod) {
        if (next.has(id)) after = () => { const n = new Set(X.sel); n.delete(id); X.set(n, id); };
        else { next.add(id); X.anchor = id; }
      } else if (!next.has(id)) {
        next = new Set([id]);
        X.anchor = id;
      } else if (next.size > 1) after = () => X.set(new Set([id]), id);
      if (!X.set(next, id)) return;
      if (e.pointerType === "touch") return;
      core.startTrackDrag(e, X.box, () => X.ordered(), {
        onTap: () => {
          if (after) after();
          const now = performance.now();
          if (X.lastTap && X.lastTap.id === id && now - X.lastTap.t < 450) {
            X.lastTap = null;
            core.playTrack(id);
          } else X.lastTap = { id, t: now };
        },
      });
    }
    function rangeOf(ids, a, b, into) {
      const i = ids.indexOf(a);
      const j = ids.indexOf(b);
      const out = new Set(into);
      if (i < 0 || j < 0) { out.add(b); return out; }
      for (let k = Math.min(i, j); k <= Math.max(i, j); k++) out.add(ids[k]);
      return out;
    }
    /** Arrow keys, Home, End, Ctrl+A, Enter (play), Shift+Enter (add to the
     * open playlist) and Escape in a track list. */
    function listKey(e, X, pageRows) {
      const ids = X.ids();
      if (!ids.length) return false;
      const i = ids.indexOf(X.focus);
      const go = (j) => {
        j = Math.max(0, Math.min(ids.length - 1, j));
        const id = ids[j];
        const next = e.shiftKey ? rangeOf(ids, X.anchor || id, id, new Set()) : new Set([id]);
        if (!e.shiftKey) X.anchor = id;
        if (X.set(next, id)) X.reveal(id);
      };
      const mod = e.ctrlKey || e.metaKey;
      if (e.key === "ArrowDown") go(i < 0 ? 0 : i + 1);
      else if (e.key === "ArrowUp") { if (i <= 0 && X.up) X.up(); else go(i - 1); }
      else if (e.key === "PageDown") go((i < 0 ? 0 : i) + pageRows);
      else if (e.key === "PageUp") go((i < 0 ? 0 : i) - pageRows);
      else if (e.key === "Home") go(0);
      else if (e.key === "End") go(ids.length - 1);
      else if (mod && e.key.toLowerCase() === "a") X.set(new Set(ids), X.focus);
      else if (e.key === "Enter" && e.shiftKey) addToCurrent(X.ordered());
      else if (e.key === "Enter") { const id = X.focus && X.sel.has(X.focus) ? X.focus : X.ordered()[0]; if (id) core.playTrack(id); }
      else if (e.key === "Escape") { if (X.sel.size) X.set(new Set(), null); else if (X.escape) X.escape(); else return false; }
      else if ((e.key === "Delete" || e.key === "Backspace") && X.remove && goneOf(X.ordered()).length) X.remove(X.ordered());
      else return false;
      e.preventDefault();
      return true;
    }
    function trackMenu(e, X) {
      const r = e.target.closest("[data-tid]");
      if (!r) return;
      e.preventDefault();
      const id = r.dataset.tid;
      if (!X.sel.has(id) && !X.set(new Set([id]), id)) return;
      const ids = X.ordered();
      const n = ids.length;
      const p = core.pl();
      const others = S.playlists.filter((x) => !p || x.id !== p.id);
      const canTag = ids.some((x) => !whyNot(track(x)));
      const t = track(id);
      menu({ x: e.clientX, y: e.clientY }, [
        { label: "Play", key: "Enter", action: () => core.playTrack(id), disabled: !t.exists },
        "-",
        p ? { label: `Add to ${p.name}`, key: "Shift+Enter", action: () => addToCurrent(ids) } : null,
        ...(others.length ? [{ heading: n === 1 ? "Add to playlist" : `Add ${n} tracks to` }, ...others.map((x) => ({ label: x.name, action: () => core.insertTracks(ids, { kind: "playlist", id: x.id }) }))] : []),
        { label: "New playlist from these", action: () => { core.newPlaylist(n === 1 ? titleOf(track(ids[0])) : "From the library", ids); if (S.view === "library") core.setView("playlist"); } },
        "-",
        X.inLibrary ? null : { label: "Show in the library", action: () => revealInLibrary(id), disabled: !inMusic(track(id)) },
        X.inLibrary ? null : { label: n === 1 ? "Edit tags…" : `Edit the tags of ${n} tracks…`, action: () => editTagsDialog(ids), disabled: !canTag },
        X.inLibrary ? { label: n === 1 ? "Clean up the file name…" : `Clean up ${n} file names…`, action: () => renameDialog(ids), disabled: !ids.some((x) => !whyNot(track(x), true)) } : null,
        ...(X.inLibrary && goneOf(ids).length ? ["-", {
          label: goneOf(ids).length === 1 ? "Remove from the library" : `Remove ${goneOf(ids).length} from the library`, key: "Delete",
          action: () => forgetGone(ids),
        }] : []),
      ].filter(Boolean));
    }

    // ---- playing a song on its own ------------------------------------------------------------
    /** The equalizer bars beside the song that's playing on its own. */
    function eqFor(t) {
      if (S.player.trackId !== t.id) return null;
      return h("span", { class: `eq${S.player.playing ? "" : " paused"}`, "aria-label": S.player.playing ? "Playing" : "Paused" }, h("i"), h("i"), h("i"));
    }
    /** The player started, paused or stopped: mark the song in the lists. */
    function markPlaying() {
      if (S.view === "library" && V.inner) renderRows();
      if (S.searching && SE.root) refreshSearch();
    }
    /** Space with nothing playing, in the library: the focused or selected song, or the first. */
    function trackToPlay() {
      if (!V.root || !L.ids.length) return null;
      if (L.focus && L.sel.has(L.focus)) return L.focus;
      return LX.ordered()[0] || L.ids[0];
    }

    // ---- the search panel ------------------------------------------------------------------
    const SE = { q: "", sel: new Set(), anchor: null, focus: null, ids: [], res: null, key: "", lastTap: null };
    const SX = {
      get sel() { return SE.sel; },
      get anchor() { return SE.anchor; },
      set anchor(v) { SE.anchor = v; },
      get focus() { return SE.focus; },
      get lastTap() { return SE.lastTap; },
      set lastTap(v) { SE.lastTap = v; },
      get box() { return SE.list; },
      ids: () => SE.ids,
      ordered: () => SE.ids.filter((x) => SE.sel.has(x)),
      range: (a, b, into) => rangeOf(SE.ids, a, b, into),
      set(next, focus) { SE.sel = next; SE.focus = focus; syncSearchRows(); return true; },
      reveal(id) { const el = document.getElementById(`sr-${id}`); if (el) el.scrollIntoView({ block: "nearest" }); },
      up() { SE.input.focus(); },
      escape() { SE.input.focus(); },
    };
    function buildSearch() {
      SE.input = h("input", {
        type: "search", class: "input srch-in", id: "srch-in", placeholder: "Song, artist, album, genre, label…", "aria-label": "Search your music",
        autocomplete: "off", spellcheck: "false",
        oninput: () => { clearTimeout(SE.timer); SE.timer = setTimeout(() => { SE.q = SE.input.value; refreshSearch(); }, 90); },
        onkeydown: (e) => {
          if (e.key === "ArrowDown" || e.key === "Enter") {
            e.preventDefault();
            clearTimeout(SE.timer);
            if (SE.q !== SE.input.value) { SE.q = SE.input.value; refreshSearch(); }
            if (SE.ids.length) { SE.anchor = SE.ids[0]; SX.set(new Set([SE.ids[0]]), SE.ids[0]); SE.list.focus(); SX.reveal(SE.ids[0]); }
          } else if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            if (SE.input.value) { SE.input.value = SE.q = ""; refreshSearch(); } else closeSearch();
          }
        },
      });
      SE.scope = h("p", { class: "srch-scope", "aria-live": "polite" });
      SE.list = h("div", {
        class: "srch-res", role: "listbox", tabindex: "0", "aria-label": "Search results", "aria-multiselectable": "true",
        onpointerdown: (e) => rowsDown(e, SX),
        onkeydown: (e) => { if (listKey(e, SX, 8)) e.stopPropagation(); },
        oncontextmenu: (e) => trackMenu(e, SX),
      });
      SE.root = h("div", { class: "srch" },
        h("div", { class: "srch-box" },
          h("span", { class: "srch-ic" }, icon("search")),
          SE.input,
          h("button", { type: "button", class: "btn quiet small icon", title: "Close the search (Esc)", "aria-label": "Close the search", onclick: closeSearch }, icon("close"))),
        SE.scope, SE.list,
        h("p", { class: "srch-foot", text: "Double-click a song to hear it. Drag songs into the playlist to add them." }));
    }
    function searchRoot() {
      if (!SE.root) buildSearch();
      return SE.root;
    }
    /** The sidebar in search mode: the panel (kept, so typing keeps its focus) and the foot. */
    function renderSearch(side, foot) {
      const r = searchRoot();
      if (r.parentNode !== side) fill(side, r, foot);
      else if (r.nextSibling) r.nextSibling.replaceWith(foot);
      else side.append(foot);
      refreshSearch();
    }
    function openSearch() {
      if (S.view === "library") core.setView("playlist");
      S.searching = true;
      core.renderSide();
      document.getElementById("app").classList.add("searching");
      SE.input.focus();
      SE.input.select();
    }
    function closeSearch() {
      S.searching = false;
      document.getElementById("app").classList.remove("searching");
      core.renderSide();
      const b = document.getElementById("srch-btn");
      if (b) b.focus({ preventScroll: true });
    }
    function refreshSearch() {
      if (!SE.root) return;
      const q = SE.q.trim();
      const key = `${S.tracksVersion}\u0000${q}`;
      if (SE.key !== key) {
        SE.key = key;
        SE.res = q ? SR.search(allIndex(), q, { limit: 150 }) : null;
      } else allIndex();
      const R = SE.res;
      const p = core.pl();
      const inPl = new Set(p ? p.entries.map((e) => e.trackId) : []);
      const toks = SR.norm(q).split(" ").filter(Boolean);
      const hit = (s) => SR.words(s).some((w) => toks.some((tk) => SR.closeness(tk, w) > 0));
      // The width a result's labels can take (the panel, less its padding and the time).
      const resW = Math.max(120, (document.querySelector(".srch-res") || { clientWidth: 320 }).clientWidth - 80);
      const row = (x, close) => {
        const t = x.t;
        const hints = [];
        // The track's labels (those that matched first), as many as fit, then "+2".
        const ls = [...(t.labels || [])].sort((a, b) => Number(hit(b)) - Number(hit(a)));
        if (ls.length) hints.push(...labelsThatFit(ls, t.id, resW));
        for (const f of ["genre", "year", "composer", "albumartist"]) if (x.fields.includes(f) && t[f] && hit(String(t[f]))) hints.push(fieldChip(f, String(t[f])));
        const sub = [t.artist, t.album].filter(Boolean).join(", ");
        const on = SE.sel.has(t.id);
        return h("div", {
          class: `sr${on ? " sel" : ""}${SE.focus === t.id ? " focus" : ""}${close ? " close" : ""}${S.player.trackId === t.id ? " playing" : ""}`, role: "option", id: `sr-${t.id}`,
          "aria-selected": String(on), dataset: { tid: t.id },
        },
        h("div", { class: "sr-txt" },
          h("span", { class: t.title ? "sr-t" : "sr-t untagged" }, eqFor(t), titleOf(t)),
          sub ? h("span", { class: "sr-s", text: sub }) : null,
          hints.length ? h("span", { class: "sr-hints tags" }, hints) : null),
        h("span", { class: "sr-r" },
          inPl.has(t.id) ? h("span", { class: "sr-in", title: `Already in ${p.name}`, "aria-label": `Already in ${p.name}` }, icon("check")) : null,
          h("span", { class: "sr-time num", text: t.duration ? clock(t.duration) : "" })));
      };
      SE.ids = R ? [...R.exact, ...R.close].map((x) => x.t.id) : [];
      SE.sel = new Set([...SE.sel].filter((x) => SE.ids.includes(x)));
      if (!q) {
        fill(SE.scope, `Searches ${plural(AC.n, "song")}${root() ? " in your music folder and your playlists" : " in your playlists"}: titles, artists, albums, genres, labels, years and file names.`,
          root() ? null : h("button", { type: "button", class: "btn quiet small", text: "Choose your music folder…", onclick: () => core.setView("library") }));
        fill(SE.list);
        return;
      }
      fill(SE.scope, R.exact.length || R.close.length
        ? `${R.exact.length ? plural(R.exact.length, "exact match", "exact matches") : "No exact matches"}${R.close.length ? `, ${R.close.length} close` : ""}.`
        : `Nothing matches “${q}”. Try fewer words, or part of a name.`);
      fill(SE.list,
        R.exact.map((x) => row(x, false)),
        R.close.length ? h("div", { class: "sr-div", role: "separator" }, h("span", { text: "Close matches" })) : null,
        R.close.map((x) => row(x, true)));
    }
    function syncSearchRows() {
      if (!SE.list) return;
      for (const r of SE.list.querySelectorAll(".sr")) {
        const on = SE.sel.has(r.dataset.tid);
        r.classList.toggle("sel", on);
        r.classList.toggle("focus", r.dataset.tid === SE.focus);
        r.setAttribute("aria-selected", String(on));
      }
      if (SE.focus) SE.list.setAttribute("aria-activedescendant", `sr-${SE.focus}`);
      else SE.list.removeAttribute("aria-activedescendant");
    }

    // ---- the library view -----------------------------------------------------------------
    const L = { q: "", filters: [], sort: { key: "artist", dir: 1 }, best: true, sel: new Set(), anchor: null, focus: null, rows: [], ids: [], at: new Map(), more: {}, width: 800, editor: null, lastTap: null, scrollTo: null };
    const V = {};
    const LX = {
      inLibrary: true,
      get sel() { return L.sel; },
      get anchor() { return L.anchor; },
      set anchor(v) { L.anchor = v; },
      get focus() { return L.focus; },
      get lastTap() { return L.lastTap; },
      set lastTap(v) { L.lastTap = v; },
      get box() { return V.scroll; },
      ids: () => L.ids,
      ordered: () => L.ids.filter((x) => L.sel.has(x)),
      range: (a, b, into) => rangeOf(L.ids, a, b, into),
      set: (next, focus) => setSel(next, focus),
      reveal: (id) => revealRow(id),
      escape: null,
      remove: (ids) => forgetGone(ids),
    };

    function buildView() {
      V.root = $("libview");
      V.head = h("header", { class: "lib-head" });
      V.facets = h("nav", { class: "lib-facets", "aria-label": "Browse by tag" });
      V.q = h("input", {
        type: "search", class: "input lib-q", id: "lib-q", placeholder: "Filter by title, artist, album, genre, label…", "aria-label": "Filter the library",
        autocomplete: "off", spellcheck: "false",
        oninput: () => { clearTimeout(V.qt); V.qt = setTimeout(() => setQuery(V.q.value), 110); },
        onkeydown: (e) => {
          if (e.key === "ArrowDown") { e.preventDefault(); if (L.ids.length) { L.anchor = L.ids[0]; if (setSel(new Set([L.ids[0]]), L.ids[0])) { V.scroll.focus(); revealRow(L.ids[0]); } } }
          if (e.key === "Escape" && V.q.value) { e.preventDefault(); e.stopPropagation(); V.q.value = ""; setQuery(""); }
        },
      });
      V.chips = h("div", { class: "lib-chips" });
      V.cols = h("div", { class: "lib-cols" });
      V.inner = h("div", { class: "lib-rows" });
      V.scroll = h("div", {
        class: "lib-scroll", tabindex: "0", role: "listbox", "aria-multiselectable": "true", "aria-label": "Tracks in your music folder",
        onpointerdown: (e) => rowsDown(e, LX),
        onkeydown: (e) => { if (listKey(e, LX, Math.max(1, Math.floor(V.scroll.clientHeight / RH) - 1))) e.stopPropagation(); },
        oncontextmenu: (e) => trackMenu(e, LX),
        onscroll: () => { if (!V.raf) V.raf = requestAnimationFrame(() => { V.raf = 0; renderRows(); }); },
      }, V.inner);
      V.empty = h("p", { class: "lib-none", hidden: true });
      V.foot = h("div", { class: "lib-foot" });
      V.edit = h("aside", { class: "lib-edit", "aria-label": "Tags of the selected tracks" });
      V.main = h("div", { class: "lib-main" },
        h("div", { class: "lib-bar" }, h("div", { class: "lib-qbox" }, h("span", { class: "srch-ic" }, icon("search")), V.q), V.chips),
        h("div", { class: "lib-table" }, V.cols, V.scroll, V.empty),
        V.foot);
      V.body = h("div", { class: "lib-body" }, V.facets, V.main, V.edit);
      V.start = h("div", { class: "lib-start" });
      V.root.replaceChildren(V.head, V.body, V.start);
      new ResizeObserver(() => {
        const w = V.scroll.clientWidth;
        if (!w) return;
        const before = visibleCols().join();
        L.width = w;
        if (visibleCols().join() !== before) renderTable();
        else renderRows();
      }).observe(V.scroll);
    }

    function renderView() {
      if (!V.root) buildView();
      const has = !!root();
      V.body.hidden = !has;
      V.start.hidden = has;
      if (has) computeRows();
      renderLibHead();
      if (!has) { renderStart(); return; }
      renderFacets();
      renderChips();
      renderTable();
      renderLibFoot();
      renderEditor();
    }
    /** After tracks changed (a scan, a tag edit, a rename); with `ifChanged`,
     * only if they changed since the library was last drawn. */
    let drawnAt = -1;
    function refresh(ifChanged) {
      if (ifChanged && drawnAt === S.tracksVersion) return;
      if (S.searching) refreshSearch();
      if (S.view === "library" && V.root) {
        renderView();
        drawnAt = S.tracksVersion;
      }
    }

    function statusText() {
      const m = S.music || {};
      const p = S.libProgress;
      if (m.error) return `Can't open the folder: ${m.error}. If it's on a drive that isn't plugged in, plug it in and scan again, or choose another folder.`;
      if (m.scanning) return p && p.phase === "read" && p.total ? `Reading ${p.done} of ${plural(p.total, "new or changed file")}…` : "Looking for new, changed and moved files…";
      const list = libList().list;
      const missing = list.filter((t) => !t.exists).length;
      const failed = m.last && m.last.failed ? `, ${m.last.failed} couldn't be read` : "";
      return `${plural(list.length, "track")}${missing ? `, ${missing} not found` : ""}${failed}. ${m.watching ? "Watching for changes" : "Not watching for changes; scan after you change files"}${m.lastScan ? `, last checked ${ago(m.lastScan)}` : ""}.`;
    }
    function renderLibHead() {
      const m = S.music || {};
      const p = S.libProgress;
      const pct = m.scanning && p && p.total ? Math.round((100 * p.done) / p.total) : null;
      fill(V.head,
        h("div", { class: "lib-title" },
          h("h1", { text: "Music library" }),
          root() ? h("p", { class: "lib-path", title: root(), text: root() }) : null,
          root() ? h("p", { class: `lib-status${m.error ? " bad" : ""}`, "aria-live": "polite", text: statusText() }) : null),
        root() ? h("div", { class: "tools" },
          h("button", { type: "button", class: "btn small", text: "Clean up file names…", onclick: () => renameDialog(L.sel.size ? LX.ordered() : L.ids), disabled: !L.ids.length }),
          h("button", { type: "button", class: "btn small", text: m.scanning ? "Scanning…" : "Scan now", disabled: !!m.scanning, onclick: scanNow }),
          h("button", { type: "button", class: "btn small", text: "Change folder…", onclick: chooseFolder })) : null,
        m.scanning ? h("div", { class: `lib-prog${pct == null ? " busy" : ""}`, role: "progressbar", "aria-label": "Scanning", "aria-valuenow": pct == null ? null : String(pct) }, h("i", { style: { width: `${pct == null ? 30 : pct}%` } })) : null);
    }
    function renderStart() {
      fill(V.start,
        h("div", { class: "lib-start-in" },
          h("h2", { text: "Choose the folder your music is in" }),
          h("p", { text: "Fatstick reads every song in it, and keeps up as you add, move and retag files. Search it, browse it by tag, and drag songs onto your playlists." }),
          h("p", { text: "You sort the files into folders yourself. Tags and file names only change when you ask, after a preview, and nothing outside the folder is touched." }),
          h("button", { type: "button", class: "btn primary", text: "Choose your music folder…", onclick: chooseFolder })));
    }
    // The folder's state comes from the app's events. A small folder's scan
    // can finish (and its event arrive) before the call that started it
    // returns, so an answer older than the last event is ignored.
    let scanEvents = 0;
    async function chooseFolder() {
      const path = await api.pickFolder("Your music folder");
      if (path) await useFolder(path);
    }
    async function useFolder(path) {
      const seen = scanEvents;
      let info;
      try {
        info = await api.librarySetRoot(path);
      } catch (e) {
        toast(`Couldn't use that folder: ${e}`);
        return false;
      }
      if (scanEvents === seen) S.music = info;
      L.sel = new Set();
      L.filters = [];
      L.editor = null;
      renderView();
      core.renderDisplay();
      return true;
    }
    async function scanNow() {
      S.music = { ...S.music, scanning: true };
      S.libProgress = null;
      renderLibHead();
      const seen = scanEvents;
      try {
        await api.libraryScan();
        // No scan started (no folder, or no ffmpeg): say how things are.
        const info = await api.libraryInfo();
        if (scanEvents === seen && !info.scanning) { S.music = info; renderLibHead(); }
      } catch (e) {
        toast(`Couldn't scan: ${e}`);
      }
    }

    // Filters and the rows they leave.
    function keysOf(t, field) {
      switch (field) {
        case "labels": return (t.labels || []).map((l) => [lkey(l), l]);
        case "decade": return t.year ? [[decadeOf(t.year), decadeOf(t.year)]] : [];
        case "missing": return MISSING.filter((m) => m.test(t)).map((m) => [m.key, m.label]);
        default: {
          const v = String(t[field] || "").trim();
          return v ? [[v.toLowerCase(), v]] : [];
        }
      }
    }
    function applyFilters(list, skip) {
      const by = new Map();
      for (const f of L.filters) {
        if (f.field === skip) continue;
        if (!by.has(f.field)) by.set(f.field, []);
        by.get(f.field).push(f.value);
      }
      if (!by.size) return list;
      return list.filter((t) => {
        for (const [field, vals] of by) {
          const ks = new Set(keysOf(t, field).map(([k]) => k));
          if (EVERY.has(field) ? !vals.every((v) => ks.has(v)) : !vals.some((v) => ks.has(v))) return false;
        }
        return true;
      });
    }
    const SK = {
      title: (t) => [titleOf(t)],
      artist: (t) => [t.albumartist || t.artist, t.album, t.disc || 0, t.track || 0, t.path],
      album: (t) => [t.album, t.albumartist || t.artist, t.disc || 0, t.track || 0],
      track: (t) => [t.disc || 0, t.track || 0, titleOf(t)],
      year: (t) => [t.year || 0, t.albumartist || t.artist, t.album, t.disc || 0, t.track || 0],
      genre: (t) => [t.genre, t.albumartist || t.artist, t.album, t.disc || 0, t.track || 0],
      labels: (t) => [(t.labels || []).join(", "), titleOf(t)],
      time: (t) => [t.duration || 0],
    };
    function sorted(list) {
      const kf = SK[L.sort.key] || SK.artist;
      const dir = L.sort.dir;
      const C = core.collator;
      return list.map((t) => ({ t, k: kf(t) })).sort((a, b) => {
        for (let i = 0; i < a.k.length; i++) {
          const x = a.k[i];
          const y = b.k[i];
          if (typeof x === "number" || typeof y === "number") {
            const c = (x || 0) - (y || 0);
            if (c) return c * dir;
            continue;
          }
          if (!x !== !y) return x ? -1 : 1; // empty values last, either way
          const c = C.compare(x || "", y || "");
          if (c) return c * dir;
        }
        return 0;
      }).map((x) => x.t);
    }
    function computeRows() {
      const all = libList().list;
      const q = L.q.trim();
      let qList = all;
      let ex = null;
      let cl = null;
      if (q) {
        const r = SR.search(libIndex(), q, { limit: 1e9 });
        ex = r.exact.map((x) => x.t);
        cl = r.close.map((x) => x.t);
        qList = [...ex, ...cl];
      }
      L.qList = qList;
      const keep = new Set(applyFilters(qList).map((t) => t.id));
      let rows;
      if (q) {
        let e = ex.filter((t) => keep.has(t.id));
        let c = cl.filter((t) => keep.has(t.id));
        if (!L.best) { e = sorted(e); c = sorted(c); }
        rows = [...e.map((t) => ({ t })), ...(c.length ? [{ divider: true, n: c.length }] : []), ...c.map((t) => ({ t, close: true }))];
      } else rows = sorted(all.filter((t) => keep.has(t.id))).map((t) => ({ t }));
      L.rows = rows;
      L.ids = [];
      L.at = new Map();
      rows.forEach((r, i) => { if (r.t) { L.ids.push(r.t.id); L.at.set(r.t.id, i); } });
      // The selection only keeps what's shown, unless an edit for it is on its way.
      if (!(L.editor && (L.editor.dirty() || L.editor.busy()))) {
        const shown = new Set(L.ids);
        const next = new Set([...L.sel].filter((x) => shown.has(x)));
        if (next.size !== L.sel.size) L.sel = next;
        if (L.focus && !shown.has(L.focus)) L.focus = null;
      }
    }
    function setQuery(q) {
      if (q.trim() && !L.q.trim()) L.best = true;
      L.q = q;
      computeRows();
      renderFacets();
      renderTable();
      renderLibFoot();
      renderEditor();
      V.scroll.scrollTop = 0;
    }
    function toggleFilter(field, value, label) {
      const i = L.filters.findIndex((f) => f.field === field && f.value === value);
      if (i >= 0) L.filters.splice(i, 1);
      else L.filters.push({ field, value, label });
      computeRows();
      renderFacets();
      renderChips();
      renderTable();
      renderLibFoot();
      renderEditor();
      V.scroll.scrollTop = 0;
    }

    function renderFacets() {
      const groups = GROUPS.map((g) => {
        const list = EVERY.has(g.field) ? applyFilters(L.qList) : applyFilters(L.qList, g.field);
        const m = new Map();
        for (const t of list) {
          for (const [k, label] of keysOf(t, g.field)) {
            const e = m.get(k) || { k, label, n: 0 };
            e.n++;
            m.set(k, e);
          }
        }
        for (const f of L.filters) if (f.field === g.field && !m.has(f.value)) m.set(f.value, { k: f.value, label: f.label, n: 0 });
        let items = [...m.values()];
        if (g.fixed) items.sort((a, b) => MISSING.findIndex((x) => x.key === a.k) - MISSING.findIndex((x) => x.key === b.k));
        else if (g.byValue) items.sort((a, b) => a.k.localeCompare(b.k));
        else items.sort((a, b) => b.n - a.n || core.collator.compare(a.label, b.label));
        const on = (it) => L.filters.some((f) => f.field === g.field && f.value === it.k);
        const open = L.more[g.field];
        const max = open ? 400 : g.show;
        // Chosen tags stay in view, even past the first few.
        const shown = [...items.slice(0, max), ...items.slice(max).filter(on)];
        if (!items.length) {
          return g.field === "labels"
            ? h("section", { class: "fct-g" }, h("h3", { text: g.title }), h("p", { class: "fct-none", text: "None yet. Select tracks and add labels in the tag editor on the right." }))
            : null;
        }
        return h("section", { class: "fct-g" },
          h("h3", { text: g.title }),
          h("ul", null, shown.map((it) => h("li", null, h("button", {
            type: "button", class: `fct${g.field === "labels" ? " dotted" : ""}${on(it) ? " on" : ""}`, "aria-pressed": String(on(it)), title: it.label,
            onclick: () => toggleFilter(g.field, it.k, it.label),
          },
          g.field === "labels" ? h("span", { class: `dot lab-${labelHue(it.label)}`, "aria-hidden": "true" }) : null,
          h("span", { class: "nm", text: it.label }),
          h("span", { class: "ct num", text: String(it.n) }))))),
          items.length > g.show ? h("button", { type: "button", class: "btn quiet small fct-more", text: open ? "Show fewer" : `Show all ${items.length}`, onclick: () => { L.more[g.field] = !open; renderFacets(); } }) : null,
          open && items.length > 400 ? h("p", { class: "fct-none", text: `And ${items.length - 400} more: type in the filter to find them.` }) : null);
      });
      const top = V.facets.scrollTop;
      fill(V.facets, groups);
      V.facets.scrollTop = top;
    }
    function renderChips() {
      fill(V.chips, L.filters.map((f) => {
        const x = h("button", { type: "button", class: "x", title: "Remove this filter", "aria-label": `Remove the filter ${f.label}`, onclick: () => toggleFilter(f.field, f.value, f.label) }, icon("close"));
        if (f.field === "labels") return h("span", { class: `tag lbl edit lab-${labelHue(f.label)}` }, h("span", { class: "v", text: f.label }), x);
        return h("span", { class: "tag fchip" }, FILTER_NAME[f.field] ? h("span", { class: "k", text: FILTER_NAME[f.field] }) : null, h("span", { class: "v", text: f.label }), x);
      }), L.filters.length > 1 ? h("button", { type: "button", class: "btn quiet small", text: "Clear filters", onclick: () => { L.filters = []; computeRows(); renderFacets(); renderChips(); renderTable(); renderLibFoot(); renderEditor(); } }) : null);
    }

    function visibleCols() {
      const w = L.width;
      const k = new Set(["title", "artist", "time"]);
      if (w >= 470) k.add("album");
      if (w >= 540) k.add("labels");
      if (w >= 700) k.add("year");
      if (w >= 780) k.add("genre");
      if (w >= 860) k.add("track");
      return LCOL_ORDER.filter((c) => k.has(c));
    }
    function sortBy(c) {
      if (L.sort.key === c && !(L.q.trim() && L.best)) L.sort.dir = -L.sort.dir;
      else L.sort = { key: c, dir: 1 };
      L.best = false;
      core.saveSetting("librarySort", L.sort);
      computeRows();
      renderTable();
    }
    function renderTable() {
      const cols = visibleCols();
      L.cols = cols;
      const tpl = cols.map((c) => LCOLS[c].w).join(" ");
      V.cols.style.setProperty("--cols", tpl);
      V.inner.style.setProperty("--cols", tpl);
      const best = L.q.trim() && L.best;
      fill(V.cols, cols.map((c) => {
        const on = !best && L.sort.key === c;
        return h("button", {
          type: "button", class: `lc${on ? " on" : ""}${LCOLS[c].right ? " r" : ""}`, title: `Sort by ${LCOLS[c].label === "#" ? "track number" : LCOLS[c].label.toLowerCase()}`,
          "aria-label": `Sort by ${LCOLS[c].label === "#" ? "track number" : LCOLS[c].label.toLowerCase()}`, onclick: () => sortBy(c),
        }, h("span", { text: LCOLS[c].label }), on ? icon(L.sort.dir > 0 ? "up" : "down") : null);
      }));
      const none = !L.ids.length;
      V.empty.hidden = !none;
      if (none) {
        V.empty.textContent = !libList().list.length
          ? (S.music && S.music.scanning ? "Reading your music folder…" : "No audio files in this folder yet. Add some and they show up here.")
          : L.q.trim() ? `Nothing matches “${L.q.trim()}”${L.filters.length ? " with these filters" : ""}.` : "No tracks have all of these tags.";
      }
      renderRows();
    }
    function lcell(c, t) {
      switch (c) {
        case "title": return h("div", { class: "c c-title" }, eqFor(t), h("span", { class: t.title ? "t" : "t untagged", text: titleOf(t) }), t.exists ? null : h("span", { class: "sub", text: "File not found" }));
        case "track": return h("div", { class: "c r num", text: t.track ? String(t.track) : "" });
        case "year": return h("div", { class: "c r num", text: t.year ? String(t.year) : "" });
        case "time": return h("div", { class: "c r num", text: t.duration ? clock(t.duration) : "" });
        case "labels": return h("div", { class: "c c-labels" }, labelsThatFit(t.labels || [], t.id));
        default: return h("div", { class: `c c-${c}`, text: t[c] || "" });
      }
    }
    /** As many labels as fit the width whole, then "+2" for the rest: hover
     * it (or focus it) for all of the track's labels. */
    let labW = 0;
    function labelsThatFit(ls, tid, width = labW) {
      const out = [];
      let used = 0;
      const est = (l) => 15 + l.length * 6.3;
      for (let i = 0; i < ls.length; i++) {
        const more = i < ls.length - 1 ? 28 : 0;
        if (width && i > 0 && used + est(ls[i]) + more > width) {
          const rest = ls.slice(i);
          out.push(h("span", { class: "tag lbl sm more", text: `+${rest.length}`, tabindex: "-1", dataset: { tid }, "aria-label": `And ${rest.join(", ")}` }));
          break;
        }
        out.push(labelChip(ls[i], true));
        used += est(ls[i]);
      }
      return out;
    }
    // The "+2" of a list's labels, hovered: all the track's labels, in a tip.
    let tipFor = null;
    document.addEventListener("pointerover", (e) => {
      const m = e.target.closest && e.target.closest(".tag.more[data-tid]");
      if (m === tipFor) return;
      tipFor = m;
      if (!m) { hideTip(); return; }
      const t = track(m.dataset.tid);
      if (!t || !(t.labels || []).length) { hideTip(); return; }
      showTip(m, h("div", { class: "tags tip-tags" }, t.labels.map((l) => labelChip(l, true))));
    });
    const dropTip = () => { if (tipFor) { tipFor = null; hideTip(); } };
    document.addEventListener("pointerdown", dropTip, true);
    document.addEventListener("wheel", dropTip, { capture: true, passive: true });
    function renderRows() {
      if (!V.inner) return;
      const total = L.rows.length;
      V.inner.style.height = `${total * RH}px`;
      const top = V.scroll.scrollTop;
      const hgt = V.scroll.clientHeight || 600;
      const a = Math.max(0, Math.floor(top / RH) - 6);
      const b = Math.min(total, Math.ceil((top + hgt) / RH) + 6);
      const kids = [];
      for (let i = a; i < b; i++) {
        const it = L.rows[i];
        if (it.divider) {
          kids.push(h("div", { class: "lr-div", role: "separator", style: { top: `${i * RH}px` } }, h("span", { text: `Close matches (${it.n})` })));
          continue;
        }
        const t = it.t;
        const on = L.sel.has(t.id);
        const r = h("div", {
          class: `lr${on ? " sel" : ""}${L.focus === t.id ? " focus" : ""}${t.exists ? "" : " missing"}${S.player.trackId === t.id ? " playing" : ""}`, role: "option", id: `lr-${t.id}`,
          "aria-selected": String(on), dataset: { tid: t.id }, style: { top: `${i * RH}px` },
        });
        for (const c of L.cols) r.append(lcell(c, t));
        kids.push(r);
      }
      V.inner.replaceChildren(...kids);
      if (L.focus && L.at.has(L.focus)) V.scroll.setAttribute("aria-activedescendant", `lr-${L.focus}`);
      else V.scroll.removeAttribute("aria-activedescendant");
      // The labels column's width, measured once it's drawn (and again when it changes).
      const cell = V.inner.querySelector(".c-labels");
      if (cell && Math.abs(cell.clientWidth - labW) > 4) {
        labW = cell.clientWidth;
        renderRows();
      }
    }
    function syncRows() {
      for (const r of V.inner.children) {
        const id = r.dataset.tid;
        if (!id) continue;
        const on = L.sel.has(id);
        r.classList.toggle("sel", on);
        r.classList.toggle("focus", id === L.focus);
        r.setAttribute("aria-selected", String(on));
      }
      if (L.focus && L.at.has(L.focus)) V.scroll.setAttribute("aria-activedescendant", `lr-${L.focus}`);
    }
    function revealRow(id) {
      const i = L.at.get(id);
      if (i == null) return;
      const top = i * RH;
      const s = V.scroll;
      if (top < s.scrollTop) s.scrollTop = top;
      else if (top + RH > s.scrollTop + s.clientHeight) s.scrollTop = top + RH - s.clientHeight;
      renderRows();
    }
    function setSel(next, focus) {
      // An edit not yet entered is saved before the selection moves on.
      if (!sameSet(next, L.sel) && L.editor && L.editor.dirty()) L.editor.commit();
      const changedSel = !sameSet(next, L.sel);
      L.sel = next;
      L.focus = focus;
      syncRows();
      renderLibFoot();
      if (changedSel) renderEditor(true);
      return true;
    }
    function renderLibFoot() {
      const all = libList().list.length;
      const n = L.ids.length;
      const sel = L.ids.filter((x) => L.sel.has(x));
      const dur = (ids) => ids.reduce((a, id) => a + (track(id).duration || 0), 0);
      const p = core.pl();
      fill(V.foot,
        h("p", { class: "lib-count" },
          L.q.trim() || L.filters.length ? `${n} of ${plural(all, "track")}` : plural(all, "track"),
          n ? `, ${length(dur(L.ids))}` : "",
          sel.length ? `. ${sel.length} selected, ${clock(dur(sel))}: drag ${sel.length === 1 ? "it" : "them"} onto a playlist on the left.` : ". Double-click a track to hear it; drag tracks onto a playlist on the left to add them."),
        sel.length ? h("div", { class: "tools" },
          p ? h("button", { type: "button", class: "btn small", text: `Add to ${p.name}`, onclick: () => addToCurrent(sel) }) : null,
          h("button", { type: "button", class: "btn small", text: "New playlist", onclick: () => { core.newPlaylist("From the library", sel); core.setView("playlist"); } })) : null,
        // Showing only files that have gone: out with them, in one go.
        L.filters.some((f) => f.field === "missing" && f.value === "file") && goneOf(L.ids).length
          ? h("div", { class: "tools" }, h("button", { type: "button", class: "btn small", id: "lib-forget-all", text: `Remove all ${goneOf(L.ids).length} from the library`, onclick: () => forgetGone(L.ids) })) : null);
    }
    function renderEditor(force) {
      const ids = LX.ordered();
      const ed = L.editor;
      if (!ids.length) {
        if (ed && ed.dirty()) return;
        L.editor = null;
        fill(V.edit,
          h("div", { class: "ted-idle" },
            h("h3", { text: "Tags" }),
            h("p", { text: "Select a track to see its tags here and change them. Select several to change them all at once: a genre, an album artist, or labels of your own." }),
            h("p", { text: "Labels are your own tags, like “road trip”, “kids” or “sleep”. Browse by them on the left, and search for them." })));
        return;
      }
      const same = ed && ed.ids.length === ids.length && ed.ids.every((x, i) => x === ids[i]);
      if (same && (ed.dirty() || ed.busy() || (!force && (ed.v === S.tracksVersion || V.edit.contains(document.activeElement))))) return;
      L.editor = makeEditor(ids);
      const top = V.edit.scrollTop;
      fill(V.edit, L.editor.el);
      if (same) V.edit.scrollTop = top;
    }
    function revealInLibrary(id) {
      core.setView("library");
      L.q = "";
      V.q.value = "";
      L.filters = [];
      computeRows();
      L.anchor = id;
      setSel(new Set([id]), id);
      renderView();
      revealRow(id);
      V.scroll.focus({ preventScroll: true });
    }

    // ---- events from the app ------------------------------------------------------------------
    function onProgress(p) {
      S.libProgress = p;
      if (S.music && !S.music.scanning) S.music = { ...S.music, scanning: true };
      if (S.view === "library" && V.root && !V.headRaf) V.headRaf = requestAnimationFrame(() => { V.headRaf = 0; renderLibHead(); core.renderDisplay(); });
    }
    function onScanned(x) {
      scanEvents++;
      S.music = x.info;
      S.libProgress = null;
      if (x.tracks && x.tracks.length) core.putTracks(x.tracks);
      else refresh();
      if (S.view === "library") core.renderDisplay();
    }
    // A watched folder on a network drive only hears of changes made from
    // this computer: coming back to the app after a while looks again.
    window.addEventListener("focus", () => {
      const m = S.music;
      if (m && m.root && !m.scanning && m.lastScan && Date.now() - m.lastScan > 10 * 60000) api.libraryScan().catch(() => {});
    });
    function onKey(e) {
      const mod = e.ctrlKey || e.metaKey;
      // Ctrl+Z in the library (not while typing): the last tag edit, undone.
      if (mod && !e.shiftKey && e.key.toLowerCase() === "z" && S.view === "library" && !e.target.closest("input, textarea, select") && document.getElementById("modal-back").hidden) {
        if (undoTags()) { e.preventDefault(); return true; }
      }
      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        if (S.view === "library" && root()) { V.q.focus(); V.q.select(); } else openSearch();
        return true;
      }
      return false;
    }
    /** For the display band, while the library is open. */
    function display() {
      const m = S.music || {};
      const p = S.libProgress;
      const n = libList().list.length;
      if (m.scanning) return { main: "SCANNING", count: p && p.total ? String(Math.floor((100 * p.done) / p.total)) : "", unit: p && p.total ? "%" : "", time: "", sub: statusText() };
      if (!root()) return { main: "LIBRARY", count: "", unit: "", time: "", sub: "Choose the folder your music is in." };
      return { main: "LIBRARY", count: n < 1000 ? String(n) : "", unit: n < 1000 ? "TRK" : "", time: "", sub: `${plural(n, "track")} in ${root()}.` };
    }

    return {
      tagChips, rowTags(t, ids) {
        const chips = tagChips(t);
        const why = whyNot(t);
        return h("div", { class: "row-tags" },
          h("div", { class: "tags" }, chips.length ? chips : h("span", { class: "tags-none", text: "No tags" })),
          !why ? h("button", { type: "button", class: "btn quiet small", text: "Edit tags…", onclick: (ev) => { ev.stopPropagation(); editTagsDialog(ids && ids.includes(t.id) ? ids : [t.id]); } })
            : root() && t.exists && !inMusic(t) ? h("span", { class: "tags-why", text: "Not in your music folder" }) : null);
      },
      canTag: (t) => !whyNot(t),
      inMusic, editTagsDialog, renameDialog, renderView, refresh, useFolder, markPlaying, trackToPlay, renderSearch, openSearch, closeSearch, searchRoot,
      onProgress, onScanned, onKey, display, revealInLibrary,
      /** The folder's state from the window's start, unless a scan's event beat it here. */
      initial(music) {
        if (!scanEvents) S.music = music || null;
      },
      /** The settings are in: the table's sort order. */
      loaded() {
        try {
          const v = JSON.parse((S.settings && S.settings.librarySort) || "{}");
          if (v && LCOLS[v.key] && (v.dir === 1 || v.dir === -1)) L.sort = { key: v.key, dir: v.dir };
        } catch { /* keep album order */ }
      },
      state: { L, SE },
    };
  };
})(window.Fatstick = window.Fatstick || {});
