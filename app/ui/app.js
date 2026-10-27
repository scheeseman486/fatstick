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
  volumes: [],
  volumeId: null,
  lastCopy: null, // { volume_path, order }
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
  state.lastCopy = null;
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
    state.lastCopy = null;
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

// ---- volumes -------------------------------------------------------------------
function renderVolumes(errorText) {
  const box = $("volumes");
  box.replaceChildren();
  if (errorText || state.volumes.length === 0) {
    const p = document.createElement("p");
    p.className = "vol-msg";
    p.textContent = errorText || "No FAT32 stick found. Plug one in, then select Refresh.";
    box.append(p);
  }
  for (const v of state.volumes) {
    const row = document.createElement("label");
    row.className = "vol" + (v.id === state.volumeId ? " selected" : "");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = "vol";
    radio.checked = v.id === state.volumeId;
    radio.addEventListener("change", () => {
      state.volumeId = v.id;
      state.lastCopy = null;
      renderVolumes();
      updateButtons();
    });
    const name = document.createElement("span");
    name.className = "v-name";
    name.textContent = v.label || baseName(v.device) || "Untitled";
    const meta = document.createElement("span");
    meta.className = "v-meta";
    meta.textContent = [fmtSize(v.size_bytes), v.fs_type, v.mount_point || "not mounted"].filter(Boolean).join(", ");
    row.append(radio, name, meta);
    box.append(row);
  }
}

async function refreshVolumes() {
  try {
    state.volumes = await invoke("list_volumes");
    if (!state.volumes.some((v) => v.id === state.volumeId)) {
      state.volumeId = state.volumes.length === 1 ? state.volumes[0].id : null;
    }
    renderVolumes();
  } catch (err) {
    state.volumes = [];
    state.volumeId = null;
    renderVolumes(`Couldn't list drives: ${err}`);
  }
  updateButtons();
}
$("refresh").addEventListener("click", refreshVolumes);

function updateButtons() {
  const ready = !state.busy && state.tracks.some((t) => t.exists) && state.volumeId;
  $("copy").disabled = !ready;
  $("lock").disabled = !(ready && state.lastCopy);
  $("check").disabled = !(ready && state.lastCopy);
  $("open").disabled = state.busy;
}

// ---- copy, lock, check --------------------------------------------------------------
T.event.listen("copy-progress", (e) => {
  const p = e.payload;
  const n = String(p.index + 1).padStart(2, "0");
  lcd(`COPY ${n}-${String(p.total).padStart(2, "0")}`, undefined, `Copying ${p.name}`);
});

async function run(label, fn) {
  state.busy = true;
  updateButtons();
  setStatus("");
  try {
    await fn();
  } catch (err) {
    lcd("ERROR", undefined, label);
    setStatus(String(err), true);
  } finally {
    state.busy = false;
    updateButtons();
  }
}

$("copy").addEventListener("click", () =>
  run("Copy failed", async () => {
    const tracks = state.tracks.filter((t) => t.exists).map((t) => ({ path: t.path, title: t.title }));
    const res = await invoke("copy_playlist", {
      req: {
        name: state.playlist.name,
        tracks,
        volume_id: state.volumeId,
        folder: $("folder").value.trim() || state.playlist.name,
        number_prefix: $("numbered").checked,
        ascii_names: $("ascii").checked,
        replace: $("replace").checked,
      },
    });
    state.lastCopy = { volume_path: res.plan.volume_path, order: res.plan.items.map((i) => i.target_name) };
    const ok = res.report.check.in_order;
    lcd(ok ? "COPIED IN ORDER" : "COPIED", res.report.files_copied,
      ok ? "The copy came out in playlist order. Lock the order to make sure no player sees otherwise."
         : "Copied, but the stick lists the files in a different order. Lock the order to fix it.");
    setStatus(`Copied ${res.report.files_copied} files to ${res.plan.folder_name}.`);
  }));

$("lock").addEventListener("click", () =>
  run("Lock failed", async () => {
    lcd("LOCKING", undefined, "Waiting for your system's authorization…");
    const res = await invoke("lock_order", {
      req: { volume_id: state.volumeId, volume_path: state.lastCopy.volume_path, order: state.lastCopy.order },
    });
    const o = res.outcome;
    if (!o.ok) throw new Error(o.error || "The stick's order does not match after locking.");
    lcd("ORDER LOCKED", o.check ? o.check.found : undefined,
      o.report && o.report.changed ? "Folder table rewritten and read back: it matches the playlist."
                                   : "The folder table already matched the playlist; nothing was written.");
    setStatus(o.backup_file ? `Undo backup saved: ${o.backup_file}` : "No changes were needed.");
    await refreshVolumes();
  }));

$("check").addEventListener("click", () =>
  run("Check failed", async () => {
    const c = await invoke("check_mounted_order", {
      volumeId: state.volumeId, volumePath: state.lastCopy.volume_path, order: state.lastCopy.order,
    });
    if (c.in_order) {
      lcd("IN ORDER", c.found, "The stick lists every track in playlist order.");
      setStatus(c.junk.length ? `${c.junk.length} hidden metadata files are in the folder.` : "");
    } else {
      lcd("OUT OF ORDER", c.found, "Lock the order to fix it.");
      setStatus(c.missing.length ? `${c.missing.length} tracks are missing from the stick.` : "The tracks are present but out of order.", true);
    }
  }));

// ---- start -------------------------------------------------------------------------
T.webviewWindow.getCurrentWebviewWindow().onDragDropEvent((e) => {
  if (e.payload.type === "drop" && e.payload.paths.length) {
    const p = e.payload.paths.find((x) => /\.(m3u8?|xspf|pls)$/i.test(x));
    if (p) loadPlaylist(p);
    else setStatus("Drop an .m3u, .m3u8, .xspf or .pls file.", true);
  }
});

renderTracks();
refreshVolumes();
invoke("startup_playlist").then((p) => p && loadPlaylist(p));
