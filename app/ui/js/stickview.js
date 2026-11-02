// A setlist's view in the main window (click a setlist on the left): its
// playlists and tracks in order, playlists and joined tracks collapsible. With
// the transfer open, the same list as it goes on a stick: folders and files,
// how each is made and how big. It's for organizing the setlist's playlists: drag them
// in from the left (onto the view, or onto a setlist in the sidebar), move
// them by their handles, put silence between them, take them out. What's
// in a playlist is edited in the playlist. It plays too, from its Play button
// or from a file double-clicked, and shows what's playing.
"use strict";
(function (Fatstick) {
  const { h, fill, icon, clock, length, bytes, plural, toast } = Fatstick.ui;

  Fatstick.stickViewSetup = function (core) {
    const { S, pl, stickSet, layoutData, edit, newId, isGap, gapOf, renderSide, printSetlist } = core;
    const root = document.getElementById("stickview");
    // Folders and joined tracks closed by the person, remembered per setlist.
    let closed = new Set();
    let shownSet = null;
    const storeKey = (setId) => `fatstick-layout-closed:${setId}`;
    function load(setId) {
      closed = new Set();
      try { closed = new Set(JSON.parse(localStorage.getItem(storeKey(setId)) || "[]")); } catch { /* none kept */ }
    }
    const remember = () => { try { if (shownSet) localStorage.setItem(storeKey(shownSet), JSON.stringify([...closed])); } catch { /* not kept */ } };

    /** The transfer is open: this is about what goes on the stick. */
    const onStick = () => S.processing === "fat";
    /** One track: its place, title and artist, what's done to it, length. On a
     * stick: its file's name, what it is, length, how it's made, size. */
    function fileRow(f, folderKey) {
      const stick = onStick();
      const subs = [];
      const who = stick ? [f.title, f.artist].filter(Boolean).join(", ") : f.kind === "joined" ? "" : f.artist;
      if (who && f.kind !== "silence") subs.push(who);
      if (f.gain) subs.push(`Gain ${f.gain}`);
      if (f.edges) subs.push(f.edges);
      if (stick) for (const n of f.notes || []) subs.push(n);
      for (let i = 0; i < subs.length; i++) subs[i] = subs[i].charAt(0).toUpperCase() + subs[i].slice(1);
      const row = h("li", { class: `lay-file${stick ? "" : " plain"}`, dataset: { file: f.id } },
        h("span", { class: "lay-n", text: String(f.n).padStart(2, "0") }),
        h("span", { class: "lay-name" }, h("span", { class: "nm", text: stick ? f.name : f.title }), subs.length ? h("span", { class: "sub", text: subs.join(". ") }) : null),
        h("span", { class: "lay-len", text: clock(f.secs) }),
        stick ? h("span", { class: `lay-how${f.how.startsWith("Copy") ? "" : " enc"}`, text: f.how }) : null,
        stick ? h("span", { class: "lay-size", text: bytes(f.bytes) }) : null);
      if (!f.parts) return row;
      // A joined track: the tracks inside, with where each starts.
      const key = `${folderKey}/${f.name}`;
      const inner = h("details", { class: "lay-parts", open: !closed.has(key) || null },
        h("summary", { text: `${plural(f.parts.length, "track")} inside, in this order` }),
        h("ol", null, f.parts.map((x) => h("li", { dataset: { part: x.id } },
          h("span", { class: "lay-at", text: clock(x.at) }),
          h("span", { class: "lay-name" }, h("span", { class: "nm", text: x.title }),
            h("span", { class: "sub", text: [x.artist, x.gain ? `Gain ${x.gain}` : "", x.edges].filter(Boolean).join(". ") })),
          h("span", { class: "lay-len", text: clock(x.secs) })))));
      inner.addEventListener("toggle", () => { if (inner.open) closed.delete(key); else closed.add(key); remember(); });
      row.append(inner);
      return row;
    }

    /** A setlist's playlists in order: put one before `index`. */
    function place(s, id, index, how) {
      const p = isGap(id) ? null : pl(id);
      const what = isGap(id) ? "the silence" : p ? p.name : "it";
      const from = s.playlistIds.indexOf(id);
      if (from >= 0 && (index === from || index === from + 1)) return;
      edit(from >= 0 ? `Move ${what}` : `Add ${what} to ${s.name}`, () => {
        const a = s.playlistIds;
        let at = index;
        if (from >= 0) { a.splice(from, 1); if (from < at) at--; }
        a.splice(Math.max(0, Math.min(a.length, at)), 0, id);
      });
      if (how === "side" && from < 0) toast(`Added ${what} to ${s.name}.`);
    }
    function addSilence(s, index) {
      const id = newId("gap-");
      edit(`Add silence to ${s.name}`, () => {
        s.gaps = [...(s.gaps || []), { id, seconds: 30 }];
        s.playlistIds.splice(index, 0, id);
      });
      // Straight to its seconds, to change them.
      requestAnimationFrame(() => {
        const input = root.querySelector(`[data-item="${id}"] input.secs`);
        if (input) { input.focus(); input.select(); }
      });
    }

    // Not while the seconds of a silence are being typed: after.
    let deferred = false;
    root.addEventListener("focusout", () => { if (deferred) { deferred = false; setTimeout(render, 0); } });
    // Double-click a file, or a track in a joined one, to play the setlist from there.
    root.addEventListener("dblclick", (ev) => {
      if (ev.target.closest("input, button, summary")) return;
      const row = ev.target.closest("[data-file]");
      if (!row) return;
      const part = ev.target.closest("[data-part]");
      window.getSelection()?.removeAllRanges();
      core.playSetlistFile(row.dataset.file, part ? part.dataset.part : null);
    });
    /** What's playing, if it's this setlist: its row, followed as it moves on. */
    let followed = null;
    function markPlaying() {
      if (root.hidden) return;
      const pp = S.player;
      const sp = pp.set && pp.set.id === shownSet ? pp.set : null;
      const btn = root.querySelector("#sv-play");
      if (btn) fill(btn, icon(sp && pp.playing ? "pause" : "play"), sp ? (pp.playing ? "Pause" : "Go on") : "Play");
      for (const el of root.querySelectorAll(".sv-now")) el.classList.remove("sv-now");
      if (!sp) { followed = null; return; }
      const row = root.querySelector(`[data-file="${CSS.escape(sp.file)}"]`);
      const part = sp.item.joined && pp.entryId ? root.querySelector(`[data-part="${CSS.escape(pp.entryId)}"]`) : null;
      for (const el of [row, part, row && row.closest(".lay-folder")]) if (el) el.classList.add("sv-now");
      const at = `${sp.file}/${pp.entryId}`;
      if (at !== followed) {
        followed = at;
        const el = part || row;
        if (el && el.offsetParent) el.scrollIntoView({ block: "nearest" });
      }
    }
    function render() {
      if (root.hidden) return;
      const a = document.activeElement;
      if (a && root.contains(a) && a.matches("input")) { deferred = true; return; }
      const s = stickSet();
      if (!s) {
        fill(root, h("div", { class: "sv-scroll" }, h("p", { class: "st-note", text: "Make a setlist on the left to choose what goes on it." })));
        return;
      }
      if (s.id !== shownSet) { shownSet = s.id; load(s.id); }
      const d = layoutData(s.id);
      const byId = new Map(d.folders.map((f) => [f.id, f]));
      const items = s.playlistIds.filter((id) => (isGap(id) ? gapOf(s, id) : pl(id)));
      const files = d.folders.reduce((a, f) => a + f.files.length, 0);
      const secs = d.folders.reduce((a, f) => a + f.secs, 0);
      const total = d.folders.reduce((a, f) => a + f.bytes, 0);
      const stick = onStick();
      const where = d.stick ? `Syncs to ${d.stick.name}, ${bytes(d.stick.size)}.` : "No USB stick picked.";
      const space = d.space
        ? `. ${bytes(d.space.used)} of ${bytes(d.space.data)} used${d.space.fits ? "" : ", more than the stick holds"}.`
        : `, about ${bytes(total)}.`;
      const real = d.folders.filter((f) => f.kind !== "silence");
      const tracks = real.reduce((a, f) => a + f.tracks, 0);
      const all = (open) => () => {
        for (const el of root.querySelectorAll("details")) el.open = open;
      };
      const scrolled = root.querySelector("#sv-scroll");
      const scrollTop = scrolled ? scrolled.scrollTop : 0;
      const list = h("div", { class: "lay-folders", id: "sv-list", role: "list", "aria-label": `What's in ${s.name}, in order` });
      items.forEach((id, i) => {
        if (!d.single && i > 0) list.append(inserter(s, i));
        list.append(folderBlock(s, d, id, byId.get(id)));
      });
      list.append(h("div", { class: "drop-line sv-drop", id: "sv-drop", hidden: true }));

      fill(root,
        h("header", { class: "sv-head" },
          h("div", { class: "lay-title" },
            h("h1", { text: s.name }),
            stick ? h("span", { class: `pill ${d.status.kind === "ok" ? "ok" : d.status.kind === "out" ? "out" : ""}`, text: d.status.kind === "ok" ? "Up to date" : d.status.kind === "out" ? "Out of date" : "New" }) : null),
          stick ? h("p", { class: "lay-sub", text: `${d.profile} profile. Label ${d.label || "none"}. ${where}` }) : null,
          stick ? h("p", { class: `lay-status ${d.status.kind}`, text: d.status.text }) : null,
          h("div", { class: "lay-bar" },
            h("p", { class: `lay-sum${stick && d.space && !d.space.fits ? " bad" : ""}`, text: stick ? `${plural(d.folders.length, d.single ? "playlist" : "folder")}, ${plural(files, "file")}, ${length(secs)}${space}` : `${plural(real.length, "playlist")}, ${plural(tracks, "track")}, ${length(secs)}.` }),
            h("span", { class: "tools" },
              h("button", { type: "button", class: "btn small", id: "sv-play", disabled: !files, title: "Play this setlist from beginning to end, as the stick will have it. Double-click a file to start there.", onclick: () => core.toggleSetlist(s.id) }, icon("play"), "Play"),
              d.single ? null : h("button", { type: "button", class: "btn small", id: "sv-silence", title: "Silence at the end; drag it where it goes", onclick: () => addSilence(s, s.playlistIds.length) }, icon("plus"), "Silence"),
              h("button", { type: "button", class: "btn small", text: "Expand all", onclick: all(true) }),
              h("button", { type: "button", class: "btn small", text: "Collapse all", onclick: all(false) }),
              h("button", { type: "button", class: "btn small", text: "Print…", title: "This setlist on paper (Ctrl+P)", onclick: () => printSetlist() }))),
          // Ways of getting this setlist out of the app, each opening the panel at the right.
          h("section", { class: "sv-transfer", "aria-labelledby": "sv-transfer-h" },
            h("h2", { id: "sv-transfer-h", text: "Transfer this setlist" }),
            h("div", { class: "tools" }, core.transfers.map(([kind, m]) => h("button", {
              type: "button", class: "btn small", id: `proc-${kind}`, "aria-pressed": "false", "aria-controls": "stick", text: m.label,
              onclick: () => core.setProcessing(S.processing === kind ? null : kind),
            }))))),
        h("div", { class: "sv-scroll", id: "sv-scroll" },
          d.errors && d.errors.length ? h("p", { class: "st-note err", text: `The file names don't work: ${d.errors[0]}` }) : null,
          items.length ? list : h("div", { class: "sv-empty", id: "sv-list" },
            h("p", { text: "Nothing in this setlist yet. Drag a playlist here from the left." }),
            h("div", { class: "drop-line sv-drop", id: "sv-drop", hidden: true })),
          d.ignored && d.ignored.length ? h("p", { class: "st-note warn", text: `${stick ? "Not written" : "Left out"}, as this setlist's player profile puts one playlist on a stick: ${d.ignored.join(", ")}.` }) : null,
          h("p", { class: "lay-foot", text: stick ? "The order here is the order a player reads the stick in: folders, then files, top to bottom. Drag a playlist in from the left, or by its handle to move it." : "Double-click a track to play from there. Drag a playlist in from the left, or by its handle to move it." })),
      );
      // Drawn again, it stays where it was scrolled to.
      if (scrollTop) root.querySelector("#sv-scroll").scrollTop = scrollTop;
      core.renderProcessButtons();
      markPlaying();
    }

    /** Between two folders: silence here (shown on hover). */
    function inserter(s, index) {
      return h("div", { class: "sv-insert" },
        h("button", { type: "button", class: "sv-ins-btn", title: "Put silence here, between these playlists", onclick: () => addSilence(s, index) }, icon("plus"), "Silence here"));
    }

    /** One playlist's folder (or silence) in the setlist: what's in it, in order. */
    function folderBlock(s, d, id, f) {
      const gap = isGap(id) ? gapOf(s, id) : null;
      const p = gap ? null : pl(id);
      const what = gap ? "the silence" : p.name;
      const mv = (dlt) => edit(`Move ${what} ${dlt < 0 ? "up" : "down"}`, () => {
        const a = s.playlistIds;
        const j = a.indexOf(id);
        if (j < 0 || j + dlt < 0 || j + dlt >= a.length) return false;
        [a[j], a[j + dlt]] = [a[j + dlt], a[j]];
      });
      const grip = h("button", {
        type: "button", class: "sv-grip", title: "Drag to move it. Or Alt+↑ and Alt+↓.", "aria-label": `Move ${what}: drag, or Alt+Up and Alt+Down`,
        onclick: (ev) => ev.preventDefault(),
        onkeydown: (ev) => {
          if (ev.altKey && (ev.key === "ArrowUp" || ev.key === "ArrowDown")) {
            ev.preventDefault();
            mv(ev.key === "ArrowUp" ? -1 : 1);
            requestAnimationFrame(() => { const g = root.querySelector(`[data-item="${id}"] .sv-grip`); if (g) g.focus(); });
          }
        },
      }, icon("grip"));
      grip.addEventListener("pointerdown", (ev) => { if (ev.button === 0) startDrag(ev, { id, name: gap ? "Silence" : p.name, from: "stick" }); });
      const off = h("button", {
        type: "button", class: "sv-off", title: "Take out of this setlist", "aria-label": `Take ${what} out of this setlist`,
        onclick: (ev) => {
          ev.preventDefault();
          edit(`Take ${what} out of ${s.name}`, () => {
            s.playlistIds = s.playlistIds.filter((x) => x !== id);
            if (gap) s.gaps = s.gaps.filter((g) => g.id !== id);
          });
          toast(gap ? `Took the silence out of ${s.name}.` : `Took ${what} out of ${s.name}. The playlist itself is still there.`, { label: "Undo", run: core.undo });
        },
      }, icon("close"));
      const stick = onStick();
      const name = !f ? (gap ? "Silence" : p.name) : !stick ? (gap ? "Silence" : f.playlist) : d.single ? "Top level of the stick" : `${f.folder}/`;

      // Silence, or a playlist this profile doesn't write: a line, nothing inside.
      if (gap || !f) {
        let meta;
        if (gap) {
          const secs = h("input", { type: "number", class: "input secs", min: "1", max: "600", step: "1", value: String(gap.seconds), "aria-label": "Seconds of silence" });
          secs.addEventListener("change", () => {
            const v = Math.round(Number(secs.value));
            if (!(v >= 1 && v <= 600)) { secs.value = String(gap.seconds); return; }
            edit(`Silence of ${v} s`, () => { gapOf(s, id).seconds = v; });
          });
          secs.addEventListener("keydown", (ev) => { if (ev.key === "Enter") secs.blur(); });
          meta = h("span", { class: "lay-fmeta gap-meta" }, secs, h("span", { text: `s of silence${f && stick ? `, ${bytes(f.bytes)}` : ""}` }));
        } else meta = h("span", { class: "lay-fmeta", text: `${stick ? "Not written" : "Left out"}: this setlist's player profile puts one playlist on a stick` });
        return h("div", { class: `lay-folder lay-line k-${gap ? "silence" : "off"}`, role: "listitem", dataset: { item: id, ...(f ? { file: f.files[0].id } : {}) } },
          h("div", { class: "lay-sumline" }, grip, h("span", { class: "lay-fname", text: name }), meta, off));
      }
      const key = id; // the playlist, whatever its folder's number
      const kindText = !stick
        ? `${plural(f.tracks, "track")}${f.kind === "joined" ? " as one" : ""}, ${length(f.secs)}`
        : f.kind === "joined"
          ? `${plural(f.tracks, "track")} joined into one file, ${length(f.secs)}, ${bytes(f.bytes)}`
          : `${plural(f.files.length, "file")}, ${length(f.secs)}, ${bytes(f.bytes)}`;
      const det = h("details", { class: `lay-folder k-${f.kind}`, open: !closed.has(key) || null, role: "listitem", dataset: { item: id, folder: key } },
        h("summary", null,
          grip,
          h("span", { class: "lay-fname", text: name }),
          stick && !(f.folder || "").includes(f.playlist) ? h("span", { class: "lay-pl", text: f.playlist }) : null,
          h("span", { class: "lay-fmeta", text: kindText }),
          off),
        h("ol", { class: "lay-files" }, f.files.map((x) => fileRow(x, key))),
        f.skipped.length ? h("p", { class: "st-note warn lay-skip", text: `${stick ? "Not on the stick" : "Left out"}: ${f.skipped.join("; ")}.` }) : null);
      det.addEventListener("toggle", () => { if (det.open) closed.delete(key); else closed.add(key); remember(); });
      return det;
    }

    // ---- dragging playlists: from the left, and within the setlist --------------------------
    let drag = null;
    let swallowClick = false;
    /** Start (maybe) dragging `item` ({ id, name, from: "side" | "stick" }). */
    function startDrag(ev, item) {
      drag = { x: ev.clientX, y: ev.clientY, started: false, pointerId: ev.pointerId, item, target: null };
      if (item.from === "stick") ev.preventDefault();
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onCancel);
    }
    function targetAt(x, y) {
      const el = document.elementFromPoint(x, y);
      if (!el) return null;
      const side = el.closest("[data-set]");
      if (side) return { kind: "set", id: side.dataset.set };
      const list = el.closest("#sv-list");
      if (list && !root.hidden) return { kind: "view", index: indexAt(y) };
      if (el.closest("#stick .folders, #stick [aria-label='Playlists in this setlist']")) return { kind: "set", id: S.setId, panel: true };
      return null;
    }
    /** Where in the setlist a drop at `y` goes (an index into its playlistIds). */
    function indexAt(y) {
      const s = stickSet();
      const cards = [...root.querySelectorAll(".lay-folder[data-item]")];
      for (const c of cards) {
        const r = c.getBoundingClientRect();
        if (y < r.top + r.height / 2) return s.playlistIds.indexOf(c.dataset.item);
      }
      return s.playlistIds.length;
    }
    function showDrop(index) {
      const line = root.querySelector("#sv-drop");
      const list = root.querySelector("#sv-list");
      if (!line || !list) return;
      const s = stickSet();
      const cards = [...root.querySelectorAll(".lay-folder[data-item]")];
      const lr = list.getBoundingClientRect();
      let y;
      if (!cards.length) y = lr.height / 2;
      else {
        const before = cards.find((c) => s.playlistIds.indexOf(c.dataset.item) >= index);
        const r = (before || cards[cards.length - 1]).getBoundingClientRect();
        y = (before ? r.top - 4 : r.bottom + 3) - lr.top;
      }
      line.style.top = `${y}px`;
      line.hidden = false;
    }
    function hideDrop() { const line = root.querySelector("#sv-drop"); if (line) line.hidden = true; }
    function onMove(ev) {
      const d = drag;
      if (!d || ev.pointerId !== d.pointerId) return;
      if (!d.started) {
        if (Math.hypot(ev.clientX - d.x, ev.clientY - d.y) < 5) return;
        d.started = true;
        document.body.classList.add("pl-dragging");
        root.querySelector(`[data-item="${d.item.id}"]`)?.classList.add("dragging");
        document.querySelector(`[data-pl="${d.item.id}"]`)?.classList.add("dragging");
        $badge().hidden = false;
      }
      d.px = ev.clientX;
      d.py = ev.clientY;
      if (!d.scrolling) { d.scrolling = true; requestAnimationFrame(autoScroll); }
      const t = targetAt(ev.clientX, ev.clientY);
      const s = t && t.kind === "set" ? stickSet(t.id) : null;
      d.target = t && (t.kind === "view" || s) ? t : null;
      if (d.target && d.target.kind === "view") showDrop(d.target.index); else hideDrop();
      for (const b of document.querySelectorAll("[data-set]")) b.classList.toggle("drop-target", !!d.target && d.target.kind === "set" && b.dataset.set === d.target.id);
      root.classList.toggle("drop-over", !!d.target && d.target.kind === "view");
      const badge = $badge();
      badge.style.left = `${ev.clientX + 14}px`;
      badge.style.top = `${ev.clientY + 12}px`;
      badge.classList.toggle("nowhere", !d.target);
      const on = s && s.playlistIds.includes(d.item.id);
      badge.textContent = !d.target ? d.item.name
        : d.target.kind === "view" ? (d.item.from === "stick" ? `Move ${d.item.name} here` : `Add ${d.item.name} here`)
          : on ? `${d.item.name} is in ${s.name} already` : `Add ${d.item.name} to ${s.name}`;
    }
    const $badge = () => document.getElementById("drag-badge");
    /** Near the top or bottom of the view while dragging: scroll it. */
    function autoScroll() {
      const d = drag;
      if (!d || !d.started) return;
      const sc = root.querySelector("#sv-scroll");
      if (sc && !root.hidden) {
        const r = sc.getBoundingClientRect();
        const edge = 40;
        let v = 0;
        if (d.px > r.left && d.px < r.right) {
          if (d.py < r.top + edge && d.py > r.top - 30) v = -Math.ceil((r.top + edge - d.py) / 3);
          else if (d.py > r.bottom - edge && d.py < r.bottom + 30) v = Math.ceil((d.py - (r.bottom - edge)) / 3);
        }
        if (v) {
          sc.scrollTop += v;
          const t = targetAt(d.px, d.py);
          if (t && t.kind === "view") { d.target = t; showDrop(t.index); }
        }
      }
      requestAnimationFrame(autoScroll);
    }
    function finish(commit) {
      const d = drag;
      drag = null;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      if (!d || !d.started) return;
      // The click that ends a drag isn't a click on what's under it.
      swallowClick = true;
      setTimeout(() => { swallowClick = false; }, 0);
      hideDrop();
      document.body.classList.remove("pl-dragging");
      root.classList.remove("drop-over");
      for (const el of document.querySelectorAll(".dragging, .drop-target")) el.classList.remove("dragging", "drop-target");
      $badge().hidden = true;
      if (!commit || !d.target) return;
      if (d.target.kind === "view") { place(stickSet(), d.item.id, d.target.index, d.item.from); return; }
      const s = stickSet(d.target.id);
      if (!s) return;
      if (s.playlistIds.includes(d.item.id)) { toast(`${d.item.name} is in ${s.name} already.`); return; }
      place(s, d.item.id, s.playlistIds.length, "side");
      renderSide();
    }
    function onUp(ev) { if (drag && ev.pointerId === drag.pointerId) finish(true); }
    function onCancel(ev) { if (drag && ev.pointerId === drag.pointerId) finish(false); }
    window.addEventListener("click", (ev) => { if (swallowClick) { ev.stopPropagation(); ev.preventDefault(); swallowClick = false; } }, true);

    return { render, markPlaying, startDrag, dragging: () => !!(drag && drag.started) };
  };
})(window.Fatstick = window.Fatstick || {});
