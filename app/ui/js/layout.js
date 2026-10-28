// A stick's layout in a window of its own: every folder and every file in
// the order a player reads them, folders and joined tracks collapsible. The
// main window plans the stick and sends the plan whenever it changes; this
// window only draws it.
"use strict";
(function (Fatstick) {
  const { h, fill, clock, length, bytes, plural } = Fatstick.ui;
  const link = Fatstick.link;
  const setId = new URLSearchParams(location.search).get("set") || "";
  const root = document.getElementById("lay");
  const storeKey = `fatstick-layout-closed:${setId}`;
  let closed = new Set();
  try { closed = new Set(JSON.parse(localStorage.getItem(storeKey) || "[]")); } catch { /* a fresh window */ }
  const remember = () => { try { localStorage.setItem(storeKey, JSON.stringify([...closed])); } catch { /* not kept */ } };
  let last = null;

  function theme(t) {
    const r = document.documentElement;
    if (t === "light" || t === "dark") r.setAttribute("data-theme", t);
    else r.removeAttribute("data-theme");
  }

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
      h("span", { class: `lay-how${f.how === "Copy" || f.how.startsWith("Copy") ? "" : " enc"}`, text: f.how }),
      h("span", { class: "lay-size", text: bytes(f.bytes) }));
    if (!f.parts) return row;
    // A joined track: the tracks inside, with where each starts.
    const key = `${folderKey}/${f.name}`;
    const inner = h("details", { class: "lay-parts", open: !closed.has(key) || null },
      h("summary", { text: `${plural(f.parts.length, "track")} inside, in this order` }),
      h("ol", null, f.parts.map((p) => h("li", null,
        h("span", { class: "lay-at", text: clock(p.at) }),
        h("span", { class: "lay-name" }, h("span", { class: "nm", text: p.title }),
          h("span", { class: "sub", text: [p.artist, p.gain ? `Gain ${p.gain}` : "", p.edges].filter(Boolean).join(". ") })),
        h("span", { class: "lay-len", text: clock(p.secs) })))));
    inner.addEventListener("toggle", () => { if (inner.open) closed.delete(key); else closed.add(key); remember(); });
    row.append(inner);
    return row;
  }

  function folderBlock(f, i, single) {
    const key = f.folder || "(top)";
    const kindText = f.kind === "silence"
      ? `${f.secs < 60 ? `${Math.round(f.secs)} s` : clock(f.secs)} of silence`
      : f.kind === "joined"
        ? `${plural(f.tracks, "track")} joined into one file, ${length(f.secs)}, ${bytes(f.bytes)}`
        : `${plural(f.files.length, "file")}, ${length(f.secs)}, ${bytes(f.bytes)}`;
    const d = h("details", { class: `lay-folder k-${f.kind}`, open: !closed.has(key) || null, dataset: { folder: key } },
      h("summary", null,
        h("span", { class: "lay-fname", text: single ? "Top level of the stick" : `${f.folder}/` }),
        f.kind !== "silence" && f.playlist && !(f.folder || "").includes(f.playlist) ? h("span", { class: "lay-pl", text: f.playlist }) : null,
        h("span", { class: "lay-fmeta", text: kindText })),
      h("ol", { class: "lay-files" }, f.files.map((x) => fileRow(x, key))),
      f.skipped.length ? h("p", { class: "st-note warn lay-skip", text: `Not on the stick: ${f.skipped.join("; ")}.` }) : null);
    d.addEventListener("toggle", () => { if (d.open) closed.delete(key); else closed.add(key); remember(); });
    return d;
  }

  function render(d) {
    last = d;
    theme(d.theme);
    if (d.gone) {
      document.title = "Stick layout";
      fill(root, h("p", { class: "st-note", text: "This stick isn't in the list any more." }));
      return;
    }
    document.title = `Layout of ${d.name}`;
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
    fill(root,
      h("header", { class: "lay-head" },
        h("div", { class: "lay-title" },
          h("h1", { text: d.name }),
          h("span", { class: `pill ${d.status.kind === "ok" ? "ok" : d.status.kind === "out" ? "out" : ""}`, text: d.status.kind === "ok" ? "Up to date" : d.status.kind === "out" ? "Out of date" : "New" })),
        h("p", { class: "lay-sub", text: `${d.profile} profile. Label ${d.label || "none"}. ${where}` }),
        h("p", { class: `lay-status ${d.status.kind}`, text: d.status.text }),
        h("div", { class: "lay-bar" },
          h("p", { class: `lay-sum${d.space && !d.space.fits ? " bad" : ""}`, text: `${plural(d.folders.length, d.single ? "playlist" : "folder")}, ${plural(files, "file")}, ${length(secs)}${space}` }),
          h("span", { class: "tools" },
            h("button", { type: "button", class: "btn small", text: "Expand all", onclick: all(true) }),
            h("button", { type: "button", class: "btn small", text: "Collapse all", onclick: all(false) })))),
      d.errors && d.errors.length ? h("p", { class: "st-note err", text: `The file names don't work: ${d.errors[0]}` }) : null,
      d.folders.length
        ? h("div", { class: "lay-folders" }, d.folders.map((f, i) => folderBlock(f, i, d.single)))
        : h("p", { class: "st-note", text: "Nothing on this stick yet. Add a playlist to it in the main window." }),
      d.ignored && d.ignored.length ? h("p", { class: "st-note warn", text: `Not written, as this profile puts one playlist on a stick: ${d.ignored.join(", ")}.` }) : null,
      h("p", { class: "lay-foot", text: "The order here is the order a player reads the stick in: folders, then files, top to bottom." }),
    );
    link.emit("layout:rendered", { setId, folders: d.folders.length, files, joined: d.folders.filter((f) => f.kind === "joined").length, silences: d.folders.filter((f) => f.kind === "silence").length });
  }

  link.on("layout:data", (d) => { if (d && d.setId === setId) render(d); });
  link.emit("layout:want", { setId });
  // Ask again until the main window answers (it may still be starting).
  const retry = setInterval(() => { if (last) clearInterval(retry); else link.emit("layout:want", { setId }); }, 1000);
  window.addEventListener("beforeunload", () => link.emit("layout:gone", { setId }));
})(window.Fatstick = window.Fatstick || {});
