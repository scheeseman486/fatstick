// Playlist2USB's window. Plain DOM, no framework.
// State lives in S. Every playlist edit goes through edit(), so it can be
// undone, and is saved to the library straight away. Rendering is a few
// functions that rebuild their own area; the stick panel is split into
// sections so typing a name template doesn't lose focus. Everything outside
// the window goes through P2U.api: the app's own commands, or a demo version
// in a plain browser.
"use strict";
(function () {
  const P = window.P2U;
  const { h, fill, icon, clock, length, bytes, ago, plural, db, menu, popover, closeMenu, modal, closeModal, modalOpen, toast } = P.ui;
  const R = P.rules;
  const N = P.names;
  const api = P.api;
  const DEMO = api.kind === "demo";
  const $ = (id) => document.getElementById(id);
  const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
  const clone = (o) => JSON.parse(JSON.stringify(o));
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  let uid = 0;
  // Ids are saved, so they must be unique across sessions.
  const newId = (p) => `${p}${Date.now().toString(36)}${(++uid).toString(36)}${Math.floor(Math.random() * 1296).toString(36)}`;
  const pad2 = (n, w = 2) => String(n).padStart(w, "0");

  const PRESETS = [
    { label: "01 - Artist - Title", tpl: "{n} - [{artist} - ]{title|filename}" },
    { label: "01 - Title", tpl: "{n} - {title|filename}" },
    { label: "01 Title (short)", tpl: "{n} {title|filename}" },
    { label: "01 (number only)", tpl: "{n}" },
    { label: "Artist - Title (no number)", tpl: "[{artist} - ]{title|filename}" },
  ];
  /** The profile the app ships with: safe for most cheap players. */
  const GENERIC = {
    id: "prof-generic", name: "Generic", builtIn: true,
    bitrateCap: 320, mode: "cbr", sampleRates: [44100, 48000], tags: "id3v23", id3v1: false, cover: "small",
    layout: "folders", fileTemplate: "{n} - [{artist} - ]{title|filename}", folderTemplate: "{n} {playlist}",
    ascii: false, maxLen: 0, trackTagIsPosition: true, numberInTitle: false,
  };

  const S = {
    tracks: new Map(),
    playlists: [],
    profiles: [],
    sets: [],
    /** The last sync of each stick, by stick id. */
    syncs: {},
    sticks: [],
    stickError: null,
    current: null,
    setId: null,
    sel: new Set(),
    anchor: null,
    focus: null,
    cols: { album: true, format: true, stick: true, gain: true, name: false },
    listWidth: 800,
    undo: [],
    redo: [],
    clip: null,
    /** Preparation keys whose files are in the cache, and ones that failed. */
    cache: new Set(),
    prepFailed: new Map(),
    analyzing: new Set(),
    sync: null,
    renaming: null,
    theme: "system",
    scan: null,
    ffmpegError: null,
    version: "",
    player: { entryId: null, playlistId: null, playing: false, pos: 0, dur: 0, asOnStick: false, volume: 0.8, silent: false, error: null, from: "source" },
  };
  const hostTheme = document.documentElement.getAttribute("data-theme");

  // ---- lookups ----------------------------------------------------------------
  const pl = (id = S.current) => S.playlists.find((p) => p.id === id);
  const stickSet = (id = S.setId) => S.sets.find((s) => s.id === id);
  const prof = (id) => S.profiles.find((p) => p.id === id) || S.profiles[0] || GENERIC;
  const unknownTrack = (id) => ({ id, path: "", filename: "(a track the library lost)", title: "", artist: "", album: "", codec: "", exists: false, duration: 0, size: 0 });
  const track = (id) => S.tracks.get(id) || unknownTrack(id);
  const device = (id) => S.sticks.find((s) => s.id === id);
  /** The stick a set syncs to, if it's plugged in. Device names (sdb,
   * PhysicalDrive2, disk4) go to whatever is plugged in next, so a set also
   * remembers the stick's model and size: another stick in the same place
   * isn't taken for it, and isn't erased without being picked. */
  const chosen = (s) => {
    const d = device(s.stickId);
    return d && (!s.stickModel || (d.name === s.stickModel && d.sizeBytes === s.stickSize)) ? d : null;
  };
  const setsFor = (plId) => S.sets.filter((s) => s.playlistIds.includes(plId));
  function listSet() {
    const cur = stickSet();
    if (cur && cur.playlistIds.includes(S.current)) return cur;
    return setsFor(S.current)[0] || null;
  }
  const listProfile = () => (listSet() ? prof(listSet().profileId) : prof(GENERIC.id));

  // ---- planning: what a sync would write ------------------------------------------
  function planPlaylist(p, profile) {
    const items = p.entries.map((e) => ({ entry: e, t: track(e.trackId) }));
    const ds = items.map((it) => R.decide(it.t, profile));
    const gs = R.gains(items.map((i) => i.t), ds, p.norm);
    items.forEach((it, i) => {
      it.d = ds[i];
      it.g = gs[i];
      it.bytes = R.outBytes(it.t, it.d, profile);
      // Loudness not measured yet: the gain (and so the recipe) isn't known.
      it.waiting = !!(it.g && it.g.unknown);
      it.recipe = R.recipe(it.t, it.d, it.waiting ? null : it.g, profile);
      it.key = it.recipe && it.recipe.kind !== "copy" && !it.waiting ? `${it.t.id}|${R.describe(it.recipe)}` : null;
    });
    const present = items.filter((it) => it.d.action !== "skip");
    const nm = N.fileNames(present.map((it) => ({ tags: it.t, filename: it.t.filename })), {
      template: profile.fileTemplate, ascii: profile.ascii, maxLen: profile.maxLen, playlist: p.name,
    });
    present.forEach((it, i) => {
      it.name = nm.names[i].name;
      it.notes = nm.names[i].notes;
    });
    return { p, items, present, errors: nm.errors, numbered: nm.numbered };
  }

  function planSet(s) {
    if (!s) return null;
    const profile = prof(s.profileId);
    const pls = s.playlistIds.map((id) => pl(id)).filter(Boolean);
    const single = profile.layout === "single";
    const used = single ? pls.slice(0, 1) : pls;
    const fnames = N.folderNames(used, { template: used.length > 1 ? profile.folderTemplate : "{playlist}", ascii: profile.ascii }).names;
    const folders = used.map((p, i) => ({ ...planPlaylist(p, profile), folder: single ? null : fnames[i].name }));
    const dev = chosen(s);
    const lay = dev
      ? R.layout(dev.sizeBytes, folders.map((f, i) => ({ index: i, top: single, files: f.present.map((it) => ({ bytes: it.bytes, entry: it.entry.id })) })))
      : null;
    const all = folders.flatMap((f) => f.present);
    const keys = [...new Set(all.map((it) => it.key).filter(Boolean))];
    return {
      s, profile, single, folders, dev, lay, all, keys,
      ignored: single ? pls.slice(1) : [],
      pending: keys.filter((k) => !S.cache.has(k)),
      failed: keys.filter((k) => S.prepFailed.has(k)),
      waiting: all.filter((it) => it.waiting),
      total: all.reduce((a, it) => a + it.bytes, 0),
      skipped: folders.flatMap((f) => f.items.filter((it) => it.d.action === "skip")),
      errors: [...new Set(folders.flatMap((f) => f.errors))],
      numbered: folders.every((f) => f.numbered),
    };
  }

  /** Everything about a stick's settings that changes what a sync writes. */
  function recipeOf(plan) {
    const pr = plan.profile;
    const norm = plan.folders.map((f) => (f.p.norm.on ? `${f.p.norm.target}${f.p.norm.mode}` : "off")).join(",");
    return [pr.bitrateCap, pr.mode, pr.layout, pr.fileTemplate, pr.folderTemplate, pr.ascii, pr.maxLen, pr.tags, pr.cover, pr.id3v1, pr.trackTagIsPosition, pr.numberInTitle, norm].join("|");
  }

  function staleness(plan) {
    const ls = S.syncs[plan.s.id];
    if (!ls || !ls.content) return { never: true, parts: [] };
    const now = plan.folders.map((f) => f.present.map((it) => it.t.id));
    const flatNow = now.flat();
    const flatWas = ls.content.flat();
    const added = flatNow.filter((id) => !flatWas.includes(id)).length;
    const removed = flatWas.filter((id) => !flatNow.includes(id)).length;
    const a = flatNow.filter((id) => flatWas.includes(id));
    const b = flatWas.filter((id) => flatNow.includes(id));
    const moved = a.some((id, i) => id !== b[i]) || now.length !== ls.content.length;
    const parts = [];
    if (added) parts.push(`${plural(added, "track")} added`);
    if (removed) parts.push(`${plural(removed, "track")} removed`);
    if (moved) parts.push("order changed");
    if (recipeOf(plan) !== ls.recipe) parts.push("settings changed");
    return { at: ls.at, stickId: ls.stickId, stickName: ls.stickName, parts, current: parts.length === 0 };
  }

  // ---- saving to the library ----------------------------------------------------------
  // The window keeps what was last saved of each playlist, profile and stick,
  // and sends only what changed (and the order) after every edit.
  const saved = { playlist: new Map(), profile: new Map(), set: new Map() };
  const savedOrder = { playlist: "", profile: "", set: "" };
  let persisting = false;
  let persistAgain = false;
  function remember(kind, list) {
    saved[kind] = new Map(list.map((x) => [x.id, JSON.stringify(x)]));
    savedOrder[kind] = list.map((x) => x.id).join();
  }
  async function persist() {
    if (persisting) { persistAgain = true; return; }
    persisting = true;
    try {
      for (const [kind, list] of [["playlist", S.playlists], ["profile", S.profiles], ["set", S.sets]]) {
        // What is sent is what gets remembered: an edit made while this save
        // is under way is still unsaved afterwards, and goes in the next one.
        const now = new Map(list.map((x) => [x.id, JSON.stringify(x)]));
        const ids = list.map((x) => x.id);
        const prev = saved[kind];
        const put = ids.filter((id) => prev.get(id) !== now.get(id)).map((id) => JSON.parse(now.get(id)));
        const del = [...prev.keys()].filter((id) => !now.has(id));
        if (!put.length && !del.length && ids.join() === savedOrder[kind]) continue;
        await api.saveDocs(kind, put, del, ids);
        saved[kind] = now;
        savedOrder[kind] = ids.join();
      }
    } catch (e) {
      toast(`Couldn't save to the library: ${e}`);
    } finally {
      persisting = false;
      if (persistAgain) { persistAgain = false; persist(); }
    }
  }
  let settingTimers = {};
  function saveSetting(key, value) {
    clearTimeout(settingTimers[key]);
    settingTimers[key] = setTimeout(() => api.setSetting(key, typeof value === "string" ? value : JSON.stringify(value)).catch(() => {}), 400);
  }
  /** After any change: save, and update the background work. */
  function changed() {
    persist();
    scheduleWork();
  }

  // ---- undoable edits ---------------------------------------------------------------
  const snapshot = () => clone({ playlists: S.playlists, sets: S.sets });
  function edit(label, fn, coalesce) {
    const before = snapshot();
    if (fn() === false) return false;
    const last = S.undo[S.undo.length - 1];
    if (coalesce && last && last.label === label && Date.now() - last.t < 1500) {
      last.t = Date.now();
    } else {
      S.undo.push({ label, before, current: S.current, t: Date.now() });
      if (S.undo.length > 300) S.undo.shift();
    }
    S.redo = [];
    renderAll();
    changed();
    return true;
  }
  function restore(snap) {
    S.playlists = clone(snap.playlists);
    S.sets = clone(snap.sets);
  }
  function afterRestore(current) {
    if (current && pl(current)) S.current = current;
    if (!pl()) S.current = S.playlists[0]?.id;
    if (!stickSet()) S.setId = S.sets[0]?.id;
    const ids = new Set(pl()?.entries.map((e) => e.id));
    S.sel = new Set([...S.sel].filter((id) => ids.has(id)));
    if (!ids.has(S.focus)) S.focus = null;
    const pp = S.player;
    if (pp.entryId && !pl(pp.playlistId)?.entries.some((e) => e.id === pp.entryId)) stopPlayback();
    renderAll();
    changed();
  }
  function undo() {
    const u = S.undo.pop();
    if (!u) { toast("Nothing to undo."); return; }
    S.redo.push({ label: u.label, before: snapshot(), current: S.current, t: 0 });
    restore(u.before);
    afterRestore(u.current);
    toast(`Undid: ${u.label}`);
  }
  function redo() {
    const r = S.redo.pop();
    if (!r) { toast("Nothing to redo."); return; }
    S.undo.push({ label: r.label, before: snapshot(), current: S.current, t: 0 });
    restore(r.before);
    afterRestore(r.current);
    toast(`Redid: ${r.label}`);
  }

  // ---- selection ------------------------------------------------------------------------
  const entryIds = () => pl().entries.map((e) => e.id);
  function selectOnly(id) {
    S.sel = new Set(id ? [id] : []);
    S.anchor = id;
    S.focus = id;
  }
  function selectRange(a, b, add) {
    const ids = entryIds();
    const i = ids.indexOf(a);
    const j = ids.indexOf(b);
    if (i < 0 || j < 0) return selectOnly(b);
    if (!add) S.sel = new Set();
    for (let k = Math.min(i, j); k <= Math.max(i, j); k++) S.sel.add(ids[k]);
    S.focus = b;
  }
  const selectedInOrder = () => pl().entries.filter((e) => S.sel.has(e.id));
  function syncSelection() {
    const list = $("list");
    for (const r of list.children) {
      const id = r.dataset.id;
      if (!id) continue;
      const on = S.sel.has(id);
      r.classList.toggle("sel", on);
      r.setAttribute("aria-selected", String(on));
      r.classList.toggle("focus", id === S.focus);
    }
    if (S.focus) list.setAttribute("aria-activedescendant", `row-${S.focus}`);
    else list.removeAttribute("aria-activedescendant");
    renderFoot();
  }
  function revealFocus() {
    const r = S.focus && document.getElementById(`row-${S.focus}`);
    if (r) r.scrollIntoView({ block: "nearest" });
  }

  // ---- playlist actions ---------------------------------------------------------------------
  function moveBlock(ids, index) {
    const p = pl();
    const sel = new Set(ids);
    const was = p.entries.map((e) => e.id).join();
    const moving = p.entries.filter((e) => sel.has(e.id));
    const keep = p.entries.slice(0, index).filter((e) => !sel.has(e.id)).length;
    const rest = p.entries.filter((e) => !sel.has(e.id));
    const next = [...rest.slice(0, keep), ...moving, ...rest.slice(keep)];
    if (next.map((e) => e.id).join() === was) return false;
    p.entries = next;
  }
  function moveSelectionTo(index) {
    const n = S.sel.size;
    if (!n) return;
    edit(`Move ${plural(n, "track")}`, () => moveBlock([...S.sel], index));
  }
  function nudge(dir) {
    const n = S.sel.size;
    if (!n) return;
    edit(`Move ${plural(n, "track")}`, () => {
      const e = pl().entries;
      const was = e.map((x) => x.id).join();
      if (dir < 0) {
        for (let i = 1; i < e.length; i++) if (S.sel.has(e[i].id) && !S.sel.has(e[i - 1].id)) [e[i - 1], e[i]] = [e[i], e[i - 1]];
      } else {
        for (let i = e.length - 2; i >= 0; i--) if (S.sel.has(e[i].id) && !S.sel.has(e[i + 1].id)) [e[i + 1], e[i]] = [e[i], e[i + 1]];
      }
      if (e.map((x) => x.id).join() === was) return false;
    }, true);
    revealFocus();
  }
  function removeSelected() {
    const p = pl();
    const n = S.sel.size;
    if (!n) return;
    const ids = entryIds();
    const after = ids.find((id, i) => !S.sel.has(id) && i > ids.indexOf([...S.sel].pop()));
    const removed = new Set(S.sel);
    edit(`Remove ${plural(n, "track")} from ${p.name}`, () => { p.entries = p.entries.filter((e) => !removed.has(e.id)); });
    if (S.player.entryId && removed.has(S.player.entryId)) stopPlayback();
    selectOnly(after || pl().entries[pl().entries.length - 1]?.id || null);
    syncSelection();
    toast(`Removed ${plural(n, "track")} from ${p.name}. The files are untouched.`, { label: "Undo", run: undo });
  }
  function addTracks(trackIds, plId, index, label) {
    const p = pl(plId);
    const fresh = trackIds.map((tid) => ({ id: newId("e"), trackId: tid }));
    const at = index == null ? p.entries.length : index;
    edit(label || `Add ${plural(fresh.length, "track")} to ${p.name}`, () => { p.entries.splice(at, 0, ...fresh); });
    if (plId === S.current) {
      S.sel = new Set(fresh.map((e) => e.id));
      S.anchor = fresh[0]?.id;
      S.focus = fresh[fresh.length - 1]?.id;
      syncSelection();
      revealFocus();
    }
    return fresh;
  }
  function sortBy(kind) {
    const labels = { album: "Sort by album and track", artist: "Sort by artist", title: "Sort by title", file: "Sort by file name", shuffle: "Shuffle", reverse: "Reverse" };
    edit(labels[kind], () => {
      const p = pl();
      const T = (e) => track(e.trackId);
      const was = p.entries.map((e) => e.id).join();
      if (kind === "reverse") p.entries.reverse();
      else if (kind === "shuffle") {
        for (let i = p.entries.length - 1; i > 0; i--) {
          const j = Math.floor(Math.random() * (i + 1));
          [p.entries[i], p.entries[j]] = [p.entries[j], p.entries[i]];
        }
      } else {
        const key = {
          album: (t) => [t.albumartist || t.artist, t.album, t.disc || 0, t.track || 0, t.filename],
          artist: (t) => [t.artist, t.album, t.track || 0],
          title: (t) => [t.title || t.filename],
          file: (t) => [t.path],
        }[kind];
        p.entries.sort((a, b) => {
          const x = key(T(a));
          const y = key(T(b));
          for (let i = 0; i < x.length; i++) {
            const c = typeof x[i] === "number" ? x[i] - y[i] : collator.compare(String(x[i] || ""), String(y[i] || ""));
            if (c) return c;
          }
          return 0;
        });
      }
      if (p.entries.map((e) => e.id).join() === was) return false;
    });
  }
  function cutCopy(cut) {
    const sel = selectedInOrder();
    if (!sel.length) return;
    S.clip = sel.map((e) => e.trackId);
    if (cut) {
      const ids = new Set(S.sel);
      edit(`Cut ${plural(sel.length, "track")}`, () => { pl().entries = pl().entries.filter((e) => !ids.has(e.id)); });
      selectOnly(null);
      syncSelection();
    }
    toast(`${cut ? "Cut" : "Copied"} ${plural(sel.length, "track")}. Select a track and paste to put ${sel.length === 1 ? "it" : "them"} after it.`);
  }
  function paste() {
    if (!S.clip || !S.clip.length) return;
    const ids = entryIds();
    const at = S.focus && ids.includes(S.focus) ? ids.indexOf(S.focus) + 1 : ids.length;
    addTracks(S.clip, S.current, at, `Paste ${plural(S.clip.length, "track")}`);
  }
  function newPlaylist(name, trackIds) {
    const id = newId("p-");
    edit(`New playlist`, () => {
      S.playlists.push({ id, name: name || "New playlist", entries: (trackIds || []).map((t) => ({ id: newId("e"), trackId: t })), norm: { on: false, target: -18, mode: "track" } });
    });
    openPlaylist(id);
    if (!name) startRename("playlist", id);
    return id;
  }
  function openPlaylist(id) {
    if (S.current === id) return;
    S.current = id;
    selectOnly(null);
    const sets = setsFor(id);
    if (sets.length && !sets.includes(stickSet())) S.setId = sets[0].id;
    saveSetting("currentPlaylist", id);
    saveSetting("currentSet", S.setId);
    renderAll();
    $("list").scrollTop = 0;
  }

  // ---- rendering: sidebar ------------------------------------------------------------------
  function renderSide() {
    if (S.renaming) return;
    const side = $("side");
    const plItems = S.playlists.map((p) => {
      const ts = p.entries.map((e) => track(e.trackId));
      const dur = ts.reduce((a, t) => a + (t.exists ? t.duration || 0 : 0), 0);
      return h("li", null, h("button", {
        type: "button", class: `side-item${p.id === S.current ? " current" : ""}`, dataset: { pl: p.id },
        "aria-current": p.id === S.current ? "page" : null,
        onclick: () => openPlaylist(p.id),
        ondblclick: () => startRename("playlist", p.id),
        oncontextmenu: (e) => { e.preventDefault(); playlistMenu(p, { x: e.clientX, y: e.clientY }); },
      }, h("span", { class: "nm", text: p.name }), h("span", { class: "mt", text: `${plural(p.entries.length, "track")}, ${length(dur)}` })));
    });
    const setItems = S.sets.map((s) => {
      const plan = planSet(s);
      const st = staleness(plan);
      const flag = st.never ? h("span", { class: "flag ok", text: "not synced" }) : st.current ? null : h("span", { class: "flag", text: "out of date" });
      return h("li", null, h("button", {
        type: "button", class: `side-item${s.id === S.setId ? " current" : ""}`, dataset: { set: s.id },
        onclick: () => {
          if (S.setId === s.id) return;
          S.setId = s.id;
          saveSetting("currentSet", s.id);
          if (!s.playlistIds.includes(S.current) && s.playlistIds[0]) openPlaylist(s.playlistIds[0]);
          renderAll();
        },
        ondblclick: () => startRename("set", s.id),
        oncontextmenu: (e) => { e.preventDefault(); setMenu(s, { x: e.clientX, y: e.clientY }); },
      }, h("span", { class: "nm", text: s.name }), flag,
      h("span", { class: "mt", text: `${plural(plan.folders.length, "playlist")}, ${plan.profile.name}` })));
    });
    fill(side, 
      h("section", null,
        h("div", { class: "side-h" }, h("h2", { text: "Playlists" }),
          h("button", { type: "button", class: "btn quiet small icon", title: "New playlist", "aria-label": "New playlist", onclick: () => newPlaylist() }, icon("plus"))),
        h("ul", { id: "side-pls" }, plItems),
        h("button", { type: "button", class: "btn quiet small side-more", text: "Import a playlist file…", onclick: importPlaylist })),
      h("section", null,
        h("div", { class: "side-h" }, h("h2", { text: "Sticks" }),
          h("button", { type: "button", class: "btn quiet small icon", title: "New stick", "aria-label": "New stick", onclick: newSet }, icon("plus"))),
        h("ul", null, setItems)),
    );
  }

  function startRename(kind, id) {
    const obj = kind === "playlist" ? pl(id) : stickSet(id);
    if (!obj) return;
    S.renaming = null;
    renderSide();
    const btn = document.querySelector(kind === "playlist" ? `[data-pl="${id}"]` : `[data-set="${id}"]`);
    if (!btn) return;
    S.renaming = { kind, id };
    const input = h("input", { class: "input", value: obj.name, "aria-label": "Name" });
    const done = (save) => {
      if (!S.renaming) return;
      S.renaming = null;
      const v = input.value.trim();
      if (save && v && v !== obj.name) edit(`Rename to ${v}`, () => { (kind === "playlist" ? pl(id) : stickSet(id)).name = v; });
      else renderAll();
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); done(true); }
      if (e.key === "Escape") { e.preventDefault(); done(false); }
      e.stopPropagation();
    });
    input.addEventListener("blur", () => done(true));
    btn.replaceWith(input);
    input.focus();
    input.select();
  }

  function playlistMenu(p, at) {
    const others = S.sets.filter((s) => !s.playlistIds.includes(p.id));
    menu(at, [
      { label: "Rename", action: () => startRename("playlist", p.id) },
      { label: "Duplicate", action: () => newPlaylist(`${p.name} copy`, p.entries.map((e) => e.trackId)) },
      ...(others.length ? ["-", { heading: "Put on a stick" }, ...others.map((s) => ({ label: s.name, action: () => edit(`Put ${p.name} on ${s.name}`, () => { s.playlistIds.push(p.id); }) }))] : []),
      "-",
      {
        label: "Remove playlist", disabled: S.playlists.length < 2,
        action: () => {
          edit(`Remove playlist ${p.name}`, () => {
            S.playlists = S.playlists.filter((x) => x.id !== p.id);
            for (const s of S.sets) s.playlistIds = s.playlistIds.filter((x) => x !== p.id);
            if (S.current === p.id) { S.current = S.playlists[0].id; selectOnly(null); }
          });
          if (S.player.playlistId === p.id) stopPlayback();
          toast(`Removed the playlist ${p.name}. Its files are untouched.`, { label: "Undo", run: undo });
        },
      },
    ]);
  }
  function setMenu(s, at) {
    menu(at, [
      { label: "Rename", action: () => startRename("set", s.id) },
      {
        label: "Remove from the list", disabled: S.sets.length < 2,
        action: () => {
          edit(`Remove ${s.name}`, () => { S.sets = S.sets.filter((x) => x.id !== s.id); if (S.setId === s.id) S.setId = S.sets[0].id; });
          toast(`Removed ${s.name} from the list. Nothing on the stick changed.`, { label: "Undo", run: undo });
        },
      },
    ]);
  }
  function newSet() {
    const id = newId("set-");
    edit("New stick", () => {
      S.sets.push({ id, name: `Stick ${S.sets.length + 1}`, profileId: S.profiles[0].id, playlistIds: [S.current], stickId: null, label: "" });
      S.setId = id;
    });
    startRename("set", id);
  }

  // ---- rendering: playlist header and list ------------------------------------------------
  const COLS = {
    num: { label: "#", w: "3.1em" },
    title: { label: "Title", w: "minmax(8em, 2fr)" },
    artist: { label: "Artist", w: "minmax(5em, 1.25fr)" },
    album: { label: "Album", w: "minmax(5em, 1.15fr)" },
    time: { label: "Time", w: "3.6em" },
    format: { label: "Format", w: "7.2em" },
    stick: { label: "On the stick", w: "7.6em" },
    gain: { label: "Gain", w: "4.6em" },
    name: { label: "Name on the stick", w: "minmax(9em, 2fr)" },
  };
  function visibleCols(p) {
    const w = S.listWidth;
    const k = ["num", "title"];
    if (w >= 420) k.push("artist");
    if (w >= 760 && S.cols.album) k.push("album");
    k.push("time");
    if (w >= 600 && S.cols.format) k.push("format");
    if (w >= 500 && S.cols.stick) k.push("stick");
    if (w >= 660 && S.cols.gain && p.norm.on) k.push("gain");
    if (w >= 700 && S.cols.name) k.push("name");
    return k;
  }

  function renderHead() {
    const p = pl();
    const s = listSet();
    const plan = planPlaylist(p, listProfile());
    const dur = plan.present.reduce((a, it) => a + it.t.duration, 0);
    const missing = plan.items.length - plan.present.length;
    const norm = p.norm;
    const anchor = (fn) => (e) => fn(e.currentTarget);
    fill($("pl-head"), 
      h("div", { class: "pl-title" },
        h("h1", { id: "pl-name", text: p.name }),
        h("p", { class: "pl-meta" },
          `${plural(plan.items.length, "track")}, ${length(dur)}`,
          missing ? h("span", { class: "warn", text: `, ${missing} missing` }) : null,
          s ? `. On ${s.name}.` : ". Not on a stick yet.")),
      h("div", { class: "tools" },
        h("button", { type: "button", class: `btn small${norm.on ? " tool-on" : ""}`, "data-menu-anchor": "", "aria-haspopup": "dialog", onclick: anchor(volumePopover) },
          norm.on ? `Even volume, ${String(norm.target).replace("-", "−")} LUFS` : "Even volume: off"),
        h("button", { type: "button", class: "btn small", "data-menu-anchor": "", "aria-haspopup": "menu", onclick: anchor(orderMenu) }, "Order", h("span", { class: "caret" }, icon("caret"))),
        h("button", { type: "button", class: "btn small", "data-menu-anchor": "", "aria-haspopup": "menu", onclick: anchor(columnsMenu) }, "Columns", h("span", { class: "caret" }, icon("caret"))),
        h("button", { type: "button", class: "btn small", "data-menu-anchor": "", "aria-haspopup": "menu", onclick: anchor(addMenu) }, icon("plus"), "Add", h("span", { class: "caret" }, icon("caret")))),
    );
  }

  function orderMenu(a) {
    menu(a, [
      { heading: "Rewrites the playlist order" },
      { label: "Sort by album and track", action: () => sortBy("album") },
      { label: "Sort by artist", action: () => sortBy("artist") },
      { label: "Sort by title", action: () => sortBy("title") },
      { label: "Sort by file name", action: () => sortBy("file") },
      "-",
      { label: "Shuffle", action: () => sortBy("shuffle") },
      { label: "Reverse", action: () => sortBy("reverse") },
    ]);
  }
  function columnsMenu(a) {
    const t = (k) => () => { S.cols[k] = !S.cols[k]; saveSetting("columns", S.cols); renderList(); };
    menu(a, [
      { label: "Album", checked: S.cols.album, action: t("album") },
      { label: "Format", checked: S.cols.format, action: t("format") },
      { label: "On the stick", checked: S.cols.stick, action: t("stick") },
      { label: "Gain (when evening the volume)", checked: S.cols.gain, action: t("gain") },
      { label: "Name on the stick", checked: S.cols.name, action: t("name") },
    ]);
  }
  function addMenu(a) {
    menu(a, [
      { label: "Add files…", key: "Ctrl+O", action: () => addFiles(false) },
      { label: "Add a folder…", action: () => addFiles(true) },
      "-",
      { label: "Import a playlist file…", action: importPlaylist },
      { label: "Export this playlist…", action: exportPlaylist, disabled: !pl().entries.length },
    ]);
  }

  function volumePopover(a) {
    const p = pl();
    const n = p.norm;
    const out = h("output", { text: "" });
    const warn = h("p", { class: "warn" });
    const range = h("input", { type: "range", min: "-23", max: "-12", step: "0.5", value: String(n.target), "aria-label": "Target loudness", id: "norm-target" });
    const on = h("input", { type: "checkbox", checked: n.on, id: "norm-on" });
    const MODES = [["track", "Each track on its own"], ["album", "Keep albums together"]];
    const radios = MODES.map(([v]) => h("input", { type: "radio", name: "norm-mode", value: v, checked: n.mode === v }));
    const show = () => {
      out.textContent = `${String(p.norm.target).replace("-", "−")} LUFS${p.norm.target === -18 ? " (suggested)" : ""}`;
      warn.textContent = p.norm.target > -16 ? "Louder targets need more gain, so more tracks stop short of the target at their peaks." : "";
      range.disabled = !p.norm.on;
      for (const r of radios) r.disabled = !p.norm.on;
    };
    const change = (fn, label, co) => { edit(label, () => { fn(p.norm); }, co); show(); };
    on.addEventListener("change", () => change((x) => { x.on = on.checked; }, on.checked ? `Even out the volume of ${p.name}` : `Stop evening the volume of ${p.name}`));
    range.addEventListener("input", () => change((x) => { x.target = Number(range.value); }, "Change the target loudness", true));
    radios.forEach((r, i) => r.addEventListener("change", () => change((x) => { x.mode = MODES[i][0]; }, MODES[i][1])));
    popover(a, h("div", { style: { display: "grid", gap: "10px" } },
      h("label", { class: "check" }, on, h("span", null, h("strong", { text: "Even out the volume" }), h("br"), h("span", { text: `For ${p.name}, on every stick it goes on.` }))),
      h("p", { text: "This changes the audio on the stick, because cheap players ignore ReplayGain tags. MP3s that are copied get lossless 1.5 dB steps; tracks that are encoded anyway get the exact gain." }),
      h("div", { class: "target" }, range, out),
      warn,
      h("fieldset", null, h("legend", { text: "Gain" }), radios.map((r, i) => h("label", { class: "check" }, r, h("span", { text: MODES[i][1] })))),
    ));
    show();
  }

  function renderList() {
    const p = pl();
    const prf = listProfile();
    const plan = planPlaylist(p, prf);
    const byEntry = new Map(plan.items.map((it) => [it.entry.id, it]));
    const cols = visibleCols(p);
    const tpl = cols.map((c) => COLS[c].w).join(" ");
    const list = $("list");
    const head = $("list-cols");
    list.style.setProperty("--cols", tpl);
    head.style.setProperty("--cols", tpl);
    head.replaceChildren(...cols.map((c) => h("div", { class: `c h-${c}`, style: c === "time" || c === "gain" || c === "num" ? { "text-align": "right" } : null, text: COLS[c].label })));
    const padN = Math.max(2, String(p.entries.length).length);
    const pp = S.player;
    const rows = p.entries.map((e, i) => {
      const it = byEntry.get(e.id);
      const t = it.t;
      const playing = pp.entryId === e.id && pp.playlistId === p.id;
      const r = h("div", {
        class: `row${S.sel.has(e.id) ? " sel" : ""}${S.focus === e.id ? " focus" : ""}${t.exists ? "" : " missing"}${playing ? " playing" : ""}`,
        role: "option", id: `row-${e.id}`, dataset: { id: e.id }, "aria-selected": String(S.sel.has(e.id)), title: t.path,
      });
      for (const c of cols) r.append(cell(c, it, i, padN, playing, cols, prf));
      return r;
    });
    list.replaceChildren(...rows);
    const empty = $("list-empty");
    empty.hidden = rows.length > 0;
    if (!rows.length) {
      empty.replaceChildren(
        h("p", { text: "This playlist is empty. Drop audio files or folders here, or add them from your music folder." }),
        h("div", { class: "tools", style: { "justify-content": "center" } },
          h("button", { type: "button", class: "btn", text: "Add files…", onclick: () => addFiles(false) }),
          h("button", { type: "button", class: "btn", text: "Add a folder…", onclick: () => addFiles(true) })),
      );
    }
    if (S.focus) list.setAttribute("aria-activedescendant", `row-${S.focus}`);
    renderFoot();
  }

  function cell(c, it, i, padN, playing, cols, prf) {
    const t = it.t;
    switch (c) {
      case "num":
        return h("div", { class: "c c-num num" }, playing ? h("span", { class: `eq${S.player.playing ? "" : " paused"}`, "aria-label": S.player.playing ? "Playing" : "Paused" }, h("i"), h("i"), h("i")) : pad2(i + 1, padN));
      case "title": {
        const sub = !t.exists ? "File not found" : !cols.includes("artist") ? t.artist : null;
        return h("div", { class: "c c-title" },
          h("span", { class: t.title ? "t" : "t untagged", text: t.title || t.filename }),
          sub ? h("span", { class: "sub", text: sub }) : null);
      }
      case "artist": return h("div", { class: "c c-artist", text: t.artist });
      case "album": return h("div", { class: "c c-album", text: t.album });
      case "time": return h("div", { class: "c c-time", text: t.duration ? clock(t.duration) : "" });
      case "format": return h("div", { class: "c c-format", text: R.formatLabel(t) + (t.channels > 2 ? ` ${t.channels === 6 ? "5.1" : t.channels + "ch"}` : "") });
      case "stick": {
        const d = it.d;
        if (d.action === "skip") {
          return h("div", { class: "c c-stick" }, t.exists ? h("span", { class: "chip bad", title: d.reason, text: t.codec ? "Skipped" : "Not read" })
            : h("button", { type: "button", class: "btn quiet small", text: "Locate…", onclick: () => locate(t) }));
        }
        const why = [d.reason, ...(d.notes || [])].join("; ");
        if (d.action === "copy") return h("div", { class: "c c-stick" }, h("span", { class: "chip", title: why, text: "Copy" }));
        return h("div", { class: "c c-stick" }, h("span", { class: `chip ${d.lossy ? "lossy" : "enc"}`, title: why, text: `MP3 ${prf.bitrateCap}` }));
      }
      case "gain": {
        const g = it.g;
        if (!g) return h("div", { class: "c c-gain" });
        if (g.unknown) return h("div", { class: "c c-gain", title: "Loudness is measured when the app prepares the file", text: "–" });
        const tip = `${g.exact ? "Exact gain, applied while encoding" : "Lossless 1.5 dB steps"}${g.limited ? `. Wanted ${db(g.want)}, stopped short of the peaks` : ""}`;
        return h("div", { class: `c c-gain${g.limited ? " lim" : ""}`, title: tip }, h("span", { class: g.limited ? "lim" : null, text: g.db === 0 ? "0 dB" : db(g.db) }));
      }
      case "name":
        return h("div", { class: "c c-name", title: it.notes && it.notes.length ? it.notes.join("; ") : null },
          it.name ? it.name : "", it.notes && it.notes.length ? h("span", { class: "note", text: " •" }) : null);
      default: return h("div", { class: "c" });
    }
  }

  function renderFoot() {
    const n = S.sel.size;
    const foot = $("list-foot");
    if (n > 1) {
      const dur = selectedInOrder().reduce((a, e) => a + (track(e.trackId).duration || 0), 0);
      foot.textContent = `${n} selected, ${clock(dur)}. Drag them anywhere in the list, or onto a playlist on the left to add them there.`;
    } else {
      foot.textContent = "Drag tracks to reorder them, or select and press Alt+↑ or Alt+↓. Removing a track never deletes its file.";
    }
  }

  // ---- rendering: stick panel ----------------------------------------------------------------
  const ST = {};
  function buildStick() {
    const root = $("stick");
    ST.head = h("section", { class: "st-sec" });
    ST.folders = h("section", { class: "st-sec", "aria-label": "Playlists on this stick" });
    ST.profile = h("section", { class: "st-sec" });
    ST.names = h("section", { class: "st-sec" });
    ST.device = h("section", { class: "st-sec" });
    ST.space = h("section", { class: "st-sec" });
    ST.sync = h("div", { class: "sync" });
    // File names: built once, so the template field keeps focus while typing.
    ST.tpl = h("input", { class: "input mono", id: "tpl", spellcheck: "false", autocomplete: "off", "aria-label": "File name template", "aria-describedby": "tpl-help" });
    ST.tpl.addEventListener("input", () => {
      const pr = currentProfile();
      if (!pr) return;
      pr.fileTemplate = ST.tpl.value;
      renderAfterProfile(true);
    });
    ST.presets = h("select", { class: "input", "aria-label": "Name presets" },
      h("option", { value: "", text: "Presets" }), PRESETS.map((p) => h("option", { value: p.tpl, text: p.label })));
    ST.presets.addEventListener("change", () => {
      if (!ST.presets.value) return;
      ST.tpl.value = ST.presets.value;
      ST.presets.value = "";
      ST.tpl.dispatchEvent(new Event("input"));
    });
    const tokens = h("div", { class: "tokens", "aria-label": "Insert a field" },
      ["{n}", "{artist}", "{title}", "{album}", "{track}", "{year}", "{filename}", "[{artist} - ]", "{title|filename}"].map((tok) =>
        h("button", { type: "button", text: tok, title: `Insert ${tok}`, onclick: () => insertToken(tok) })));
    ST.ascii = h("input", { type: "checkbox", id: "opt-ascii" });
    ST.ascii.addEventListener("change", () => { currentProfile().ascii = ST.ascii.checked; renderAfterProfile(); });
    ST.maxLen = h("select", { class: "input", id: "opt-maxlen", "aria-label": "Longest name" },
      [["0", "No length limit"], ["64", "At most 64 characters"], ["48", "At most 48 characters"], ["32", "At most 32 characters"]].map(([v, l]) => h("option", { value: v, text: l })));
    ST.maxLen.addEventListener("change", () => { currentProfile().maxLen = Number(ST.maxLen.value); renderAfterProfile(); });
    ST.tplMsg = h("p", { class: "st-note", id: "tpl-help" });
    ST.preview = h("div", { class: "preview" });
    ST.names.append(
      h("h3", null, "File names", h("span", { class: "aside", id: "names-profile" })),
      h("div", { class: "tpl-row" }, ST.tpl, ST.presets),
      tokens,
      h("div", { class: "name-opts" }, h("label", { class: "check" }, ST.ascii, h("span", { text: "Plain ASCII" })), ST.maxLen),
      ST.tplMsg,
      ST.preview,
    );
    root.replaceChildren(h("div", { class: "st-scroll" }, ST.head, ST.folders, ST.profile, ST.names, ST.device, ST.space), ST.sync);
  }
  const currentProfile = () => (stickSet() ? prof(stickSet().profileId) : null);
  function insertToken(tok) {
    const el = ST.tpl;
    const a = el.selectionStart ?? el.value.length;
    const b = el.selectionEnd ?? el.value.length;
    el.value = el.value.slice(0, a) + tok + el.value.slice(b);
    el.focus();
    el.setSelectionRange(a + tok.length, a + tok.length);
    el.dispatchEvent(new Event("input"));
  }
  function renderAfterProfile(fromTemplate) {
    renderList();
    renderStick(fromTemplate);
    renderSide();
    renderDisplay();
    changed();
  }

  function renderStick(fromTemplate) {
    const s = stickSet();
    if (!s) {
      for (const k of ["head", "folders", "profile", "device", "space"]) ST[k].replaceChildren();
      ST.head.append(h("p", { class: "st-note", text: "Make a stick to choose what goes on it." }), h("button", { type: "button", class: "btn", text: "New stick", onclick: newSet }));
      ST.names.hidden = true;
      fill(ST.sync, );
      return;
    }
    ST.names.hidden = false;
    const plan = planSet(s);
    renderStickHead(plan);
    renderFolders(plan);
    renderProfileSec(plan);
    renderNames(plan, fromTemplate);
    renderDevices(plan);
    renderSpace(plan);
    renderSyncBar(plan);
  }

  function renderStickHead(plan) {
    const s = plan.s;
    const st = staleness(plan);
    const sel = h("select", { class: "input", id: "set-select", "aria-label": "Stick" },
      S.sets.map((x) => h("option", { value: x.id, text: x.name, selected: x.id === s.id })),
      h("option", { value: "__new", text: "New stick…" }));
    sel.addEventListener("change", () => {
      if (sel.value === "__new") { newSet(); return; }
      S.setId = sel.value;
      const ns = stickSet();
      if (!ns.playlistIds.includes(S.current) && ns.playlistIds[0]) openPlaylist(ns.playlistIds[0]);
      renderAll();
    });
    let status;
    if (st.never) status = h("p", { class: "st-status", text: "Not synced yet." });
    else {
      const where = device(st.stickId)?.name || st.stickName || "a stick that isn't plugged in";
      status = st.current
        ? h("p", { class: "st-status ok", text: `Up to date. Synced ${ago(st.at)} to ${where}.` })
        : h("p", { class: "st-status out", text: `Out of date: ${st.parts.join(", ")} since the last sync ${ago(st.at)}.` });
    }
    const pill = st.never ? h("span", { class: "pill", text: "New" }) : st.current ? h("span", { class: "pill ok", text: "Up to date" }) : h("span", { class: "pill out", text: "Out of date" });
    fill(ST.head, h("div", { class: "st-head" }, sel, pill), status);
  }

  function renderFolders(plan) {
    const s = plan.s;
    const single = plan.single;
    const rows = s.playlistIds.map((id, i) => {
      const p = pl(id);
      if (!p) return null;
      const f = plan.folders.find((x) => x.p.id === id);
      const off = !f;
      const bytesF = f ? f.present.reduce((a, it) => a + it.bytes, 0) : 0;
      const name = f ? (f.folder ? `${f.folder}/` : "Top level of the stick") : p.name;
      const meta = off ? "Not written: this profile puts one playlist on a stick" : `${f.folder && f.folder.includes(p.name) ? "" : `${p.name}, `}${plural(f.present.length, "track")}, ${bytes(bytesF)}`;
      const mv = (d) => edit(`Move ${p.name} ${d < 0 ? "up" : "down"}`, () => {
        const a = s.playlistIds;
        [a[i], a[i + d]] = [a[i + d], a[i]];
      });
      return h("li", { class: off ? "off" : null },
        h("span", { class: "fn", text: String(i + 1) }),
        h("button", { type: "button", class: "fname btn quiet small", style: { "justify-content": "flex-start", padding: "0", height: "auto", color: "inherit" }, text: name, title: `Show ${p.name}`, onclick: () => openPlaylist(id) }),
        h("span", { class: "fbtns" },
          h("button", { type: "button", class: "btn quiet small icon", title: "Move up", "aria-label": `Move ${p.name} up`, disabled: i === 0, onclick: () => mv(-1) }, icon("up")),
          h("button", { type: "button", class: "btn quiet small icon", title: "Move down", "aria-label": `Move ${p.name} down`, disabled: i === s.playlistIds.length - 1, onclick: () => mv(1) }, icon("down")),
          h("button", { type: "button", class: "btn quiet small icon", title: "Take off this stick", "aria-label": `Take ${p.name} off this stick`, onclick: () => edit(`Take ${p.name} off ${s.name}`, () => { s.playlistIds.splice(i, 1); }) }, icon("close"))),
        h("span", { class: "fmeta", text: meta }));
    });
    const addable = S.playlists.filter((p) => !s.playlistIds.includes(p.id));
    const add = h("select", { class: "input", "aria-label": "Add a playlist to this stick" },
      h("option", { value: "", text: addable.length ? "Add a playlist…" : "Every playlist is on it" }),
      addable.map((p) => h("option", { value: p.id, text: p.name })));
    add.disabled = !addable.length;
    add.addEventListener("change", () => {
      const p = pl(add.value);
      if (p) edit(`Put ${p.name} on ${s.name}`, () => { s.playlistIds.push(p.id); });
    });
    fill(ST.folders, 
      h("h3", null, single ? "Playlist on this stick" : "Playlists on this stick", h("span", { class: "aside", text: single ? "tracks at the top level" : "one folder each, in this order" })),
      rows.length ? h("ol", { class: "folders" }, rows) : h("p", { class: "st-note", text: "Nothing on this stick yet." }),
      single && s.playlistIds.length > 1 ? h("p", { class: "st-note warn", text: `${plan.profile.name} writes one playlist per stick, so only the first one goes on.` }) : null,
      single && s.playlistIds.length ? null : add,
    );
  }

  function profileSummary(pr) {
    return [
      `MP3 up to ${pr.bitrateCap} kbps ${pr.mode === "cbr" ? "CBR" : "VBR"}`,
      pr.sampleRates.map((r) => `${r / 1000}`).join(" or ") + " kHz",
      pr.tags === "id3v23" ? "ID3v2.3" : "ID3v2.4",
      pr.cover === "none" ? "no cover art" : "small cover art",
      pr.layout === "single" ? "one playlist per stick" : "a folder per playlist",
    ].join(", ") + ".";
  }
  function renderProfileSec(plan) {
    const s = plan.s;
    const sel = h("select", { class: "input", id: "profile-select", "aria-label": "Player profile" },
      S.profiles.map((p) => h("option", { value: p.id, text: p.name, selected: p.id === s.profileId })));
    sel.addEventListener("change", () => edit(`Use ${prof(sel.value).name} for ${s.name}`, () => { s.profileId = sel.value; }));
    fill(ST.profile, 
      h("h3", { text: "Player profile" }),
      h("div", { class: "prof-row" }, sel, h("button", { type: "button", class: "btn", text: "Edit…", onclick: () => profileEditor(s.profileId) })),
      h("p", { class: "prof-sum", text: profileSummary(plan.profile) }),
    );
  }

  function renderNames(plan, fromTemplate) {
    const pr = plan.profile;
    if (!fromTemplate && document.activeElement !== ST.tpl) ST.tpl.value = pr.fileTemplate;
    ST.ascii.checked = pr.ascii;
    ST.maxLen.value = String(pr.maxLen || 0);
    $("names-profile").textContent = `in ${pr.name}`;
    const errs = plan.errors;
    ST.tpl.classList.toggle("bad", errs.length > 0);
    ST.tpl.setAttribute("aria-invalid", String(errs.length > 0));
    if (errs.length) {
      ST.tplMsg.className = "st-note err";
      ST.tplMsg.textContent = errs.join(" ");
    } else if (!plan.numbered) {
      ST.tplMsg.className = "st-note warn";
      ST.tplMsg.textContent = "No {n}: a player that sorts by name would play these in alphabetical order.";
    } else {
      ST.tplMsg.className = "st-note";
      ST.tplMsg.textContent = `Every stick with the ${pr.name} profile uses these names. The .mp3 is added for you.`;
    }
    const noted = plan.all.filter((it) => it.notes.length).length;
    const lis = [];
    for (const f of plan.folders) {
      if (f.folder) lis.push(h("li", { class: "fold", text: `${f.folder}/` }));
      const shown = f.present.filter((it, i) => i < 4 || it.notes.length);
      let last = -1;
      for (const it of shown) {
        const idx = f.present.indexOf(it);
        if (idx > last + 1) lis.push(h("li", { class: "file gap", text: `… ${idx - last - 1} more` }));
        lis.push(h("li", { class: f.folder ? "file" : null }, it.name, it.notes.length ? h("span", { class: "nt", text: it.notes.join("; ") }) : null));
        last = idx;
      }
      if (last < f.present.length - 1) lis.push(h("li", { class: `${f.folder ? "file " : ""}gap`, text: `… ${f.present.length - last - 1} more` }));
    }
    ST.preview.replaceChildren(
      h("div", { class: "pv-head", style: { padding: "0 10px 4px" } },
        h("span", { class: noted ? "st-note warn" : "st-note", text: noted ? `${plural(noted, "name")} with notes` : "No names needed changes" }),
        h("button", { type: "button", class: "btn quiet small", text: "Show all", onclick: () => allNames(plan.s.id) })),
      h("ol", null, lis),
    );
  }

  function renderDevices(plan) {
    const s = plan.s;
    const items = S.sticks.map((d) => {
      const on = d === chosen(s);
      const input = h("input", { type: "radio", name: "dev", value: d.id, checked: on, "aria-label": d.name });
      input.addEventListener("change", () => edit(`Sync ${s.name} to ${d.name}`, () => {
        s.stickId = d.id;
        s.stickModel = d.name;
        s.stickSize = d.sizeBytes;
      }));
      return h("label", { class: `dev${on ? " sel" : ""}` }, input,
        h("span", { class: "dn", text: d.name }), h("span", { class: "ds", text: bytes(d.sizeBytes) }),
        h("span", { class: "don", text: `${d.device ? `${d.device}. ` : ""}Now: ${d.volumes.length ? d.volumes.join("; ") : "blank"}` }));
    });
    const gone = s.stickId && !chosen(s);
    const other = gone && device(s.stickId);
    fill(ST.device,
      h("h3", null, "USB stick", h("button", { type: "button", class: "btn quiet small", text: "Look again", onclick: () => refreshSticks(false) })),
      gone ? h("p", { class: "st-note warn", text: other
        ? `${other.name} isn't the stick this one syncs to (${s.stickModel}, ${bytes(s.stickSize)}). Pick it to sync to it instead.`
        : "The stick this one syncs to isn't plugged in. Plug it in, or pick another." }) : null,
      items.length ? h("div", { class: "devices", role: "radiogroup", "aria-label": "USB stick" }, items) : h("p", { class: "st-note", text: "No USB stick found. Plug one in; it shows up here." }),
      S.stickError ? h("p", { class: "st-note err", text: `Looking for sticks failed: ${S.stickError}` }) : null,
    );
  }

  function renderSpace(plan) {
    const lay = plan.lay;
    const kids = [h("h3", { text: "Space" })];
    if (!lay) {
      kids.push(h("p", { class: "st-note", text: "Pick a USB stick to see whether this fits." }));
    } else {
      const segs = [];
      for (let f = 0; f < plan.folders.length; f++) {
        const sz = lay.runs.filter((r) => r.folder === f).reduce((a, r) => a + r.size, 0);
        segs.push(h("i", { style: { width: `${Math.max(0.3, (100 * sz) / lay.dataBytes)}%` }, title: `${plan.folders[f].folder || "Tracks"}: ${bytes(sz)}` }));
      }
      kids.push(h("div", { class: `cap${lay.fits ? "" : " over"}`, role: "img", "aria-label": `${bytes(lay.used)} of ${bytes(lay.dataBytes)} used` }, segs));
      const used = Math.max(1, lay.used);
      const strip = lay.runs.map((r) => {
        const it = plan.all.find((x) => x.entry.id === r.entry);
        return h("i", {
          class: `f${r.folder % 3}${it && it.d.action === "encode" ? " enc" : ""}`,
          style: { flex: `${r.size / used} 1 0` }, title: `${it ? it.name : ""}, ${bytes(r.size)}`,
          dataset: { entry: r.entry },
        });
      });
      kids.push(h("div", { class: "strip", role: "img", "aria-label": "Files on the stick, back to back in playlist order" }, strip));
      kids.push(h("p", { class: "strip-legend" }, h("span", null, h("i"), "Each file in one unbroken run, in playlist order"), h("span", null, h("i", { class: "enc" }), "Encoded")));
      kids.push(lay.fits
        ? h("p", { class: "fit ok" }, h("strong", { text: "Fits. " }), `${bytes(lay.used)} of ${bytes(lay.dataBytes)}, ${bytes(lay.free)} left over.`)
        : h("p", { class: "fit bad" }, h("strong", { text: "Doesn't fit. " }), `${bytes(lay.used)} needed, the stick holds ${bytes(lay.dataBytes)}. Take a playlist off or pick a bigger stick.`));
    }
    const enc = plan.all.filter((it) => it.d.action === "encode");
    const lossy = enc.filter((it) => it.d.lossy).length;
    const parts = [`${plural(plan.all.length - enc.length, "track")} copied as they are`];
    if (enc.length) parts.push(`${enc.length} encoded at ${plan.profile.bitrateCap} kbps${lossy ? ` (${lossy} lossy to lossy)` : ""}`);
    if (plan.skipped.length) parts.push(`${plan.skipped.length} skipped (missing or unreadable)`);
    kids.push(h("p", { class: "counts", text: parts.join(", ") + "." }));
    ST.prep = h("p", { class: "prep", "aria-live": "polite" });
    kids.push(ST.prep);
    ST.space.replaceChildren(...kids);
    renderPrep(plan);
  }

  function renderPrep(plan) {
    if (!ST.prep) return;
    plan = plan || planSet(stickSet());
    if (!plan) return;
    const total = plan.keys.length;
    const left = plan.pending.length - plan.failed.length;
    if (S.ffmpegError) { fill(ST.prep, h("span", { class: "st-note err", text: "Nothing can be prepared: ffmpeg wasn't found." })); return; }
    if (plan.failed.length) {
      const e = S.prepFailed.get(plan.failed[0]);
      fill(ST.prep, h("span", { class: "st-note err", text: `${plural(plan.failed.length, "file")} couldn't be prepared: ${e}` }));
      return;
    }
    if (plan.waiting.length) {
      fill(ST.prep, h("span", { class: "spin", "aria-hidden": "true" }), `Measuring loudness: ${plural(plan.waiting.length, "track")} to go.`);
      return;
    }
    if (!total) { fill(ST.prep, "Nothing to prepare: every track is copied as it is."); return; }
    if (left <= 0) { fill(ST.prep, `Prepared: all ${plural(total, "file")} that need encoding or gain are in the cache.`); return; }
    fill(ST.prep, h("span", { class: "spin", "aria-hidden": "true" }), `Preparing in the background: ${total - left} of ${total} ready.`);
  }

  function renderSyncBar(plan) {
    if (S.sync && S.sync.setId === plan.s.id) return renderSyncProgress();
    const lay = plan.lay;
    const reasons = [];
    if (S.ffmpegError) reasons.push("ffmpeg wasn't found, so nothing can be prepared.");
    if (!plan.dev) reasons.push(S.sticks.length ? "Pick a USB stick." : "Plug in a USB stick.");
    else if (!lay.fits) reasons.push("It doesn't fit on this stick.");
    if (!plan.all.length) reasons.push("There's nothing to write yet.");
    if (plan.errors.length) reasons.push("Fix the file name template first.");
    if (S.sync && S.sync.running) reasons.push(`${stickSet(S.sync.setId)?.name || "Another stick"} is syncing.`);
    const btn = h("button", { type: "button", class: "btn primary", id: "sync-btn", text: "Erase and sync…", disabled: reasons.length > 0, onclick: confirmSync });
    fill(ST.sync,
      plan.dev ? h("p", { class: "erase" }, h("strong", { text: "Syncing erases the stick. " }), `Everything on ${plan.dev.name} is replaced with ${plan.folders.length === 1 ? "this playlist" : `these ${plan.folders.length} playlists`}.`) : null,
      btn,
      reasons.length ? h("p", { class: "why-not", text: reasons[0] }) : null,
      DEMO ? h("p", { class: "sync-note", text: "Demo: the sync is simulated, nothing is written." }) : null,
    );
  }

  // ---- rendering: player and display --------------------------------------------------------
  function renderPlayer() {
    const pp = S.player;
    const p = pp.entryId ? pl(pp.playlistId) : null;
    const e = p ? p.entries.find((x) => x.id === pp.entryId) : null;
    const t = e ? track(e.trackId) : null;
    const idx = p && e ? p.entries.indexOf(e) : -1;
    const playBtn = h("button", { type: "button", class: "pbtn play", "aria-label": pp.playing ? "Pause" : "Play", title: pp.playing ? "Pause (Space)" : "Play (Space)", onclick: togglePlay }, icon(pp.playing ? "pause" : "play"));
    const seek = h("input", { type: "range", min: "0", max: String(Math.max(1, Math.round(pp.dur || 1))), step: "1", value: String(Math.round(pp.pos)), "aria-label": "Position", disabled: !t, id: "seek" });
    seek.addEventListener("input", () => seekTo(Number(seek.value)));
    const aos = h("input", { type: "checkbox", checked: pp.asOnStick, id: "aos" });
    aos.addEventListener("change", () => {
      pp.asOnStick = aos.checked;
      if (pp.entryId) playEntry(pp.entryId, pp.playlistId, pp.pos);
      else renderPlayer();
    });
    const vol = h("input", { type: "range", min: "0", max: "1", step: "0.01", value: String(pp.volume), "aria-label": "Volume", id: "vol" });
    vol.addEventListener("input", () => { pp.volume = Number(vol.value); api.volume(pp.volume).catch(() => {}); saveSetting("volume", String(pp.volume)); });
    let sub = "Double-click a track, or press Enter, to hear it.";
    if (t) sub = `${t.artist || "No artist tag"}. ${p.name}, ${idx + 1} of ${p.entries.length}.${pp.silent ? " Demo track, no sound." : ""}`;
    fill($("player"), 
      h("div", { class: "pad" },
        h("button", { type: "button", class: "pbtn", "aria-label": "Previous", title: "Previous", disabled: !t, onclick: prevTrack }, icon("prev")),
        playBtn,
        h("button", { type: "button", class: "pbtn", "aria-label": "Next", title: "Next", disabled: !t, onclick: () => nextTrack(false) }, icon("next"))),
      h("div", { class: "np" }, h("div", { class: "np-t", text: t ? t.title || t.filename : "Nothing playing" }), h("div", { class: "np-s", text: sub })),
      h("div", { class: "seek" }, h("span", { id: "t-pos", text: t ? clock(pp.pos) : "0:00" }), seek, h("span", { id: "t-dur", text: t ? clock(pp.dur) : "0:00" })),
      h("label", { class: "aos" }, aos, h("span", { text: "As on the stick" }), h("small", { text: aosCaption(e) })),
      h("div", { class: "vol" }, icon("volume"), vol),
    );
  }

  function aosCaption(e) {
    if (!e) return "Hear the gain and bitrate the sync will use.";
    const it = stickItem(S.player.playlistId, e.id);
    if (!it) return "Not on a stick yet, so nothing changes.";
    if (!it.recipe) return "";
    const r = it.recipe;
    if (r.kind === "copy") return it.waiting ? "Loudness not measured yet." : "Copied as it is, no change.";
    if (r.kind === "gain") return `${db(r.steps * R.STEP)} in lossless steps, copied as it is.`;
    const g = r.gainDb ? `, ${db(r.gainDb)}` : "";
    if (!S.player.asOnStick) return `The stick gets a ${r.cap} kbps encode${g}.`;
    return S.player.from === "cache" ? `Playing the cached ${r.cap} kbps encode${g}.` : `The ${r.cap} kbps encode isn't ready, so the source plays${g}.`;
  }

  function tickPlayerUI() {
    const pp = S.player;
    const pos = $("t-pos");
    if (pos) pos.textContent = clock(pp.pos);
    const sk = $("seek");
    if (sk && document.activeElement !== sk) {
      sk.max = String(Math.max(1, Math.round(pp.dur || 1)));
      sk.value = String(Math.round(pp.pos));
    }
    const dur = $("t-dur");
    if (dur) dur.textContent = clock(pp.dur);
    renderDisplay();
  }

  function segText(s, n) {
    return s.toUpperCase().normalize("NFD").replace(/[̀-ͯ]/g, "").replace(/[^A-Z0-9 \-:.]/g, " ").replace(/\s+/g, " ").trim().slice(0, n);
  }
  function renderDisplay() {
    const pp = S.player;
    let main;
    let count = "";
    let unit = "";
    let time = "";
    let sub;
    const s = stickSet();
    if (S.sync && S.sync.running) {
      main = S.sync.lcd;
      count = String(Math.floor(S.sync.pct));
      unit = "%";
      sub = S.sync.msg;
    } else if (S.scan) {
      main = "READING";
      count = S.scan.total ? String(Math.floor((100 * S.scan.done) / S.scan.total)) : "";
      unit = S.scan.total ? "%" : "";
      sub = S.scan.total ? `Reading files: ${S.scan.done} of ${S.scan.total}.` : "Reading files.";
    } else if (pp.entryId) {
      const p = pl(pp.playlistId);
      const i = p.entries.findIndex((e) => e.id === pp.entryId);
      const t = track(p.entries[i].trackId);
      const w = Math.max(2, String(p.entries.length).length);
      main = `${pp.playing ? "PLAY" : "PAUSE"} ${pad2(i + 1, w)}-${pad2(p.entries.length, w)}`;
      time = clock(pp.pos);
      sub = `${t.title || t.filename}${t.artist ? `, ${t.artist}` : ""}`;
    } else {
      const p = pl() || { name: "", entries: [] };
      const live = p.entries.map((e) => track(e.trackId)).filter((t) => t.exists && t.duration);
      main = p.name;
      count = String(p.entries.length);
      unit = "TRK";
      time = clock(live.reduce((a, t) => a + t.duration, 0));
      if (s) {
        const st = staleness(planSet(s));
        sub = st.never ? `${s.name} hasn't been synced yet.` : st.current ? `${s.name} is up to date.` : `${s.name} is out of date: ${st.parts.join(", ")}.`;
      } else sub = "Put this playlist on a stick to sync it.";
    }
    const m = segText(main, 16);
    $("disp-text").textContent = m.padEnd(16, " ");
    $("disp-count").textContent = segText(count, 3).padStart(3, " ");
    $("disp-unit").textContent = unit || " ";
    const tt = time.padStart(7, " ");
    $("disp-time").textContent = tt;
    document.querySelector(".disp-time .ghost").textContent = tt.replace(/[0-9 ]/g, "~");
    $("disp-sub").textContent = sub || "";
  }

  function renderAll() {
    renderSide();
    renderHead();
    renderList();
    renderStick();
    renderPlayer();
    renderDisplay();
  }

  // ---- dialogs ----------------------------------------------------------------------------------
  function allNames(setId) {
    const plan = planSet(stickSet(setId));
    const lis = [];
    for (const f of plan.folders) {
      if (f.folder) lis.push(h("li", { class: "fold" }, h("span", { class: "nm", text: `${f.folder}/` }), h("span", { class: "sz", text: plural(f.present.length, "file") }), h("span")));
      for (const it of f.present) {
        lis.push(h("li", { class: "file" },
          h("span", { class: "nm", text: it.name }),
          h("span", { class: "sz", text: it.d.action === "copy" ? "copy" : `MP3 ${plan.profile.bitrateCap}` }),
          h("span", { class: "sz", text: bytes(it.bytes) }),
          it.notes.length ? h("span", { class: "nt", text: it.notes.join("; ") }) : null));
      }
    }
    if (plan.skipped.length) {
      lis.push(h("li", { class: "fold" }, h("span", { class: "nm", text: "Skipped" }), h("span"), h("span")));
      for (const it of plan.skipped) lis.push(h("li", { class: "file" }, h("span", { class: "nm", text: `${it.t.title || it.t.filename}: ${it.d.reason}` }), h("span"), h("span")));
    }
    modal({
      title: `On ${plan.s.name}, in FAT order`,
      wide: true,
      body: [h("p", { text: `This is the order a player reads the stick in: folders, then files, exactly as listed. Volume label ${N.volumeLabel(plan.s.label || plan.s.name) || "none"}.` }), h("ul", { class: "tree" }, lis)],
      buttons: [h("button", { type: "button", class: "btn", text: "Close", onclick: closeModal })],
    });
  }

  async function locate(t) {
    if (DEMO) {
      modal({
        title: `Where is ${t.title || t.filename}?`,
        body: [h("p", { text: `The playlist points at ${t.path}, and there's no file there now. Point the entry at the file's new place; the file itself isn't changed.` })],
        buttons: [
          h("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal }),
          h("button", { type: "button", class: "btn primary", text: "Use the demo copy", onclick: async () => { await api.locate(t.id); closeModal(); renderAll(); scheduleWork(); toast(`Located ${t.title} in ~/Music/Old library.`); } }),
        ],
      });
      return;
    }
    const path = await api.pickFile(`Where is ${t.title || t.filename}?`);
    if (!path) return;
    let r;
    try {
      r = await api.locate(t.id, path);
    } catch (e) {
      toast(`Couldn't use that file: ${e}`);
      return;
    }
    S.tracks.set(r.track.id, r.track);
    if (r.track.id !== t.id) {
      edit(`Locate ${r.track.title || r.track.filename}`, () => {
        for (const p of S.playlists) for (const e of p.entries) if (e.trackId === t.id) e.trackId = r.track.id;
      });
    } else {
      renderAll();
      scheduleWork();
    }
    toast(`Located ${r.track.title || r.track.filename}.`);
    if (r.matches.length) {
      modal({
        title: `Found ${plural(r.matches.length, "more missing track")} there`,
        body: [h("p", { text: `${plural(r.matches.length, "other missing track")} ${r.matches.length === 1 ? "has its" : "have their"} file in the same folder. Point ${r.matches.length === 1 ? "it" : "them"} there too?` })],
        buttons: [
          h("button", { type: "button", class: "btn", text: "Leave them", onclick: closeModal }),
          h("button", {
            type: "button", class: "btn primary", text: "Fix them",
            onclick: async () => {
              closeModal();
              for (const [id, p] of r.matches) {
                try {
                  const x = await api.locate(id, p);
                  S.tracks.set(x.track.id, x.track);
                } catch { /* left missing */ }
              }
              renderAll();
              scheduleWork();
              toast(`Located ${plural(r.matches.length, "more track")}.`);
            },
          }),
        ],
      });
    }
  }

  function profileEditor(id) {
    const pr = prof(id);
    const usedBy = S.sets.filter((s) => s.profileId === id).map((s) => s.name);
    const changed = () => renderAfterProfile();
    const radio = (name, value, label, key, conv = (v) => v) => {
      const r = h("input", { type: "radio", name, value, checked: String(pr[key]) === String(value) });
      r.addEventListener("change", () => { pr[key] = conv(value); changed(); });
      return h("label", { class: "check" }, r, h("span", { text: label }));
    };
    const check = (label, key) => {
      const c = h("input", { type: "checkbox", checked: !!pr[key] });
      c.addEventListener("change", () => { pr[key] = c.checked; changed(); });
      return h("label", { class: "check" }, c, h("span", { text: label }));
    };
    const rate = (hz) => {
      const c = h("input", { type: "checkbox", checked: pr.sampleRates.includes(hz) });
      c.addEventListener("change", () => {
        pr.sampleRates = c.checked ? [...new Set([...pr.sampleRates, hz])].sort((a, b) => a - b) : pr.sampleRates.filter((x) => x !== hz);
        if (!pr.sampleRates.length) { pr.sampleRates = [44100]; c.checked = hz === 44100; }
        changed();
      });
      return h("label", { class: "check" }, c, h("span", { text: `${hz / 1000} kHz` }));
    };
    const name = h("input", { class: "input", value: pr.name, "aria-label": "Profile name", "data-autofocus": "" });
    name.addEventListener("input", () => { pr.name = name.value || "Untitled profile"; changed(); });
    const folderTpl = h("input", { class: "input mono", value: pr.folderTemplate, "aria-label": "Folder names" });
    folderTpl.addEventListener("input", () => { pr.folderTemplate = folderTpl.value || "{n} {playlist}"; changed(); });
    const body = [
      h("p", { text: `Cheap players can't be identified reliably, so a profile describes what yours can play. Used by ${usedBy.length ? usedBy.join(" and ") : "no stick yet"}. Changes apply as you make them.` }),
      h("div", { class: "form" },
        h("span", { class: "lbl", text: "Name" }), name,
        h("span", { class: "lbl", text: "Bitrate cap" }), h("div", { class: "opts" },
          radio("cap", 320, "320 kbps", "bitrateCap", Number),
          radio("cap", 256, "256 kbps, for players that stumble on 320", "bitrateCap", Number),
          h("span", { class: "hint", text: "MP3s at or under the cap are copied bit for bit. Everything else is encoded with LAME at the cap." })),
        h("span", { class: "lbl", text: "Encoding" }), h("div", { class: "opts" },
          radio("mode", "cbr", "Constant bitrate (sizes known in advance)", "mode"),
          radio("mode", "vbr", "Variable bitrate (smaller; frames over the cap are encoded again as CBR)", "mode")),
        h("span", { class: "lbl", text: "Sample rates" }), h("div", { class: "opts row-opts" }, rate(32000), rate(44100), rate(48000)),
        h("span", { class: "lbl", text: "Tags" }), h("div", { class: "opts" },
          radio("tags", "id3v23", "ID3v2.3 (older players read it best)", "tags"),
          radio("tags", "id3v24", "ID3v2.4", "tags"),
          check("Also write an ID3v1.1 tag at the end", "id3v1"),
          check("Track number tag is the position in the playlist", "trackTagIsPosition"),
          check("Put the number in front of the title tag", "numberInTitle")),
        h("span", { class: "lbl", text: "Cover art" }), h("div", { class: "opts" },
          radio("cover", "small", "One small JPEG, up to 500 × 500 px and 100 KB", "cover"),
          radio("cover", "none", "None", "cover")),
        h("span", { class: "lbl", text: "Layout" }), h("div", { class: "opts" },
          radio("layout", "folders", "A folder per playlist, in stick order", "layout"),
          radio("layout", "single", "One playlist per stick, tracks at the top level", "layout")),
        h("span", { class: "lbl", text: "Folder names" }), h("div", { class: "opts" }, folderTpl, h("span", { class: "hint", text: "Used when a stick holds several playlists. {n} is the folder's position, {playlist} its name." })),
      ),
    ];
    const dup = () => {
      const copy = { ...clone(pr), id: newId("prof-"), name: `${pr.name} copy`, builtIn: false };
      S.profiles.push(copy);
      const s = stickSet();
      if (s && s.profileId === pr.id) edit(`Use ${copy.name} for ${s.name}`, () => { s.profileId = copy.id; });
      closeModal();
      profileEditor(copy.id);
    };
    const reset = () => {
      Object.assign(pr, clone(GENERIC));
      closeModal();
      changed();
      profileEditor(pr.id);
    };
    modal({
      title: `Player profile: ${pr.name}`,
      wide: true,
      body,
      buttons: [
        pr.builtIn ? h("button", { type: "button", class: "btn left", text: "Reset to defaults", onclick: reset }) : null,
        h("button", { type: "button", class: `btn${pr.builtIn ? "" : " left"}`, text: "Duplicate", onclick: dup }),
        h("button", { type: "button", class: "btn primary", text: "Done", onclick: closeModal }),
      ],
    });
  }

  // ---- background work: loudness and preparation ------------------------------------------
  // Loudness is measured for playlists that even out the volume; the files the
  // sticks will need are prepared in the cache, current stick first, so
  // pressing Sync is mostly writing.
  let workTimer = null;
  let lastQueue = null;
  function scheduleWork() {
    clearTimeout(workTimer);
    workTimer = setTimeout(() => { requestAnalysis(); queuePrep(); }, 300);
  }
  function requestAnalysis() {
    const want = new Set();
    const pls = new Set([S.current, ...S.sets.flatMap((s) => s.playlistIds)]);
    for (const id of pls) {
      const p = pl(id);
      if (!p || !p.norm.on) continue;
      for (const e of p.entries) {
        const t = track(e.trackId);
        if (t.exists && t.codec && t.lufs == null && !t.analysisError && !S.analyzing.has(t.id)) want.add(t.id);
      }
    }
    if (!want.size) return;
    for (const id of want) S.analyzing.add(id);
    api.analyze([...want]).catch(() => {});
  }
  async function queuePrep() {
    if (S.ffmpegError) return;
    const jobs = [];
    const seen = new Set();
    for (const s of [stickSet(), ...S.sets.filter((x) => x !== stickSet())].filter(Boolean)) {
      for (const it of planSet(s).all) {
        if (!it.key || seen.has(it.key) || S.cache.has(it.key) || S.prepFailed.has(it.key)) continue;
        seen.add(it.key);
        jobs.push({ key: it.key, trackId: it.t.id, recipe: it.recipe });
      }
    }
    const sig = jobs.map((j) => j.key).join("\n");
    if (sig === lastQueue) return;
    lastQueue = sig;
    try {
      const ready = await api.prepareQueue(jobs);
      for (const k of ready) S.cache.add(k);
      if (ready.length) renderPrep();
    } catch (e) {
      console.warn("prepare queue", e);
    }
  }
  let renderTimer = null;
  function scheduleRender() {
    clearTimeout(renderTimer);
    renderTimer = setTimeout(() => {
      renderHead();
      renderList();
      renderStick();
      renderSide();
    }, 150);
  }
  function onPrepared(p) {
    if (p.ok) { S.cache.add(p.key); S.prepFailed.delete(p.key); } else S.prepFailed.set(p.key, p.error || "failed");
    renderPrep();
  }
  function onAnalyzed(a) {
    S.analyzing.delete(a.trackId);
    const t = S.tracks.get(a.trackId);
    if (!t) return;
    if (a.error) t.analysisError = a.error;
    else Object.assign(t, { lufs: a.lufs, truePeak: a.truePeak });
    scheduleRender();
    scheduleWork();
  }

  // ---- sync ---------------------------------------------------------------------------------------
  const STEPS = ["Prepare the files", "Check the space", "Erase the stick", "Write the files", "Read back and compare", "Eject"];
  const STAGE_STEP = { measure: 0, prepare: 0, tags: 0, check: 1, authorize: 2, erase: 2, write: 3, verify: 4, eject: 5 };
  function confirmSync() {
    const plan = planSet(stickSet());
    const dev = plan.dev;
    const enc = plan.all.filter((it) => it.d.action === "encode").length;
    const kv = [
      ["Stick", `${dev.name}, ${bytes(dev.sizeBytes)}${dev.device ? ` (${dev.device})` : ""}`],
      ["Now on it", dev.volumes.length ? dev.volumes.join("; ") : "Nothing (blank)"],
      ["Will hold", `${plan.folders.map((f) => f.folder || f.p.name).join(", ")}: ${plural(plan.all.length, "track")}, about ${bytes(plan.lay.used)}`],
      ["Encoded", enc ? `${plural(enc, "track")} at ${plan.profile.bitrateCap} kbps` : "None, every track is copied"],
      ["Skipped", plan.skipped.length ? plan.skipped.map((it) => `${it.t.title || it.t.filename} (${it.d.reason.toLowerCase()})`).join("; ") : "None"],
      ["Format", `FAT32, ${bytes(plan.lay.cluster)} clusters, label ${N.volumeLabel(plan.s.label || plan.s.name) || "none"}`],
    ];
    const prompt = DEMO
      ? "This is the demo: nothing is asked and nothing is written."
      : dev.image
        ? "This is a disk image, so no password is needed."
        : "Your system asks for your password or fingerprint before the stick is erased.";
    modal({
      title: `Erase ${dev.name} and sync ${plan.s.name}?`,
      body: [
        h("div", { class: "danger-box" }, h("strong", { text: "Everything on the stick is erased. " }), "Copy off anything you want to keep first. The stick then holds only these playlists, written in FAT order."),
        h("dl", { class: "kv" }, kv.flatMap(([k, v]) => [h("dt", { text: k }), h("dd", { text: v })])),
        h("p", { class: "st-note", text: prompt }),
      ],
      buttons: [
        h("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal, "data-autofocus": "" }),
        h("button", { type: "button", class: "btn primary", text: "Erase and sync", onclick: () => { closeModal(); runSync(); } }),
      ],
    });
  }

  function renderSyncProgress() {
    const y = S.sync;
    if (!y) return;
    if (y.done) {
      fill(ST.sync,
        h("p", { class: `sync-done${y.ok ? "" : " bad"}` }, h("strong", { text: y.ok ? "Synced. " : "Stopped. " }), y.msg),
        (y.notes || []).map((n) => h("p", { class: "sync-note", text: n })),
        h("button", { type: "button", class: "btn", text: "Done", onclick: () => { S.sync = null; renderAll(); } }),
      );
      return;
    }
    fill(ST.sync,
      h("ol", { class: "steps" }, STEPS.map((label, i) => h("li", { class: i < y.step ? "done" : i === y.step ? "now" : null, text: i === 3 && y.files ? `Write ${plural(y.files, "file")}` : label }))),
      h("div", { class: "bar", role: "progressbar", "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": String(Math.round(y.pct)) }, h("i", { style: { width: `${y.pct}%` } })),
      h("p", { class: "sync-msg", text: y.msg }),
      h("button", { type: "button", class: "btn", text: y.cancel ? "Cancelling…" : "Cancel", disabled: y.step >= 4 || y.cancel, onclick: cancelSync }),
    );
  }
  function syncUpdate(fields) {
    Object.assign(S.sync, fields);
    if (S.sync.setId === S.setId) renderSyncProgress();
    renderDisplay();
  }
  function cancelSync() {
    if (!S.sync || !S.sync.running) return;
    S.sync.cancel = true;
    api.cancelSync().catch(() => {});
    renderSyncProgress();
  }

  function buildSyncPlan(plan) {
    const pr = plan.profile;
    return {
      setId: plan.s.id,
      setName: plan.s.name,
      stickId: plan.dev.id,
      stickSize: plan.dev.sizeBytes,
      label: plan.s.label || plan.s.name,
      profile: {
        bitrateCap: pr.bitrateCap, mode: pr.mode, sampleRates: pr.sampleRates, tags: pr.tags, id3v1: !!pr.id3v1,
        cover: pr.cover, trackTagIsPosition: pr.trackTagIsPosition !== false, numberInTitle: !!pr.numberInTitle,
      },
      folders: plan.folders.map((f) => ({
        name: f.folder || "",
        files: f.present.map((it, i) => ({ name: it.name, trackId: it.t.id, recipe: it.recipe, position: i + 1, total: f.present.length, key: it.key })),
      })),
      verify: true,
      estimateBytes: plan.lay ? plan.lay.used : 0,
      record: { content: plan.folders.map((f) => f.present.map((it) => it.t.id)), recipe: recipeOf(plan), playlists: plan.folders.map((f) => f.p.id) },
    };
  }

  function onSyncProgress(p) {
    if (!S.sync || !S.sync.running) return;
    const step = STAGE_STEP[p.stage] ?? S.sync.step;
    const pct = p.total ? Math.min(100, (100 * p.done) / p.total) : 0;
    const lcd = {
      prepare: `PREP ${pad2(p.done)}-${pad2(p.total)}`, tags: "TAGS", check: "CHECK", authorize: "AUTHORIZE", erase: "ERASE",
      write: "WRITE", verify: "VERIFY", eject: "EJECT",
    }[p.stage] || S.sync.lcd;
    let msg = p.message || S.sync.msg;
    if ((p.stage === "write" || p.stage === "verify") && p.total) msg = `${msg}. ${bytes(p.done)} of ${bytes(p.total)}.`;
    syncUpdate({ step, pct, lcd, msg, stage: p.stage });
  }

  async function runSync() {
    const s = stickSet();
    let plan = planSet(s);
    S.sync = { setId: s.id, running: true, step: 0, pct: 0, msg: "Getting ready.", lcd: "PREPARE", files: plan.all.length, cancel: false };
    renderAll();
    // Loudness first, where a playlist evens out the volume: the gains depend on it.
    if (plan.waiting.length) {
      requestAnalysis();
      while ((plan = planSet(s)).waiting.length) {
        if (S.sync.cancel) return finishSync({ ok: false, cancelled: true, error: "Cancelled. Nothing was written; the stick is as it was." });
        syncUpdate({ msg: `Measuring loudness: ${plural(plan.waiting.length, "track")} to go.`, lcd: "MEASURE" });
        await sleep(250);
      }
    }
    if (S.sync.cancel) return finishSync({ ok: false, cancelled: true, error: "Cancelled. Nothing was written; the stick is as it was." });
    if (!plan.dev) return finishSync({ ok: false, error: "The stick isn't plugged in any more. Nothing was written." });
    let res;
    try {
      res = await api.sync(buildSyncPlan(plan));
    } catch (e) {
      res = { ok: false, error: String(e) };
    }
    finishSync(res);
  }

  function finishSync(res) {
    const s = stickSet(S.sync.setId);
    if (res.ok && res.record) S.syncs[s.id] = res.record;
    let msg;
    if (res.ok) {
      msg = `${plural(res.files, "file")} written and checked byte for byte`;
      msg += res.ejected ? ", and the stick was ejected. You can unplug it." : ".";
    } else msg = res.error || "The sync stopped.";
    S.sync = { setId: s.id, done: true, ok: !!res.ok, cancelled: !!res.cancelled, msg, notes: res.notes || [] };
    if (res.ok) toast(`Synced ${s.name}.`);
    refreshSticks(true);
    renderAll();
  }

  // ---- player -----------------------------------------------------------------------------------
  // The app's own player decodes with the bundled ffmpeg; this side sends
  // commands and follows its events. "As on the stick" plays the prepared
  // file when it's in the cache, or the source with the planned gain.
  function stickItem(plId, entryId) {
    const p = pl(plId);
    const s = p && setsFor(p.id)[0];
    if (!s) return null;
    return planPlaylist(p, prof(s.profileId)).items.find((x) => x.entry.id === entryId) || null;
  }
  function stopPlayback() {
    api.stop().catch(() => {});
    Object.assign(S.player, { entryId: null, playlistId: null, playing: false, pos: 0, dur: 0, silent: false, error: null });
    renderPlayer();
    markPlaying();
    renderDisplay();
  }
  async function playEntry(entryId, plId = S.current, from = 0) {
    const p = pl(plId);
    const e = p && p.entries.find((x) => x.id === entryId);
    if (!e) return;
    const t = track(e.trackId);
    if (!t.exists) { toast(`${t.title || t.filename} can't play: the file isn't where the playlist says. Use Locate to find it.`); return; }
    const args = { trackId: t.id, token: entryId, from, gainDb: 0, recipe: null };
    if (S.player.asOnStick) {
      const it = stickItem(plId, entryId);
      if (it && it.recipe && it.recipe.kind !== "copy") {
        args.recipe = it.recipe;
        args.gainDb = it.recipe.kind === "gain" ? it.recipe.steps * R.STEP : it.recipe.gainDb || 0;
      }
    }
    Object.assign(S.player, { entryId, playlistId: plId, playing: true, pos: from, dur: t.duration, error: null, silent: DEMO && !t.url, from: "source" });
    renderPlayer();
    markPlaying();
    renderDisplay();
    try {
      const r = await api.play(args);
      if (S.player.entryId === entryId) { S.player.from = r.from; renderPlayer(); }
    } catch (err) {
      toast(String(err));
      stopPlayback();
    }
  }
  function onPlayer(ev) {
    const pp = S.player;
    if (!pp.entryId || ev.token !== pp.entryId) return;
    if (ev.state === "ended") { nextTrack(true); return; }
    if (ev.state === "error") { toast(ev.error || "Playback stopped."); stopPlayback(); return; }
    if (ev.state === "stopped") return;
    pp.pos = ev.pos;
    if (ev.dur) pp.dur = ev.dur;
    const was = pp.playing;
    pp.playing = ev.state === "playing";
    if (was !== pp.playing) { renderPlayer(); markPlaying(); }
    tickPlayerUI();
  }
  function togglePlay() {
    const pp = S.player;
    if (!pp.entryId) {
      const first = S.focus || pl().entries.find((e) => track(e.trackId).exists)?.id;
      if (first) playEntry(first);
      return;
    }
    pp.playing = !pp.playing;
    (pp.playing ? api.resume() : api.pause()).catch(() => {});
    renderPlayer();
    markPlaying();
    renderDisplay();
  }
  function nextTrack(auto) {
    const pp = S.player;
    const p = pl(pp.playlistId);
    if (!p) return stopPlayback();
    const i = p.entries.findIndex((e) => e.id === pp.entryId);
    for (let k = i + 1; k < p.entries.length; k++) {
      if (track(p.entries[k].trackId).exists) return playEntry(p.entries[k].id, p.id);
    }
    if (auto) stopPlayback();
  }
  function prevTrack() {
    const pp = S.player;
    if (pp.pos > 3) return seekTo(0);
    const p = pl(pp.playlistId);
    const i = p.entries.findIndex((e) => e.id === pp.entryId);
    for (let k = i - 1; k >= 0; k--) {
      if (track(p.entries[k].trackId).exists) return playEntry(p.entries[k].id, p.id);
    }
    seekTo(0);
  }
  function seekTo(sec) {
    S.player.pos = sec;
    api.seek(sec).catch(() => {});
    tickPlayerUI();
  }
  function markPlaying() {
    if (S.player.playlistId === S.current || !S.player.entryId) renderList();
  }

  // ---- adding files ------------------------------------------------------------------------------
  function sortDropped(ts) {
    return ts.sort((a, b) => (a.disc || 0) - (b.disc || 0) || (a.track || 0) - (b.track || 0) || collator.compare(a.path, b.path));
  }
  /** The demo in a browser: dropped File objects, read in the page. */
  async function addFileObjects(files, target) {
    const audioFiles = files.filter((f) => P.sniff.isAudio(f.name));
    const lists = files.filter((f) => P.sniff.isPlaylist(f.name));
    if (lists.length) toast("Playlist files open in the desktop app; a browser can't follow the paths inside them.");
    if (!audioFiles.length) { if (!lists.length) toast("Those aren't audio files this app reads."); return; }
    toast(`Reading ${plural(audioFiles.length, "file")}…`);
    const ts = await api.addFileObjects(audioFiles);
    for (const t of ts) S.tracks.set(t.id, t);
    sortDropped(ts);
    insertTracks(ts.map((t) => t.id), target);
  }
  function insertTracks(ids, target, name = "Dropped files") {
    if (!ids.length) return;
    if (target && target.kind === "playlist") {
      addTracks(ids, target.id);
      toast(`Added ${plural(ids.length, "track")} to ${pl(target.id).name}.`, { label: "Undo", run: undo });
    } else if (target && target.kind === "new") {
      newPlaylist(name, ids);
      toast(`Made a playlist from ${plural(ids.length, "file")}.`, { label: "Undo", run: undo });
    } else {
      addTracks(ids, S.current, target ? target.index : null);
      toast(`Added ${plural(ids.length, "track")} to ${pl().name}. The files stay where they are.`, { label: "Undo", run: undo });
    }
  }
  /** The app: files, folders and playlist files by path, read by the library. */
  async function addPaths(paths, target) {
    if (S.ffmpegError) { toast("Files can't be read: ffmpeg wasn't found. Reinstall Playlist2USB to get it back."); return; }
    S.scan = { done: 0, total: 0 };
    renderDisplay();
    let r;
    try {
      r = await api.addPaths(paths);
    } catch (e) {
      toast(`Couldn't add those: ${e}`);
      return;
    } finally {
      S.scan = null;
      renderDisplay();
    }
    for (const t of r.tracks) S.tracks.set(t.id, t);
    // A new playlist made from one folder takes the folder's name.
    const one = paths.length === 1 && paths[0].replace(/[\\/]+$/, "");
    const folderName = one && r.trackIds.some((id) => track(id).path !== one) ? one.split(/[\\/]/).pop() : null;
    if (r.trackIds.length) insertTracks(r.trackIds, target, folderName || undefined);
    for (const imp of r.playlists) {
      if (target && target.kind === "list" && !r.trackIds.length && r.playlists.length === 1) insertTracks(imp.trackIds, target);
      else {
        newPlaylist(imp.name, imp.trackIds);
        const missing = imp.trackIds.filter((id) => !track(id).exists).length;
        toast(`Imported ${imp.name}: ${plural(imp.trackIds.length, "track")}${missing ? `, ${missing} not found` : ""}. The playlist file isn't changed.`, { label: "Undo", run: undo });
      }
      if (imp.skipped.length) toast(`${imp.name}: ${plural(imp.skipped.length, "stream")} left out; only files can go on a stick.`);
    }
    if (r.failed.length) toast(`${plural(r.failed.length, "file")} couldn't be read: ${r.failed[0]}`);
    else if (!r.trackIds.length && !r.playlists.length) toast("No audio files there.");
    scheduleWork();
  }
  async function addFiles(folder) {
    if (DEMO) {
      const input = folder ? $("folder-in") : $("file-in");
      input.onchange = () => {
        const files = [...input.files];
        input.value = "";
        addFileObjects(files, { kind: "list", index: null });
      };
      input.click();
      return;
    }
    const paths = await api.pickFiles({ folder });
    if (paths.length) addPaths(paths, { kind: "list", index: null });
  }
  async function importPlaylist() {
    if (DEMO) { toast("Importing M3U, XSPF and PLS files works in the desktop app, where the paths inside them can be opened."); return; }
    const paths = await api.pickFiles({ playlists: true });
    if (paths.length) addPaths(paths, { kind: "new" });
  }
  async function exportPlaylist() {
    const p = pl();
    if (!p || !p.entries.length) { toast("This playlist is empty; there's nothing to export."); return; }
    if (DEMO) { toast("Exporting works in the desktop app."); return; }
    const path = await api.pickSavePath(`${p.name.replace(/[\\/:*?"<>|]/g, "_")}.m3u8`);
    if (!path) return;
    try {
      await api.exportPlaylist(p.entries.map((e) => e.trackId), p.name, path, true);
      toast(`Exported ${p.name} to ${path}.`);
    } catch (e) {
      toast(`Couldn't export: ${e}`);
    }
  }
  let stickPoll = null;
  async function refreshSticks(quiet) {
    try {
      const r = await api.listSticks();
      const changedSticks = JSON.stringify(r.sticks) !== JSON.stringify(S.sticks);
      S.sticks = r.sticks;
      S.stickError = r.error;
      if (changedSticks || !quiet) { renderStick(); renderSide(); }
      if (!quiet) toast(r.sticks.length ? `${plural(r.sticks.length, "stick")} found.` : "No USB stick found.");
    } catch (e) {
      if (!quiet) toast(`Couldn't look for sticks: ${e}`);
    }
  }

  // ---- pointer drag sorting -----------------------------------------------------------------------
  let drag = null;
  let pending = null;
  let lastTap = null;
  function rowsNow() {
    return [...$("list").children].filter((r) => r.dataset.id);
  }
  function dropIndexAt(y) {
    const rows = rowsNow();
    for (let i = 0; i < rows.length; i++) {
      const r = rows[i].getBoundingClientRect();
      if (y < r.top + r.height / 2) return i;
    }
    return rows.length;
  }
  function showLine(index) {
    const line = $("drop-line");
    const rows = rowsNow();
    const wrap = $("list-wrap").getBoundingClientRect();
    let y;
    if (!rows.length) y = $("list").getBoundingClientRect().top + 6;
    else if (index < rows.length) y = rows[index].getBoundingClientRect().top - 0.5;
    else y = rows[rows.length - 1].getBoundingClientRect().bottom + 0.5;
    line.style.top = `${y - wrap.top}px`;
    line.hidden = false;
  }
  function hideLine() { $("drop-line").hidden = true; }
  function targetAt(x, y) {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;
    const side = el.closest("[data-pl]");
    if (side) return { kind: "playlist", id: side.dataset.pl };
    if (el.closest("#side-pls") || el.closest(".side-h")) return { kind: "new" };
    if (el.closest("#list-wrap")) return { kind: "list", index: dropIndexAt(y) };
    return null;
  }
  function markSideTarget(id) {
    for (const b of document.querySelectorAll("[data-pl]")) b.classList.toggle("drop-target", b.dataset.pl === id);
  }

  function onListPointerDown(e) {
    if (e.button !== 0) return;
    const row = e.target.closest(".row");
    if (!row || e.target.closest("button, input, a, select")) return;
    // Touch drags start on the track number only, so the list still scrolls.
    const canDrag = e.pointerType !== "touch" || !!e.target.closest(".c-num");
    if (canDrag) e.preventDefault();
    $("list").focus({ preventScroll: true });
    const id = row.dataset.id;
    const mod = e.ctrlKey || e.metaKey;
    pending = null;
    if (e.shiftKey) selectRange(S.anchor || id, id, mod);
    else if (mod) {
      if (S.sel.has(id)) pending = { toggleOff: id };
      else { S.sel.add(id); S.anchor = id; }
      S.focus = id;
    } else if (!S.sel.has(id)) selectOnly(id);
    else { pending = { collapse: id }; S.focus = id; }
    syncSelection();
    if (!canDrag) { pending = null; return; }
    drag = { x: e.clientX, y: e.clientY, started: false, pointerId: e.pointerId, target: null, rowId: id };
    $("list").setPointerCapture(e.pointerId);
  }
  function onListPointerMove(e) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    if (!drag.started) {
      if (Math.hypot(e.clientX - drag.x, e.clientY - drag.y) < 5) return;
      drag.started = true;
      drag.ids = selectedInOrder().map((x) => x.id);
      pending = null;
      for (const r of rowsNow()) if (S.sel.has(r.dataset.id)) r.classList.add("dragging");
      const badge = $("drag-badge");
      const first = track(pl().entries.find((x) => x.id === drag.ids[0]).trackId);
      badge.textContent = drag.ids.length === 1 ? (first.title || first.filename) : `${drag.ids.length} tracks`;
      badge.hidden = false;
      autoScroll();
    }
    drag.px = e.clientX;
    drag.py = e.clientY;
    updateDrag();
  }
  function updateDrag() {
    const badge = $("drag-badge");
    badge.style.left = `${drag.px + 14}px`;
    badge.style.top = `${drag.py + 12}px`;
    const t = targetAt(drag.px, drag.py);
    drag.target = t && t.kind !== "new" && !(t.kind === "playlist" && t.id === S.current) ? t : null;
    if (drag.target && drag.target.kind === "list") showLine(drag.target.index);
    else hideLine();
    markSideTarget(drag.target && drag.target.kind === "playlist" ? drag.target.id : null);
    badge.classList.toggle("nowhere", !drag.target);
    if (drag.target && drag.target.kind === "playlist") badge.textContent = `Add ${drag.ids.length === 1 ? "1 track" : `${drag.ids.length} tracks`} to ${pl(drag.target.id).name}`;
    else badge.textContent = drag.ids.length === 1 ? (track(pl().entries.find((x) => x.id === drag.ids[0]).trackId).title || "1 track") : `${drag.ids.length} tracks`;
  }
  function autoScroll() {
    if (!drag || !drag.started) return;
    const list = $("list");
    const r = list.getBoundingClientRect();
    const edge = 36;
    let v = 0;
    if (drag.py < r.top + edge) v = -Math.ceil((r.top + edge - drag.py) / 3);
    else if (drag.py > r.bottom - edge) v = Math.ceil((drag.py - (r.bottom - edge)) / 3);
    if (v) { list.scrollTop += v; updateDrag(); }
    requestAnimationFrame(autoScroll);
  }
  function endDrag(commit) {
    const d = drag;
    drag = null;
    hideLine();
    markSideTarget(null);
    $("drag-badge").hidden = true;
    for (const r of rowsNow()) r.classList.remove("dragging");
    if (!d) return;
    if (!d.started) {
      if (pending && pending.toggleOff) { S.sel.delete(pending.toggleOff); syncSelection(); }
      if (pending && pending.collapse) { selectOnly(pending.collapse); syncSelection(); }
      pending = null;
      // Double-click to play. Detected here because pointerdown is cancelled
      // (no text selection while dragging), which also stops dblclick events.
      const now = performance.now();
      if (commit && lastTap && lastTap.id === d.rowId && now - lastTap.t < 450 && Math.hypot(lastTap.x - d.x, lastTap.y - d.y) < 6) { lastTap = null; playEntry(d.rowId); }
      else lastTap = { id: d.rowId, t: now, x: d.x, y: d.y };
      return;
    }
    if (!commit || !d.target) return;
    if (d.target.kind === "list") moveSelectionTo(d.target.index);
    else if (d.target.kind === "playlist") {
      const tids = d.ids.map((id) => pl().entries.find((x) => x.id === id).trackId);
      addTracks(tids, d.target.id);
      toast(`Added ${plural(tids.length, "track")} to ${pl(d.target.id).name}.`, { label: "Undo", run: undo });
    }
  }

  // ---- files dropped from the OS -------------------------------------------------------------------
  function fileHover(x, y) {
    const t = targetAt(x, y);
    $("list-wrap").classList.toggle("file-over", !!t && t.kind === "list");
    if (t && t.kind === "list") showLine(t.index);
    else hideLine();
    markSideTarget(t && t.kind === "playlist" ? t.id : null);
    return t;
  }
  function clearFileHover() {
    $("list-wrap").classList.remove("file-over");
    hideLine();
    markSideTarget(null);
  }
  const hasFiles = (e) => e.dataTransfer && [...e.dataTransfer.types].includes("Files");
  async function readEntry(entry, out) {
    if (entry.isFile) {
      await new Promise((res) => entry.file((f) => { Object.defineProperty(f, "webkitRelativePath", { value: entry.fullPath.slice(1) }); out.push(f); res(); }, res));
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      for (;;) {
        const batch = await new Promise((res) => reader.readEntries(res, () => res([])));
        if (!batch.length) break;
        for (const e of batch) await readEntry(e, out);
      }
    }
  }
  function wireFileDrops() {
    if (!DEMO) {
      api.onFileDrop(({ type, paths, pos }) => {
        if (type === "over" || type === "enter") { if (pos) fileHover(pos.x, pos.y); }
        else if (type === "drop") { const t = pos ? targetAt(pos.x, pos.y) : null; clearFileHover(); addPaths(paths, t || { kind: "list", index: null }); }
        else clearFileHover();
      });
      return;
    }
    document.addEventListener("dragover", (e) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
      fileHover(e.clientX, e.clientY);
    });
    document.addEventListener("dragleave", (e) => { if (!e.relatedTarget) clearFileHover(); });
    document.addEventListener("drop", async (e) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      const t = targetAt(e.clientX, e.clientY) || { kind: "list", index: null };
      clearFileHover();
      const items = [...e.dataTransfer.items].map((it) => it.webkitGetAsEntry && it.webkitGetAsEntry()).filter(Boolean);
      let files = [];
      if (items.length) for (const en of items) await readEntry(en, files);
      else files = [...e.dataTransfer.files];
      addFileObjects(files, t);
    });
  }

  // ---- keyboard --------------------------------------------------------------------------------------
  function onKey(e) {
    if (modalOpen() || !$("menu").hidden) return;
    if (drag && e.key === "Escape") { e.preventDefault(); endDrag(false); return; }
    if (e.target.closest("input, select, textarea")) return;
    const mod = e.ctrlKey || e.metaKey;
    const k = e.key.toLowerCase();
    if (mod && k === "z") { e.preventDefault(); if (e.shiftKey) redo(); else undo(); return; }
    if (mod && k === "y") { e.preventDefault(); redo(); return; }
    if (mod && k === "o") { e.preventDefault(); addFiles(false); return; }
    if (e.key === " " && !e.target.closest("button")) { e.preventDefault(); togglePlay(); return; }
    const inList = document.activeElement === $("list") || document.activeElement === document.body;
    if (!inList) return;
    const ids = entryIds();
    const i = ids.indexOf(S.focus);
    const go = (j) => {
      j = Math.max(0, Math.min(ids.length - 1, j));
      if (!ids.length) return;
      if (e.shiftKey) selectRange(S.anchor || ids[j], ids[j], false);
      else selectOnly(ids[j]);
      syncSelection();
      revealFocus();
    };
    if ((e.key === "ArrowUp" || e.key === "ArrowDown") && e.altKey) { e.preventDefault(); nudge(e.key === "ArrowUp" ? -1 : 1); return; }
    if (e.key === "ArrowDown") { e.preventDefault(); go(i < 0 ? 0 : i + 1); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); go(i < 0 ? 0 : i - 1); return; }
    if (e.key === "Home") { e.preventDefault(); go(0); return; }
    if (e.key === "End") { e.preventDefault(); go(ids.length - 1); return; }
    if (mod && k === "a") { e.preventDefault(); S.sel = new Set(ids); syncSelection(); return; }
    if (mod && k === "x") { e.preventDefault(); cutCopy(true); return; }
    if (mod && k === "c") { e.preventDefault(); cutCopy(false); return; }
    if (mod && k === "v") { e.preventDefault(); paste(); return; }
    if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); removeSelected(); return; }
    if (e.key === "Enter" && S.focus) { e.preventDefault(); playEntry(S.focus); return; }
    if (e.key === "Escape") { selectOnly(null); syncSelection(); }
  }

  function rowMenu(e) {
    const row = e.target.closest(".row");
    if (!row) return;
    e.preventDefault();
    const id = row.dataset.id;
    if (!S.sel.has(id)) { selectOnly(id); syncSelection(); }
    const n = S.sel.size;
    const t = track(pl().entries.find((x) => x.id === id).trackId);
    const others = S.playlists.filter((p) => p.id !== S.current);
    const tids = () => selectedInOrder().map((x) => x.trackId);
    menu({ x: e.clientX, y: e.clientY }, [
      { label: "Play", key: "Enter", action: () => playEntry(id), disabled: !t.exists },
      "-",
      { label: "Move to the top", action: () => moveSelectionTo(0) },
      { label: "Move to the bottom", action: () => moveSelectionTo(pl().entries.length) },
      ...(others.length ? [{ heading: n === 1 ? "Add to playlist" : `Add ${n} tracks to` }, ...others.map((p) => ({ label: p.name, action: () => { addTracks(tids(), p.id); toast(`Added ${plural(n, "track")} to ${p.name}.`, { label: "Undo", run: undo }); } }))] : []),
      "-",
      t.exists ? { label: "Show where the file is", action: () => toast(t.path) } : { label: "Locate…", action: () => locate(t) },
      { label: n === 1 ? "Remove from playlist" : `Remove ${n} from playlist`, key: "Delete", action: removeSelected },
    ]);
  }

  // ---- theme and the app menu -----------------------------------------------------------------------
  function applyTheme() {
    const root = document.documentElement;
    if (S.theme === "system") { if (hostTheme) root.setAttribute("data-theme", hostTheme); else root.removeAttribute("data-theme"); }
    else root.setAttribute("data-theme", S.theme);
  }
  function appMenu(anchor) {
    const theme = (v, label) => ({ label, checked: S.theme === v, action: () => { S.theme = v; applyTheme(); saveSetting("theme", v); } });
    menu(anchor, [
      { label: "Add files…", key: "Ctrl+O", action: () => addFiles(false) },
      { label: "Add a folder…", action: () => addFiles(true) },
      { label: "Import a playlist file…", action: importPlaylist },
      { label: "Export this playlist…", action: exportPlaylist, disabled: !pl() },
      "-",
      { heading: "Theme" },
      theme("system", "Same as the system"),
      theme("light", "Light"),
      theme("dark", "Dark"),
      "-",
      { label: "Clear the cache…", action: clearCacheDialog },
      { label: "About Playlist2USB", action: aboutDialog },
    ]);
  }
  async function clearCacheDialog() {
    const c = await api.cacheInfo();
    modal({
      title: "Clear the cache?",
      body: [h("p", { text: `The cache holds ${plural(c.files, "prepared file")} (${bytes(c.bytes)}): encodes and gain-adjusted copies. Clearing it is always safe; the next sync prepares them again, which takes longer.` })],
      buttons: [
        h("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal, "data-autofocus": "" }),
        h("button", {
          type: "button", class: "btn primary", text: "Clear the cache",
          onclick: async () => {
            closeModal();
            await api.clearCache();
            S.cache.clear();
            lastQueue = null;
            renderPrep();
            scheduleWork();
            toast("Cleared the cache.");
          },
        }),
      ],
    });
  }
  async function aboutDialog() {
    const a = await api.about();
    modal({
      title: "About Playlist2USB",
      body: [
        h("p", { text: "Writes playlists onto USB sticks in FAT order, the order cheap MP3 players and car stereos actually play." }),
        h("dl", { class: "kv" },
          h("dt", { text: "Version" }), h("dd", { text: a.version }),
          h("dt", { text: "Media engine" }), h("dd", { text: a.ffmpeg || S.ffmpegError || "not found" }),
          a.ffmpegPath ? [h("dt", { text: "ffmpeg" }), h("dd", { text: a.ffmpegPath })] : null,
          a.dataDir ? [h("dt", { text: "Library" }), h("dd", { text: a.dataDir })] : null,
          h("dt", { text: "Cache" }), h("dd", { text: `${plural(a.cache.files, "file")}, ${bytes(a.cache.bytes)}${a.cache.dir ? ` in ${a.cache.dir}` : ""}` })),
        h("p", { class: "st-note", text: "Free software under the GNU General Public License, version 3 or later. It includes FFmpeg (LGPL 2.1 or later) built with LAME (LGPL), and the fonts DSEG14 and Barlow (SIL Open Font License). The licence texts and the source code for every bundled part come with the app." }),
      ],
      buttons: [h("button", { type: "button", class: "btn primary", text: "Close", onclick: closeModal })],
    });
  }

  // ---- start -------------------------------------------------------------------------------------------
  async function init() {
    buildStick();
    $("app-menu").addEventListener("click", (e) => appMenu(e.currentTarget));
    const list = $("list");
    list.addEventListener("pointerdown", onListPointerDown);
    list.addEventListener("pointermove", onListPointerMove);
    list.addEventListener("pointerup", () => endDrag(true));
    list.addEventListener("pointercancel", () => endDrag(false));
    list.addEventListener("lostpointercapture", () => { if (drag) endDrag(true); });
    list.addEventListener("contextmenu", rowMenu);
    list.addEventListener("focus", () => { if (pl() && !S.focus && pl().entries[0]) { S.focus = pl().entries[0].id; syncSelection(); } });
    document.addEventListener("keydown", onKey);
    wireFileDrops();
    api.on("prepared", onPrepared);
    api.on("analyzed", onAnalyzed);
    api.on("player", onPlayer);
    api.on("sync-progress", onSyncProgress);
    api.on("scan-progress", (p) => { S.scan = p; renderDisplay(); });

    let d;
    try {
      d = await api.init();
    } catch (e) {
      $("disp-text").textContent = "ERROR".padEnd(16, " ");
      $("disp-sub").textContent = `The library couldn't be opened: ${e}`;
      return;
    }
    S.version = d.version;
    S.ffmpegError = d.ffmpegError;
    const st = d.settings || {};
    if (["system", "light", "dark"].includes(st.theme)) S.theme = st.theme;
    applyTheme();
    try { Object.assign(S.cols, JSON.parse(st.columns || "{}")); } catch { /* keep the defaults */ }
    if (st.volume) S.player.volume = Number(st.volume) || 0.8;
    for (const t of d.tracks) S.tracks.set(t.id, { ...t });
    S.playlists = clone(d.playlists);
    S.profiles = clone(d.profiles);
    S.sets = clone(d.sets);
    remember("playlist", S.playlists);
    remember("profile", S.profiles);
    remember("set", S.sets);
    // First run: the generic profile, one empty playlist and one stick.
    if (!S.profiles.length) S.profiles.push(clone(GENERIC));
    if (!S.playlists.length) S.playlists.push({ id: newId("p-"), name: "Playlist 1", entries: [], norm: { on: false, target: -18, mode: "track" } });
    if (!S.sets.length) S.sets.push({ id: newId("set-"), name: "My stick", profileId: S.profiles[0].id, playlistIds: [S.playlists[0].id], stickId: null, label: "" });
    for (const r of d.syncs) S.syncs[r.setId] = r;
    S.current = pl(st.currentPlaylist) ? st.currentPlaylist : S.playlists[0].id;
    S.setId = stickSet(st.currentSet) ? st.currentSet : (setsFor(S.current)[0] || S.sets[0]).id;
    await refreshSticks(true);
    // The demo's sync records predate its settings: they count as current.
    for (const s of S.sets) if (S.syncs[s.id] && S.syncs[s.id].recipe == null) S.syncs[s.id].recipe = recipeOf(planSet(s));
    const ro = new ResizeObserver(() => {
      const w = $("list-wrap").clientWidth;
      const before = visibleCols(pl()).join();
      S.listWidth = w;
      if (visibleCols(pl()).join() !== before) renderList();
    });
    ro.observe($("list-wrap"));
    S.listWidth = $("list-wrap").clientWidth || 800;
    api.volume(S.player.volume).catch(() => {});
    renderAll();
    persist();
    scheduleWork();
    // For the app's self-test (tests/app_e2e.py) and for poking around in the inspector.
    P.app = { S, pl, stickSet, track, planSet, staleness, addPaths, playEntry, stopPlayback, refreshSticks, ready: true };
    if (S.ffmpegError) toast("ffmpeg wasn't found, so files can't be read or converted. Reinstall Playlist2USB to get it back.");
    if (!DEMO) {
      stickPoll = setInterval(() => { if (!(S.sync && S.sync.running)) refreshSticks(true); }, 3000);
      api.refreshTracks().then((ts) => {
        for (const t of ts) {
          const old = S.tracks.get(t.id);
          S.tracks.set(t.id, old && old.analysisError ? { ...t, analysisError: old.analysisError } : t);
        }
        scheduleRender();
        scheduleWork();
      }).catch(() => {});
      if (d.openPaths && d.openPaths.length) addPaths(d.openPaths, { kind: "new" });
    }
  }
  init();
})();
