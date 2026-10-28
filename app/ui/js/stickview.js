// A stick's view in the main window (click a stick on the left): every
// folder and file in the order a player reads them, folders and joined
// tracks collapsible. It's for organizing the stick's playlists: drag them
// in from the left (onto the view, or onto a stick in the sidebar), move
// them by their handles, put silence between them, take them off. What's
// in a playlist is edited in the playlist.
"use strict";
(function (Fatstick) {
  const { h, fill, icon, clock, length, bytes, plural, toast } = Fatstick.ui;

  Fatstick.stickViewSetup = function (core) {
    const { S, pl, stickSet, layoutData, edit, newId, isGap, gapOf, renderSide } = core;
    const root = document.getElementById("stickview");
    // Folders and joined tracks closed by the person, remembered per stick.
    let closed = new Set();
    let shownSet = null;
    const storeKey = (setId) => `fatstick-layout-closed:${setId}`;
    function load(setId) {
      closed = new Set();
      try { closed = new Set(JSON.parse(localStorage.getItem(storeKey(setId)) || "[]")); } catch { /* none kept */ }
    }
    const remember = () => { try { if (shownSet) localStorage.setItem(storeKey(shownSet), JSON.stringify([...closed])); } catch { /* not kept */ } };

    /** One file: its place, name on the stick, what it is, length, how it's made, size. */
    function fileRow(f, folderKey) {
      const subs = [];
      const who = [f.title, f.artist].filter(Boolean).join(", ");
      if (who && f.kind !== "silence") subs.push(who);
      if (f.gain) subs.push(`Gain ${f.gain}`);
      if (f.edges) subs.push(f.edges);
      for (const n of f.notes || []) subs.push(n);
      for (let i = 0; i < subs.length; i++) subs[i] = subs[i].charAt(0).toUpperCase() + subs[i].slice(1);
      const row = h("li", { class: "lay-file" },
        h("span", { class: "lay-n", text: String(f.n).padStart(2, "0") }),
        h("span", { class: "lay-name" }, h("span", { class: "nm", text: f.name }), subs.length ? h("span", { class: "sub", text: subs.join(". ") }) : null),
        h("span", { class: "lay-len", text: clock(f.secs) }),
        h("span", { class: `lay-how${f.how.startsWith("Copy") ? "" : " enc"}`, text: f.how }),
        h("span", { class: "lay-size", text: bytes(f.bytes) }));
      if (!f.parts) return row;
      // A joined track: the tracks inside, with where each starts.
      const key = `${folderKey}/${f.name}`;
      const inner = h("details", { class: "lay-parts", open: !closed.has(key) || null },
        h("summary", { text: `${plural(f.parts.length, "track")} inside, in this order` }),
        h("ol", null, f.parts.map((x) => h("li", null,
          h("span", { class: "lay-at", text: clock(x.at) }),
          h("span", { class: "lay-name" }, h("span", { class: "nm", text: x.title }),
            h("span", { class: "sub", text: [x.artist, x.gain ? `Gain ${x.gain}` : "", x.edges].filter(Boolean).join(". ") })),
          h("span", { class: "lay-len", text: clock(x.secs) })))));
      inner.addEventListener("toggle", () => { if (inner.open) closed.delete(key); else closed.add(key); remember(); });
      row.append(inner);
      return row;
    }

    /** A stick's playlists in the order they go on it: put one before `index`. */
    function place(s, id, index, how) {
      const p = isGap(id) ? null : pl(id);
      const what = isGap(id) ? "the silence" : p ? p.name : "it";
      const from = s.playlistIds.indexOf(id);
      if (from >= 0 && (index === from || index === from + 1)) return;
      edit(from >= 0 ? `Move ${what}` : `Put ${what} on ${s.name}`, () => {
        const a = s.playlistIds;
        let at = index;
        if (from >= 0) { a.splice(from, 1); if (from < at) at--; }
        a.splice(Math.max(0, Math.min(a.length, at)), 0, id);
      });
      if (how === "side" && from < 0) toast(`Put ${what} on ${s.name}.`);
    }
    function addSilence(s, index) {
      const id = newId("gap-");
      edit(`Put silence on ${s.name}`, () => {
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
    function render() {
      if (root.hidden) return;
      const a = document.activeElement;
      if (a && root.contains(a) && a.matches("input")) { deferred = true; return; }
      const s = stickSet();
      if (!s) {
        fill(root, h("div", { class: "sv-scroll" }, h("p", { class: "st-note", text: "Make a stick on the left to choose what goes on it." })));
        return;
      }
      if (s.id !== shownSet) { shownSet = s.id; load(s.id); }
      const d = layoutData(s.id);
      const byId = new Map(d.folders.map((f) => [f.id, f]));
      const items = s.playlistIds.filter((id) => (isGap(id) ? gapOf(s, id) : pl(id)));
      const files = d.folders.reduce((a, f) => a + f.files.length, 0);
      const secs = d.folders.reduce((a, f) => a + f.secs, 0);
      const total = d.folders.reduce((a, f) => a + f.bytes, 0);
      const where = d.stick ? `On ${d.stick.name}, ${bytes(d.stick.size)}.` : "No USB stick picked.";
      const space = d.space
        ? `. ${bytes(d.space.used)} of ${bytes(d.space.data)} used${d.space.fits ? "" : ", more than the stick holds"}.`
        : `, about ${bytes(total)}.`;
      const all = (open) => () => {
        for (const el of root.querySelectorAll("details")) el.open = open;
      };
      const list = h("div", { class: "lay-folders", id: "sv-list", role: "list", "aria-label": `What's on ${s.name}, in order` });
      items.forEach((id, i) => {
        if (!d.single && i > 0) list.append(inserter(s, i));
        list.append(folderBlock(s, d, id, byId.get(id)));
      });
      list.append(h("div", { class: "drop-line sv-drop", id: "sv-drop", hidden: true }));

      fill(root,
        h("header", { class: "sv-head" },
          h("div", { class: "lay-title" },
            h("h1", { text: s.name }),
            h("span", { class: `pill ${d.status.kind === "ok" ? "ok" : d.status.kind === "out" ? "out" : ""}`, text: d.status.kind === "ok" ? "Up to date" : d.status.kind === "out" ? "Out of date" : "New" })),
          h("p", { class: "lay-sub", text: `${d.profile} profile. Label ${d.label || "none"}. ${where}` }),
          h("p", { class: `lay-status ${d.status.kind}`, text: d.status.text }),
          h("div", { class: "lay-bar" },
            h("p", { class: `lay-sum${d.space && !d.space.fits ? " bad" : ""}`, text: `${plural(d.folders.length, d.single ? "playlist" : "folder")}, ${plural(files, "file")}, ${length(secs)}${space}` }),
            h("span", { class: "tools" },
              d.single ? null : h("button", { type: "button", class: "btn small", id: "sv-silence", title: "Silence at the end; drag it where it goes", onclick: () => addSilence(s, s.playlistIds.length) }, icon("plus"), "Silence"),
              h("button", { type: "button", class: "btn small", text: "Expand all", onclick: all(true) }),
              h("button", { type: "button", class: "btn small", text: "Collapse all", onclick: all(false) })))),
        h("div", { class: "sv-scroll", id: "sv-scroll" },
          d.errors && d.errors.length ? h("p", { class: "st-note err", text: `The file names don't work: ${d.errors[0]}` }) : null,
          items.length ? list : h("div", { class: "sv-empty", id: "sv-list" },
            h("p", { text: "Nothing on this stick yet. Drag a playlist here from the left." }),
            h("div", { class: "drop-line sv-drop", id: "sv-drop", hidden: true })),
          d.ignored && d.ignored.length ? h("p", { class: "st-note warn", text: `Not written, as this profile puts one playlist on a stick: ${d.ignored.join(", ")}.` }) : null,
          h("p", { class: "lay-foot", text: "The order here is the order a player reads the stick in: folders, then files, top to bottom. Drag a playlist in from the left, or by its handle to move it." })),
      );
    }

    /** Between two folders: silence here (shown on hover). */
    function inserter(s, index) {
      return h("div", { class: "sv-insert" },
        h("button", { type: "button", class: "sv-ins-btn", title: "Put silence here, between these playlists", onclick: () => addSilence(s, index) }, icon("plus"), "Silence here"));
    }

    /** One playlist's folder (or silence) on the stick: what's in it, in order. */
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
        type: "button", class: "sv-off", title: "Take off this stick", "aria-label": `Take ${what} off this stick`,
        onclick: (ev) => {
          ev.preventDefault();
          edit(`Take ${what} off ${s.name}`, () => {
            s.playlistIds = s.playlistIds.filter((x) => x !== id);
            if (gap) s.gaps = s.gaps.filter((g) => g.id !== id);
          });
          toast(gap ? `Took the silence off ${s.name}.` : `Took ${what} off ${s.name}. The playlist itself is still there.`, { label: "Undo", run: core.undo });
        },
      }, icon("close"));
      const name = f ? (d.single ? "Top level of the stick" : `${f.folder}/`) : gap ? "Silence" : p.name;

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
          meta = h("span", { class: "lay-fmeta gap-meta" }, secs, h("span", { text: `s of silence${f ? `, ${bytes(f.bytes)}` : ""}` }));
        } else meta = h("span", { class: "lay-fmeta", text: "Not written: this profile puts one playlist on a stick" });
        return h("div", { class: `lay-folder lay-line k-${gap ? "silence" : "off"}`, role: "listitem", dataset: { item: id } },
          h("div", { class: "lay-sumline" }, grip, h("span", { class: "lay-fname", text: name }), meta, off));
      }
      const key = id; // the playlist, whatever its folder's number
      const kindText = f.kind === "joined"
        ? `${plural(f.tracks, "track")} joined into one file, ${length(f.secs)}, ${bytes(f.bytes)}`
        : `${plural(f.files.length, "file")}, ${length(f.secs)}, ${bytes(f.bytes)}`;
      const det = h("details", { class: `lay-folder k-${f.kind}`, open: !closed.has(key) || null, role: "listitem", dataset: { item: id, folder: key } },
        h("summary", null,
          grip,
          h("span", { class: "lay-fname", text: name }),
          !(f.folder || "").includes(f.playlist) ? h("span", { class: "lay-pl", text: f.playlist }) : null,
          h("span", { class: "lay-fmeta", text: kindText }),
          off),
        h("ol", { class: "lay-files" }, f.files.map((x) => fileRow(x, key))),
        f.skipped.length ? h("p", { class: "st-note warn lay-skip", text: `Not on the stick: ${f.skipped.join("; ")}.` }) : null);
      det.addEventListener("toggle", () => { if (det.open) closed.delete(key); else closed.add(key); remember(); });
      return det;
    }

    // ---- dragging playlists: from the left, and within the stick ----------------------------
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
      if (el.closest("#stick .folders, #stick [aria-label='Playlists on this stick']")) return { kind: "set", id: S.setId, panel: true };
      return null;
    }
    /** Where in the stick's list a drop at `y` goes (an index into its playlistIds). */
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
        : d.target.kind === "view" ? (d.item.from === "stick" ? `Move ${d.item.name} here` : `Put ${d.item.name} here`)
          : on ? `${d.item.name} is on ${s.name} already` : `Put ${d.item.name} on ${s.name}`;
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
      if (s.playlistIds.includes(d.item.id)) { toast(`${d.item.name} is on ${s.name} already.`); return; }
      place(s, d.item.id, s.playlistIds.length, "side");
      renderSide();
    }
    function onUp(ev) { if (drag && ev.pointerId === drag.pointerId) finish(true); }
    function onCancel(ev) { if (drag && ev.pointerId === drag.pointerId) finish(false); }
    window.addEventListener("click", (ev) => { if (swallowClick) { ev.stopPropagation(); ev.preventDefault(); swallowClick = false; } }, true);

    return { render, startDrag, dragging: () => !!(drag && drag.started) };
  };
})(window.Fatstick = window.Fatstick || {});
