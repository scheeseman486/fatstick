// Playlist2USB front end: plain DOM, no framework, talks to the Rust side via
// window.__TAURI__ (withGlobalTauri). Text from playlists is only ever inserted
// with textContent.
"use strict";

const T = window.__TAURI__;
const invoke = T.core.invoke;
const $ = (id) => document.getElementById(id);

const state = {
  playlist: null, // { name, path, tracks: [...] }
  tracks: [],
  sticks: [],
  stickId: null,
  busy: false,
};

// ---- segment display -------------------------------------------------------
const LCD_WIDTH = 14;
function lcd(text, count, sub) {
  const clean = (s) => s.toUpperCase().replace(/[^A-Z0-9 \-]/g, " ");
  $("lcd-state").textContent = clean(text).slice(0, LCD_WIDTH).padEnd(LCD_WIDTH, " ");
  if (count !== undefined) $("lcd-count").textContent = count === null ? "---" : String(count).padStart(3, " ");
  if (sub !== undefined) $("lcd-sub").textContent = sub;
}

function setStatus(msg, isError = false) {
  const el = $("status");
  el.textContent = msg;
  el.classList.toggle("error", isError);
}

function fmtDur(s) {
  if (s == null || s < 0) return "";
  s = Math.round(s);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function fmtSize(b) {
  if (!b) return "";
  const g = b / 1024 ** 3;
  return g >= 1 ? `${g.toFixed(1)} GB` : `${Math.round(b / 1024 ** 2)} MB`;
}

function baseName(p) {
  return p.split(/[\\/]/).pop();
}

// ---- track list --------------------------------------------------------------
function renderTracks() {
  const list = $("list");
  list.replaceChildren();
  $("empty").hidden = state.tracks.length > 0;
  state.tracks.forEach((t, i) => {
    const li = document.createElement("li");
    li.tabIndex = 0;
    li.draggable = true;
    li.dataset.index = i;
    if (!t.exists) li.classList.add("missing");
    const main = document.createElement("div");
    main.className = "t-title";
    main.textContent = t.title;
    const file = document.createElement("span");
    file.className = "t-file";
    file.textContent = t.exists ? baseName(t.path) : `File not found: ${t.path}`;
    main.append(file);
    const dur = document.createElement("span");
    dur.className = "t-dur";
    dur.textContent = t.exists ? fmtDur(t.duration_secs) : "missing";
    li.append(main, dur);
    list.append(li);
  });
  const present = state.tracks.filter((t) => t.exists).length;
  const missing = state.tracks.length - present;
  const total = state.tracks.reduce((a, t) => a + (t.duration_secs || 0), 0);
  $("pl-meta").textContent = state.playlist
    ? `${present} tracks${total ? `, ${Math.round(total / 60)} min` : ""}${missing ? `, ${missing} missing (skipped)` : ""}`
    : "M3U, M3U8, XSPF (VLC) and PLS playlists work.";
  updateButtons();
}

function moveTrack(from, to) {
  if (to < 0 || to >= state.tracks.length || from === to) return;
  const [t] = state.tracks.splice(from, 1);
  state.tracks.splice(to, 0, t);
  renderTracks();
  const li = $("list").children[to];
  if (li) li.focus();
}

let dragFrom = null;
$("list").addEventListener("dragstart", (e) => {
  const li = e.target.closest("li");
  dragFrom = Number(li.dataset.index);
  li.classList.add("dragging");
  e.dataTransfer.effectAllowed = "move";
});
$("list").addEventListener("dragend", () => {
  dragFrom = null;
  for (const li of $("list").children) li.classList.remove("dragging", "drop-before", "drop-after");
});
$("list").addEventListener("dragover", (e) => {
  const li = e.target.closest("li");
  if (dragFrom === null || !li) return;
  e.preventDefault();
  const after = e.offsetY > li.offsetHeight / 2;
  for (const x of $("list").children) x.classList.remove("drop-before", "drop-after");
  li.classList.add(after ? "drop-after" : "drop-before");
});
$("list").addEventListener("drop", (e) => {
  const li = e.target.closest("li");
  if (dragFrom === null || !li) return;
  e.preventDefault();
  let to = Number(li.dataset.index) + (e.offsetY > li.offsetHeight / 2 ? 1 : 0);
  if (to > dragFrom) to -= 1;
  moveTrack(dragFrom, to);
});
$("list").addEventListener("keydown", (e) => {
  const li = e.target.closest("li");
  if (!li || !e.altKey) return;
  const i = Number(li.dataset.index);
  if (e.key === "ArrowUp") { e.preventDefault(); moveTrack(i, i - 1); }
  if (e.key === "ArrowDown") { e.preventDefault(); moveTrack(i, i + 1); }
});

// ---- playlists -----------------------------------------------------------------
async function loadPlaylist(path) {
  try {
    const pl = await invoke("load_playlist", { path });
    state.playlist = pl;
    state.tracks = pl.tracks;
    $("pl-name").textContent = pl.name;
    $("folder").value = pl.name;
    renderTracks();
    const present = pl.tracks.filter((t) => t.exists).length;
    lcd("READY", present, `${pl.name}: ${present} tracks ready.${pl.skipped.length ? ` ${pl.skipped.length} stream URLs skipped.` : ""}`);
    setStatus("");
  } catch (err) {
    setStatus(`Couldn't read that playlist: ${err}`, true);
  }
}

$("open").addEventListener("click", async () => {
  const path = await T.dialog.open({
    multiple: false,
    directory: false,
    filters: [{ name: "Playlists", extensions: ["m3u", "m3u8", "xspf", "pls"] }],
  });
  if (path) loadPlaylist(path);
});

// ---- sticks --------------------------------------------------------------------
function describeContents(st) {
  if (!st.volumes.length) return "Nothing recognisable on it";
  return "On it now: " + st.volumes
    .map((v) => `${v.label || "no label"} (${v.fs_type || "unknown"}${v.mount_point ? ", mounted" : ""})`)
    .join(", ");
}

function renderSticks(errorText) {
  const box = $("sticks");
  box.replaceChildren();
  if (errorText || state.sticks.length === 0) {
    const p = document.createElement("p");
    p.className = "stk-msg";
    p.textContent = errorText || "No USB stick found. Plug one in, then select Refresh.";
    box.append(p);
  }
  for (const st of state.sticks) {
    const row = document.createElement("label");
    row.className = "stk" + (st.id === state.stickId ? " selected" : "");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = "stick";
    radio.checked = st.id === state.stickId;
    radio.addEventListener("change", () => {
      state.stickId = st.id;
      renderSticks();
      updateButtons();
    });
    const name = document.createElement("span");
    name.className = "s-name";
    name.textContent = st.name || "USB stick";
    const meta = document.createElement("span");
    meta.className = "s-meta";
    meta.textContent = [fmtSize(st.size_bytes), st.device].filter(Boolean).join(", ");
    const on = document.createElement("span");
    on.className = "s-on";
    on.textContent = describeContents(st);
    row.append(radio, name, meta, on);
    box.append(row);
  }
}

async function refreshSticks() {
  try {
    state.sticks = await invoke("list_sticks");
    if (!state.sticks.some((s) => s.id === state.stickId)) {
      state.stickId = state.sticks.length === 1 ? state.sticks[0].id : null;
    }
    renderSticks();
  } catch (err) {
    state.sticks = [];
    state.stickId = null;
    renderSticks(`Couldn't list USB sticks: ${err}`);
  }
  updateButtons();
}
$("refresh").addEventListener("click", refreshSticks);

function selectedStick() {
  return state.sticks.find((s) => s.id === state.stickId);
}

function updateButtons() {
  const st = selectedStick();
  const present = state.tracks.filter((t) => t.exists);
  const need = present.reduce((a, t) => a + t.size, 0);
  // Rough: FAT32 tables and cluster slack take a little more than the files.
  const fits = !st || need * 1.01 + 8 * 1024 ** 2 <= st.size_bytes;
  const fit = $("fit");
  fit.textContent = st && present.length ? `${fmtSize(need) || "0 MB"} of ${fmtSize(st.size_bytes)}` : "";
  fit.classList.toggle("error", !fits);
  if (!fits) fit.textContent = `Too big: ${fmtSize(need)} won't fit on ${fmtSize(st.size_bytes)}`;
  $("write").disabled = state.busy || !present.length || !st || !fits;
  $("open").disabled = state.busy;
  $("refresh").disabled = state.busy;
}

// ---- write and check ------------------------------------------------------------
T.event.listen("write-progress", (e) => {
  const p = e.payload;
  if (p.stage === "erase") {
    lcd("ERASING", undefined, "Clearing the stick…");
    return;
  }
  const n = String(p.index + 1).padStart(2, "0");
  const word = p.stage === "verify" ? "CHECK" : "WRITE";
  const pct = p.bytes_total ? ` ${Math.round((100 * p.bytes_done) / p.bytes_total)}%` : "";
  lcd(`${word} ${n}-${String(p.total).padStart(2, "0")}`, undefined, `${p.stage === "verify" ? "Reading back" : "Writing"} ${p.name}${pct}`);
});

async function run(label, fn) {
  state.busy = true;
  updateButtons();
  setStatus("");
  try {
    await fn();
  } catch (err) {
    lcd("ERROR", undefined, label);
    setStatus(String((err && err.message) || err), true);
  } finally {
    state.busy = false;
    updateButtons();
  }
}

function request() {
  return {
    name: state.playlist.name,
    tracks: state.tracks.filter((t) => t.exists).map((t) => ({ path: t.path, title: t.title })),
    stick_id: state.stickId,
    folder: $("folder").value.trim(),
    number_prefix: $("numbered").checked,
    ascii_names: $("ascii").checked,
    verify_data: true,
  };
}

async function confirmErase(st) {
  const what = `${st.name || "USB stick"} (${fmtSize(st.size_bytes)}, ${st.device})`;
  const msg = `Erase ${what} and write "${state.playlist.name}" to it?\n\n${describeContents(st)}.\n\nEverything on the stick will be deleted. This can't be undone.`;
  return T.dialog.ask(msg, { title: "Erase the stick?", kind: "warning", okLabel: "Erase and write", cancelLabel: "Cancel" });
}

$("write").addEventListener("click", async () => {
  const st = selectedStick();
  if (!st || !(await confirmErase(st))) return;
  run("Write failed", async () => {
    lcd("WAITING", undefined, "Waiting for your system's authorization…");
    const res = await invoke("write_playlist", { req: request() });
    const o = res.outcome;
    const v = o.verification;
    if (!o.ok) {
      const why = o.error
        || (v && v.problems.length ? v.problems[0] : "")
        || (v && v.data_mismatches.length ? `${v.data_mismatches.length} files read back differently.` : "")
        || "The check after writing failed.";
      throw new Error(o.erased ? `${why}` : `Nothing was written: ${why}`);
    }
    const n = o.result.plan.files.length;
    const done = res.ejected ? "The stick has been ejected: unplug it." : (o.error || "Eject the stick before unplugging it.");
    lcd("WRITTEN OK", n, `Read back from the stick: ${n} tracks in FAT order, each in one unbroken block, every byte matches. ${done}`);
    setStatus("");
    await refreshSticks();
  });
});

// ---- start -------------------------------------------------------------------------
T.webviewWindow.getCurrentWebviewWindow().onDragDropEvent((e) => {
  if (e.payload.type === "drop" && e.payload.paths.length) {
    const p = e.payload.paths.find((x) => /\.(m3u8?|xspf|pls)$/i.test(x));
    if (p) loadPlaylist(p);
    else setStatus("Drop an .m3u, .m3u8, .xspf or .pls file.", true);
  }
});

renderTracks();
refreshSticks();
invoke("startup_playlist").then((p) => p && loadPlaylist(p));
