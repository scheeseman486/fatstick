// The front end's one door to the outside. In a browser it serves the demo
// data; inside the Tauri app it lists the real USB sticks and imports real
// playlist files through the existing Rust commands. Syncing is simulated in
// both for now: the library, converter and multi-folder writer it needs are
// the next build steps in the design doc.
"use strict";
(function (P2U) {
  const T = window.__TAURI__;
  const AUDIO_EXT = ["mp3", "flac", "m4a", "aac", "ogg", "opus", "wma", "wav", "aif", "aiff", "wv", "ape"];
  const LIST_EXT = ["m3u", "m3u8", "xspf", "pls"];

  function summary(v) {
    const fs = { vfat: "FAT32", exfat: "exFAT", ntfs: "NTFS", ext4: "ext4" }[v.fs_type] || v.fs_type || "unknown format";
    return `${v.label || "no label"} (${fs})${v.mount_point ? `, mounted at ${v.mount_point}` : ""}`;
  }

  // The same objects every time, so a simulated sync's result stays on the stick.
  const demo = {
    kind: "demo",
    async sticks() {
      return P2U.demo.sticks;
    },
  };
  let demoInApp = null;

  const tauri = T && T.core
    ? {
        kind: "tauri",
        async sticks() {
          let real = [];
          try {
            real = await T.core.invoke("list_sticks");
          } catch (e) {
            console.warn("list_sticks failed", e);
          }
          const mine = real.map((s) => ({ ...s, real: true, volumes: s.volumes.map((v) => ({ ...v, summary: summary(v) })) }));
          demoInApp = demoInApp || P2U.demo.sticks.map((s) => ({ ...s, name: `${s.name} (demo)` }));
          return [...mine, ...demoInApp];
        },
        async pickFiles(folder) {
          const r = await T.dialog.open({
            multiple: !folder,
            directory: !!folder,
            filters: folder ? undefined : [{ name: "Audio and playlists", extensions: [...AUDIO_EXT, ...LIST_EXT] }],
          });
          return r == null ? [] : Array.isArray(r) ? r : [r];
        },
        async loadPlaylist(path) {
          return T.core.invoke("load_playlist", { path });
        },
        onFileDrop(cb) {
          const w = T.webviewWindow && T.webviewWindow.getCurrentWebviewWindow();
          if (!w) return;
          w.onDragDropEvent((e) => {
            const p = e.payload;
            const dpr = window.devicePixelRatio || 1;
            const pos = p.position ? { x: p.position.x / dpr, y: p.position.y / dpr } : null;
            cb({ type: p.type, paths: p.paths || [], pos });
          });
        },
        isPlaylistPath: (p) => LIST_EXT.includes((p.split(".").pop() || "").toLowerCase()),
        isAudioPath: (p) => AUDIO_EXT.includes((p.split(".").pop() || "").toLowerCase()),
      }
    : null;

  P2U.backend = tauri || demo;
})(window.P2U = window.P2U || {});
