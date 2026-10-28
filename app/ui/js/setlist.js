// A stick printed as a setlist: its playlists in order, big titles to read
// from the floor, the details under them a little smaller, black on white
// only (a laser printer, no colour, no greys), tight margins so more fits on
// a page. Each playlist shows its songs, or just its name (filler music
// between sets, say), or is left out; the stick's silences show as lines of
// their own. The dialog has the playlist, page, text and content settings
// beside a preview of the actual pages; what prints is those pages.
"use strict";
(function (Fatstick) {
  const { h, fill, clock, length, plural, modal, closeModal, toast } = Fatstick.ui;

  const PAPER = { a4: { label: "A4", w: 210, h: 297 }, letter: { label: "Letter", w: 215.9, h: 279.4 } };
  const FONTS = {
    condensed: { label: "Barlow Condensed", family: '"Barlow Condensed", "Arial Narrow", sans-serif', bold: 700, normal: 500 },
    sans: { label: "Barlow", family: 'Barlow, "Helvetica Neue", Arial, sans-serif', bold: 600, normal: 400 },
    serif: { label: "Serif", family: 'Georgia, "Times New Roman", "Liberation Serif", serif', bold: 700, normal: 400 },
  };
  /** Space between songs, as a share of the title size. */
  const SPACING = { tight: 0.12, normal: 0.4, roomy: 0.8 };
  const MM = 96 / 25.4; // CSS px in a millimetre

  function defaults() {
    const letter = /^(en-US|en-CA|es-MX|fr-CA|en-PH)/i.test(navigator.language || "");
    return {
      paper: letter ? "letter" : "a4", landscape: false, columns: 1, rows: 1, dividers: true, margin: 10,
      font: "condensed", titlePt: 24, detailPt: 17, spacing: "normal", bold: true, caps: false,
      numbers: true, artist: true, album: false, length: true, key: true, total: true, pageNumbers: true, rules: false,
      runOn: false,
    };
  }

  Fatstick.setlistSetup = function (core) {
    const { S, pl, track, R, keyText, saveSetting, api, DEMO, stickSet, isGap, gapOf } = core;
    const o = defaults();
    // The line under the heading, per playlist or stick ("The Fox, Saturday 9pm").
    const subs = {};
    // For a stick's setlist, how each of its playlists shows: "songs", "name" or "out".
    const modes = {};
    /** The settings saved last time (read when the app starts). */
    function load(saved) {
      if (saved && saved.options) for (const k of Object.keys(o)) if (k in saved.options && typeof saved.options[k] === typeof o[k]) o[k] = saved.options[k];
      if (saved && saved.subs) Object.assign(subs, saved.subs);
      if (saved && saved.modes) Object.assign(modes, saved.modes);
    }
    const keep = () => saveSetting("setlist", { options: o, subs, modes });
    const MODES = [["songs", "Songs"], ["name", "Name only"], ["out", "Leave out"]];
    const GAP_MODES = [["name", "Show"], ["out", "Leave out"]];
    /** How a stick's playlist (or silence) shows: its songs, its name alone, or not at all. */
    const modeOf = (setId, id) => {
      const m = modes[setId] && modes[setId][id];
      if (isGap(id)) return m === "out" ? "out" : "name";
      return m || "songs";
    };

    const dims = () => {
      const p = PAPER[o.paper] || PAPER.a4;
      return o.landscape ? { w: p.h, h: p.w } : { w: p.w, h: p.h };
    };
    const stem = (n) => (n || "").replace(/\.[^.]+$/, "");

    /** The songs, in order, as the setlist shows them. */
    function songs(p, only, from = 0) {
      return p.entries.filter((e) => !only || only.has(e.id)).map((e, i) => {
        const t = track(e.trackId);
        const x = R.edges(e);
        return {
          n: from + i + 1, title: t.title || stem(t.filename) || "Untitled", artist: t.artist || "", album: t.album || "",
          secs: Math.max(0, (t.duration || 0) - x.cutStart - x.cutEnd),
          key: R.hasKey(e) ? `Key ${keyText(R.key(e).pitch)}` : "",
        };
      });
    }

    /**
     * What's on the page, as sections: the stick's playlists in its order,
     * each with its songs under its name, or its name alone, and its
     * silences; whatever's left out isn't there.
     */
    function sections(setId) {
      const st = stickSet(setId);
      if (!st) return [];
      const out = [];
      let n = 0;
      for (const id of st.playlistIds) {
        const mode = modeOf(st.id, id);
        if (mode === "out") continue;
        if (isGap(id)) {
          const g = gapOf(st, id);
          if (g) out.push({ id, kind: "name", gap: true, name: "Silence", secs: g.seconds, songs: [] });
          continue;
        }
        const p = pl(id);
        if (!p) continue;
        const list = songs(p, null, o.runOn ? n : 0);
        n += mode === "songs" ? list.length : 0;
        out.push({ id, kind: mode, name: p.name, secs: list.reduce((a, x) => a + x.secs, 0), songs: mode === "songs" ? list : [] });
      }
      return out;
    }

    /** A playlist's name over its songs, or on its own line. */
    function setEl(sec) {
      return h("div", { class: `sl-set${sec.kind === "name" ? " only" : ""}${sec.gap ? " gap" : ""}` },
        h("span", { class: "sl-set-name", text: sec.name }),
        o.length && sec.secs ? h("span", { class: "sl-len", text: clock(sec.secs) }) : null);
    }

    function songEl(s) {
      const sub = [o.artist ? s.artist : "", o.album ? s.album : "", o.key ? s.key : ""].filter(Boolean).join(", ");
      return h("div", { class: "sl-song" },
        o.numbers ? h("span", { class: "sl-n", text: String(s.n) }) : null,
        h("div", { class: "sl-main" },
          h("div", { class: "sl-line" },
            h("span", { class: "sl-title", text: s.title }),
            o.length && s.secs ? h("span", { class: "sl-len", text: clock(s.secs) }) : null),
          sub ? h("div", { class: "sl-sub", text: sub }) : null));
    }

    /**
     * The pages: each one exactly the paper's size, the songs flowing down
     * the first column, then the next, then onto the next page. Measured at
     * full size, out of sight.
     */
    function paginate(d) {
      const pd = dims();
      const F = FONTS[o.font] || FONTS.condensed;
      const most = Math.max(1, ...d.sections.flatMap((x) => x.songs.map((y) => y.n)));
      const digits = String(most).length;
      const vars = {
        width: `${pd.w}mm`, height: `${pd.h}mm`, padding: `${o.margin}mm`,
        "--sl-font": F.family, "--sl-title": `${o.titlePt}pt`, "--sl-detail": `${o.detailPt}pt`,
        "--sl-bold": String(F.bold), "--sl-weight": String(o.bold ? F.bold : F.normal), "--sl-normal": String(F.normal),
        "--sl-gap": `${(SPACING[o.spacing] ?? SPACING.normal) * o.titlePt}pt`, "--sl-nw": `${((digits * 0.6 + 0.2) * o.titlePt).toFixed(1)}pt`,
      };
      const cls = `sl-page${o.caps ? " caps" : ""}${o.rules ? " rules" : ""}${o.pageNumbers ? "" : " nofoot"}${o.numbers ? "" : " nonum"}`;
      const box = h("div", { class: "sl-measure", "aria-hidden": "true" });
      document.body.append(box);
      const pages = [];
      let cols;
      let ci = 0;
      // The page in parts: columns, and rows of them (two by two in landscape: quarters).
      // The songs fill each part in turn, left to right, then the next row.
      const parts = o.columns * o.rows;
      const lines = () => (!o.dividers ? [] : [
        ...Array.from({ length: o.columns - 1 }, (_, i) => h("i", { class: "sl-div v", style: { left: `calc(${i + 1} * (100% + 8mm) / ${o.columns} - 4mm)` } })),
        ...Array.from({ length: o.rows - 1 }, (_, i) => h("i", { class: "sl-div h", style: { top: `calc(${i + 1} * (100% + 6mm) / ${o.rows} - 3mm)` } })),
      ]);
      const newPage = () => {
        const first = !pages.length;
        cols = h("div", { class: "sl-cols", style: { "grid-template-columns": `repeat(${o.columns}, minmax(0, 1fr))`, "grid-template-rows": `repeat(${o.rows}, minmax(0, 1fr))` } },
          Array.from({ length: parts }, () => h("div", { class: "sl-col" })), lines());
        const page = h("div", { class: cls, style: vars },
          first ? h("header", { class: "sl-head" },
            h("div", { class: "sl-h1", text: d.heading || " " }),
            d.sub || d.total ? h("div", { class: "sl-h2" }, h("span", { text: d.sub }), h("span", { text: d.total })) : null) : null,
          cols,
          h("footer", { class: "sl-foot" }));
        box.append(page);
        pages.push(page);
        ci = 0;
      };
      newPage();
      // The flow: each playlist's name (when there are several), then its songs.
      const flow = [];
      for (const sec of d.sections) {
        flow.push({ el: setEl(sec) });
        for (const x of sec.songs) flow.push({ el: songEl(x) });
      }
      for (const item of flow) {
        for (;;) {
          const col = cols.children[ci];
          col.append(item.el);
          if (col.scrollHeight <= col.clientHeight + 1) break;
          item.el.remove();
          // Too tall for an empty column: it goes there anyway.
          if (!col.children.length) { col.append(item.el); break; }
          // A playlist's name doesn't stay at the foot of a column without its first song.
          const last = col.lastElementChild;
          const orphan = last && last.classList.contains("sl-set") && !last.classList.contains("only") && col.children.length > 1 ? last : null;
          if (ci + 1 < parts) ci++;
          else newPage();
          if (orphan) cols.children[ci].append(orphan);
        }
      }
      pages.forEach((pg, i) => fill(pg.querySelector(".sl-foot"),
        h("span", { text: i > 0 ? d.heading : "" }),
        h("span", { text: pages.length > 1 ? `${i + 1} / ${pages.length}` : "" })));
      box.remove();
      return pages;
    }

    /** Pages printed exactly at the paper's size, with no margin of the printer's own. */
    function pageRule() {
      const pd = dims();
      const sheet = [...document.styleSheets].find((s) => (s.href || "").endsWith("style.css"));
      if (!sheet) return;
      try {
        for (let i = sheet.cssRules.length - 1; i >= 0; i--) if (sheet.cssRules[i].type === CSSRule.PAGE_RULE) sheet.deleteRule(i);
        sheet.insertRule(`@page { size: ${pd.w}mm ${pd.h}mm; margin: 0; }`, sheet.cssRules.length);
      } catch { /* the printer's own page then */ }
    }
    /** The pages, ready in the document for printing (and for the tests' PDF). */
    function preparePrint(d) {
      const pages = paginate(d);
      let rootEl = document.getElementById("print-root");
      if (!rootEl) { rootEl = h("div", { id: "print-root" }); document.body.append(rootEl); }
      fill(rootEl, pages);
      pageRule();
      document.documentElement.classList.add("printing-setlist");
      return pages.length;
    }
    window.addEventListener("afterprint", () => document.documentElement.classList.remove("printing-setlist"));

    /** Everything a stick's pages need. */
    function pageData(setId, heading) {
      const secs = sections(setId);
      const n = secs.reduce((a, x) => a + x.songs.length, 0);
      const time = secs.reduce((a, x) => a + x.secs, 0);
      return { heading, sub: subs[setId] || "", total: o.total ? `${plural(n, "song")}, ${length(time)}` : "", sections: secs };
    }

    /** The setlist dialog for a stick: settings, and the pages as they'll print. */
    function open(setId) {
      let st = stickSet(setId) || stickSet();
      if (!st) { toast("Make a stick first: a setlist is printed from a stick's playlists."); return; }
      let heading = st.name;
      let headingTouched = false;
      const data = () => pageData(st.id, heading);

      // ---- the controls ---------------------------------------------------------------------
      let uid = 0;
      const field = (label, input, out) => h("label", { class: "sl-field" }, h("span", { class: "lbl", text: label }), input, out || null);
      const select = (key, opts) => {
        const el = h("select", { class: "input" }, Object.entries(opts).map(([v, t]) => h("option", { value: v, text: t, selected: String(o[key]) === v })));
        el.addEventListener("change", () => { o[key] = typeof o[key] === "number" ? Number(el.value) : el.value; changed(); });
        return el;
      };
      const seg = (key, opts, label) => {
        const name = `sl-${key}-${++uid}`;
        return h("div", { class: "sl-seg", role: "radiogroup", "aria-label": label },
          opts.map(([v, t]) => {
            const input = h("input", { type: "radio", name, value: String(v), checked: o[key] === v });
            input.addEventListener("change", () => { if (input.checked) { o[key] = v; changed(); } });
            return h("label", null, input, h("span", { text: t }));
          }));
      };
      const range = (key, min, max, step, unit, label) => {
        const out = h("output", { class: "sl-out", text: `${o[key]} ${unit}` });
        const el = h("input", { type: "range", min: String(min), max: String(max), step: String(step), value: String(o[key]), "aria-label": label });
        el.addEventListener("input", () => { o[key] = Number(el.value); out.textContent = `${o[key]} ${unit}`; changed(); });
        return [el, out];
      };
      const check = (key, label) => {
        const input = h("input", { type: "checkbox", checked: !!o[key] });
        input.addEventListener("change", () => { o[key] = input.checked; changed(); });
        return h("label", { class: "check sl-check" }, input, h("span", { text: label }));
      };
      const headIn = h("input", { class: "input", value: heading, "aria-label": "Heading" });
      headIn.addEventListener("input", () => { heading = headIn.value; headingTouched = true; changed(); });
      const subIn = h("input", { class: "input", value: subs[st.id] || "", placeholder: "Where and when, say", "aria-label": "Under the heading" });
      subIn.addEventListener("input", () => { if (subIn.value.trim()) subs[st.id] = subIn.value; else delete subs[st.id]; changed(); });

      // The stick, and how each of its playlists and silences shows.
      const stickSel = h("select", { class: "input", "aria-label": "Stick" }, S.sets.map((x) => h("option", { value: x.id, text: x.name })));
      stickSel.value = st.id;
      const plList = h("div", { class: "sl-pls" });
      function renderPls() {
        const rows = st.playlistIds.filter((id) => (isGap(id) ? gapOf(st, id) : pl(id))).map((id) => {
          const gap = isGap(id);
          const name = gap ? `Silence, ${gapOf(st, id).seconds} s` : pl(id).name;
          const nm = `sl-mode-${id}-${++uid}`;
          return h("div", { class: `sl-pl${gap ? " gap" : ""}` },
            h("span", { class: "sl-pl-name", text: name, title: name }),
            h("div", { class: "sl-seg small", role: "radiogroup", "aria-label": `How ${name} shows` },
              (gap ? GAP_MODES : MODES).map(([v, t]) => {
                const input = h("input", { type: "radio", name: nm, value: v, checked: modeOf(st.id, id) === v });
                input.addEventListener("change", () => {
                  if (!input.checked) return;
                  modes[st.id] = { ...(modes[st.id] || {}), [id]: v };
                  changed();
                });
                return h("label", null, input, h("span", { text: t }));
              })));
        });
        fill(plList, rows.length ? rows : h("p", { class: "st-note", text: "Nothing on this stick yet." }));
      }
      stickSel.addEventListener("change", () => {
        st = stickSet(stickSel.value);
        if (!headingTouched) { heading = st.name; headIn.value = heading; }
        subIn.value = subs[st.id] || "";
        renderPls();
        changed();
      });
      renderPls();
      const [marginIn, marginOut] = range("margin", 5, 25, 1, "mm", "Margins, millimetres");
      const [titleIn, titleOut] = range("titlePt", 12, 48, 1, "pt", "Title size, points");
      const [detailIn, detailOut] = range("detailPt", 8, 24, 1, "pt", "Detail size, points");

      const controls = h("div", { class: "sl-controls" },
        h("fieldset", { class: "sl-group" }, h("legend", { text: "Playlists" }),
          field("Stick", stickSel),
          plList,
          check("runOn", "Number straight through")),
        h("fieldset", { class: "sl-group" }, h("legend", { text: "Page" }),
          field("Paper", select("paper", Object.fromEntries(Object.entries(PAPER).map(([k, v]) => [k, v.label])))),
          seg("landscape", [[false, "Portrait"], [true, "Landscape"]], "Orientation"),
          h("div", { class: "sl-field" }, h("span", { class: "lbl", text: "Columns" }), seg("columns", [[1, "1"], [2, "2"], [3, "3"]], "Columns")),
          h("div", { class: "sl-field" }, h("span", { class: "lbl", text: "Rows" }), seg("rows", [[1, "1"], [2, "2"], [3, "3"]], "Rows")),
          check("dividers", "Lines between the parts"),
          field("Margins", marginIn, marginOut)),
        h("fieldset", { class: "sl-group" }, h("legend", { text: "Text" }),
          field("Font", select("font", Object.fromEntries(Object.entries(FONTS).map(([k, v]) => [k, v.label])))),
          field("Titles", titleIn, titleOut),
          field("Details", detailIn, detailOut),
          h("div", { class: "sl-field" }, h("span", { class: "lbl", text: "Spacing" }), seg("spacing", [["tight", "Tight"], ["normal", "Normal"], ["roomy", "Roomy"]], "Spacing between songs")),
          h("div", { class: "sl-checks" }, check("bold", "Bold titles"), check("caps", "Capital letters"))),
        h("fieldset", { class: "sl-group" }, h("legend", { text: "On the page" }),
          field("Heading", headIn),
          field("Under it", subIn),
          h("div", { class: "sl-checks" },
            check("numbers", "Numbers"), check("artist", "Artist"), check("album", "Album"), check("length", "Length"),
            check("key", "Key shift"), check("total", "Song count and length"), check("pageNumbers", "Page numbers"), check("rules", "Lines between songs"))));

      // ---- the preview ----------------------------------------------------------------------
      const info = h("p", { class: "sl-info", "aria-live": "polite" });
      const hint = h("p", { class: "sl-hint" });
      const sheets = h("div", { class: "sl-sheets" });
      const preview = h("div", { class: "sl-preview" }, info, sheets, hint);
      let raf = 0;
      function draw() {
        raf = 0;
        if (!document.contains(sheets)) return;
        const pages = paginate(data());
        const pd = dims();
        const avail = Math.max(160, sheets.clientWidth - 8);
        // Two portrait pages side by side when there's room, else one.
        const across = pd.w < pd.h && avail > 2 * 260 ? 2 : 1;
        const scale = Math.min((avail - (across - 1) * 16) / across / (pd.w * MM), 0.75);
        fill(sheets, pages.map((pg, i) => h("figure", { class: "sl-thumb" },
          h("div", { class: "sl-paper", style: { width: `${pd.w * MM * scale}px`, height: `${pd.h * MM * scale}px` } },
            h("div", { class: "sl-scale", style: { transform: `scale(${scale})` } }, pg)),
          h("figcaption", { text: `Page ${i + 1} of ${pages.length}` }))));
        const d = data();
        const n = d.sections.reduce((a, x) => a + x.songs.length, 0);
        const split = o.columns * o.rows > 1 ? `, in ${o.rows > 1 ? `${o.columns} × ${o.rows} parts` : `${o.columns} columns`}` : "";
        info.textContent = `${plural(pages.length, "page")}, ${PAPER[o.paper].label} ${o.landscape ? "landscape" : "portrait"}${split}, ${plural(n, "song")}.`;
        hint.textContent = `In the print dialog, choose ${PAPER[o.paper].label} paper, ${o.landscape ? "landscape" : "portrait"}, at 100% size.`;
      }
      function changed() {
        keep();
        if (!raf) raf = requestAnimationFrame(draw);
      }
      const reset = () => {
        Object.assign(o, defaults());
        keep();
        closeModal();
        open(st.id);
      };
      async function print() {
        preparePrint(data());
        const mac = /Mac/i.test(navigator.platform || navigator.userAgent);
        try {
          if (mac && !DEMO) await api.printWindow();
          else window.print();
        } catch (e) {
          toast(`Couldn't print: ${e}`);
        }
      }
      modal({
        title: "Print a setlist",
        wide: true, tall: true, cls: "sl-modal",
        body: [h("div", { class: "sl-dlg" }, controls, preview)],
        buttons: [
          h("button", { type: "button", class: "btn left", text: "Back to the defaults", onclick: reset }),
          h("button", { type: "button", class: "btn", text: "Close", onclick: closeModal }),
          h("button", { type: "button", class: "btn primary", id: "sl-print", text: "Print…", onclick: print }),
        ],
      });
      // Fonts first, so the pages are measured in the font they print in.
      Promise.all(Object.values(FONTS).flatMap((f) => [`${f.bold} 20pt ${f.family}`, `${f.normal} 12pt ${f.family}`]).map((x) => document.fonts.load(x).catch(() => {})))
        .finally(() => draw());
      draw();
    }

    return {
      open, load, preparePrint, paginate, options: o,
      /** A stick's page data (the tests' way in). */
      data: (setId) => pageData(setId, stickSet(setId).name),
    };
  };
})(window.Fatstick = window.Fatstick || {});
