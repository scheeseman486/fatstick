// The window's one door to the outside, in two versions with the same shape:
//
// * In the desktop app, every call is a command of the Rust side (library,
//   ffmpeg, cache, player, sticks) and progress arrives as events.
// * In a plain browser (previews, the UI tests), a demo version serves the
//   built-in demo library and simulates the slow parts: preparation, the
//   sync and the player. It never touches a disk.
"use strict";
(function (P2U) {
  const T = window.__TAURI__;
  const AUDIO_EXT = ["mp3", "mp2", "flac", "m4a", "aac", "mp4", "ogg", "oga", "opus", "wma", "wav", "aif", "aiff", "wv", "ape"];
  const LIST_EXT = ["m3u", "m3u8", "xspf", "pls"];

  function bus() {
    const subs = {};
    return {
      on(name, fn) { (subs[name] = subs[name] || []).push(fn); },
      emit(name, payload) { for (const fn of subs[name] || []) fn(payload); },
    };
  }

  // ---- the desktop app ----------------------------------------------------------
  function tauriApi() {
    const invoke = T.core.invoke;
    return {
      kind: "app",
      init: () => invoke("init"),
      saveDocs: (kind, put, del, order) => invoke("save_docs", { kind, put, delete: del, order }),
      setSetting: (key, value) => invoke("set_setting", { key, value: String(value) }),
      addPaths: (paths) => invoke("add_paths", { paths }),
      async pickFiles({ folder = false, playlists = false } = {}) {
        const filters = folder ? undefined : playlists
          ? [{ name: "Playlists", extensions: LIST_EXT }]
          : [{ name: "Audio and playlists", extensions: [...AUDIO_EXT, ...LIST_EXT] }];
        const r = await T.dialog.open({ multiple: !folder, directory: folder, filters });
        return r == null ? [] : Array.isArray(r) ? r : [r];
      },
      async pickFile(title) {
        const r = await T.dialog.open({ multiple: false, title, filters: [{ name: "Audio", extensions: AUDIO_EXT }] });
        return r || null;
      },
      pickSavePath: (defaultPath) => T.dialog.save({ defaultPath, filters: [{ name: "M3U8 playlist", extensions: ["m3u8"] }, { name: "XSPF playlist (VLC)", extensions: ["xspf"] }] }),
      exportPlaylist: (trackIds, name, path, relative) => invoke("export_playlist", { trackIds, name, path, relative }),
      locate: (trackId, path) => invoke("locate", { trackId, path }),
      refreshTracks: () => invoke("refresh_tracks"),
      analyze: (trackIds) => invoke("analyze", { trackIds }),
      prepareQueue: (jobs) => invoke("prepare_queue", { jobs }),
      cacheInfo: () => invoke("cache_info"),
      clearCache: () => invoke("clear_cache"),
      listSticks: () => invoke("list_sticks"),
      sync: (plan) => invoke("sync", { plan }),
      cancelSync: () => invoke("cancel_sync"),
      play: (args) => invoke("player_play", { args }),
      pause: () => invoke("player_pause"),
      resume: () => invoke("player_resume"),
      seek: (pos) => invoke("player_seek", { pos }),
      stop: () => invoke("player_stop"),
      volume: (volume) => invoke("player_volume", { volume }),
      about: () => invoke("about"),
      on(name, fn) { T.event.listen(name, (e) => fn(e.payload)); },
      onFileDrop(cb) {
        const w = T.webviewWindow && T.webviewWindow.getCurrentWebviewWindow();
        if (!w) return;
        w.onDragDropEvent((e) => {
          const p = e.payload;
          const dpr = window.devicePixelRatio || 1;
          cb({ type: p.type, paths: p.paths || [], pos: p.position ? { x: p.position.x / dpr, y: p.position.y / dpr } : null });
        });
      },
    };
  }

  // ---- the demo ---------------------------------------------------------------------
  function demoApi() {
    const D = P2U.demo;
    const ev = bus();
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const tracks = new Map(D.tracks.map((t) => [t.id, t]));
    const sticks = D.sticks.map((s) => ({ id: s.id, device: s.device, name: s.name, sizeBytes: s.size_bytes, bus: s.bus, volumes: s.volumes.map((v) => v.summary), image: false, note: s.note || null, serials: [] }));
    // Tracks the demo's car stick got at its last sync count as prepared already.
    const synced = new Set(D.stickSets.flatMap((s) => (s.lastSync ? s.lastSync.content.flat() : [])));
    const ready = new Set();
    let queue = [];
    let timer = null;
    let cancel = false;

    function work() {
      clearTimeout(timer);
      const j = queue.shift();
      if (!j) return;
      const t = tracks.get(j.trackId);
      const ms = j.recipe.kind === "gain" ? 150 : Math.min(1200, 250 + (t ? t.duration : 60) * 2);
      timer = setTimeout(() => {
        ready.add(j.key);
        ev.emit("prepared", { key: j.key, ok: true, error: null, bytes: 0 });
        work();
      }, ms);
    }

    // The silent player: demo tracks have no audio; dropped files play for real.
    const P = { audio: null, timer: null, token: "", pos: 0, dur: 0, playing: false };
    function pev(state, error) {
      ev.emit("player", { state, token: P.token, pos: P.pos, dur: P.dur, error: error || null });
    }
    function stopAudio() {
      clearInterval(P.timer);
      if (P.audio) { P.audio.pause(); P.audio.removeAttribute("src"); P.audio = null; }
    }

    return {
      kind: "demo",
      async init() {
        const syncs = D.stickSets.filter((s) => s.lastSync).map((s) => ({ setId: s.id, ...s.lastSync, demo: true }));
        return {
          tracks: D.tracks,
          playlists: D.playlists,
          profiles: D.profiles,
          sets: D.stickSets.map(({ lastSync, ...s }) => s),
          syncs,
          settings: {},
          ffmpeg: "demo",
          ffmpegError: null,
          cache: { files: 0, bytes: 0, dir: "" },
          dataDir: "",
          version: "demo",
          libraryId: D.libraryId,
          openPaths: [],
        };
      },
      async saveDocs() {},
      async setSetting() {},
      async addFileObjects(files) {
        const ts = [];
        for (const f of files) {
          if (!P2U.sniff.isAudio(f.name)) continue;
          const t = await P2U.sniff.fromFile(f, `t${Math.random().toString(36).slice(2, 9)}`);
          if (t) { tracks.set(t.id, t); ts.push(t); }
        }
        return ts;
      },
      async exportPlaylist() {},
      async locate(trackId) {
        const t = tracks.get(trackId);
        if (t) { t.exists = true; t.path = t.path.replace("~/Music/", "~/Music/Old library/"); }
        return { track: t, matches: [] };
      },
      async refreshTracks() { return [...tracks.values()]; },
      async analyze(ids) {
        for (const id of ids) {
          const t = tracks.get(id);
          // Dropped files get a plausible measurement; the app uses ffmpeg's ebur128.
          if (t && t.lufs == null) { t.lufs = -11; t.truePeak = -0.5; }
          setTimeout(() => ev.emit("analyzed", { trackId: id, lufs: t ? t.lufs : null, truePeak: t ? t.truePeak : null, error: null }), 300);
        }
      },
      async prepareQueue(jobs) {
        const done = [];
        queue = [];
        for (const j of jobs) {
          if (ready.has(j.key) || synced.has(j.trackId)) { ready.add(j.key); done.push(j.key); } else queue.push(j);
        }
        work();
        return done;
      },
      async cacheInfo() { return { files: ready.size, bytes: ready.size * 9e6, dir: "" }; },
      async clearCache() { ready.clear(); synced.clear(); return { files: 0, bytes: 0, dir: "" }; },
      async listSticks() { return { sticks, error: null }; },
      async sync(plan) {
        cancel = false;
        const say = (stage, done, total, message, name = "") => ev.emit("sync-progress", { stage, done, total, name, message });
        const files = plan.folders.flatMap((f) => f.files);
        const stopped = (wrote) => {
          const dev = sticks.find((s) => s.id === plan.stickId);
          if (wrote && dev) dev.volumes = [];
          return { ok: false, cancelled: true, erased: wrote, error: wrote ? "Cancelled while writing. The stick is blank now; sync again to fill it." : "Cancelled. Nothing was written; the stick is as it was.", notes: [] };
        };
        for (let i = 0; i < files.length; i++) {
          if (cancel) return stopped(false);
          const k = files[i];
          if (k.recipe.kind !== "copy" && !ready.has(k.key)) await sleep(60);
          say("prepare", i + 1, files.length, `Prepared ${i + 1} of ${files.length}: ${k.name}`, k.name);
          await sleep(15);
        }
        say("tags", files.length, files.length, "Writing tags.");
        await sleep(300);
        say("check", 1, 1, "Laying out every cluster.");
        await sleep(400);
        if (cancel) return stopped(false);
        say("erase", 0, 1, "Erasing the stick.");
        await sleep(700);
        const total = plan.estimateBytes || files.length * 9e6;
        const speed = 15e6 * 20; // twenty times a typical stick, so the demo doesn't drag
        for (let done = 0; done < total;) {
          if (cancel) return stopped(true);
          done = Math.min(total, done + speed / 10);
          const i = Math.min(files.length - 1, Math.floor((done / total) * files.length));
          say("write", done, total, `Writing ${i + 1} of ${files.length}: ${files[i].name}`, files[i].name);
          await sleep(100);
        }
        for (let done = 0; done < total;) {
          done = Math.min(total, done + (speed * 2) / 10);
          say("verify", done, total, "Reading back and comparing every byte.");
          await sleep(100);
        }
        say("eject", 0, 1, "Ejecting the stick.");
        await sleep(500);
        const dev = sticks.find((s) => s.id === plan.stickId);
        const label = P2U.names.volumeLabel(plan.label || plan.setName);
        if (dev) dev.volumes = [`${label || "no label"} (FAT32), ${plan.folders.length > 1 || plan.folders[0].name ? `${plan.folders.length} folder${plan.folders.length === 1 ? "" : "s"}, ` : ""}${files.length} files`];
        if (dev) dev.note = { app: "Playlist2USB", format: 1, libraryId: D.libraryId, setId: plan.setId, setName: plan.setName, ...plan.note, files: files.length, syncedAt: Date.now() };
        const record = { ...plan.record, setId: plan.setId, at: Date.now(), stickId: plan.stickId, stickName: dev ? dev.name : "", files: files.length, bytes: total };
        return { ok: true, cancelled: false, erased: true, error: null, notes: ["Demo: nothing was written."], files: files.length, bytes: total, ejected: true, record };
      },
      async cancelSync() { cancel = true; },
      async play({ trackId, token, from = 0 }) {
        stopAudio();
        const t = tracks.get(trackId);
        Object.assign(P, { token, pos: from, dur: t ? t.duration : 0, playing: true });
        if (t && t.url) {
          P.audio = new Audio(t.url);
          P.audio.currentTime = from;
          P.audio.addEventListener("timeupdate", () => { if (P.audio) { P.pos = P.audio.currentTime; P.dur = P.audio.duration || P.dur; pev("playing"); } });
          P.audio.addEventListener("ended", () => { stopAudio(); pev("ended"); });
          P.audio.play().catch(() => pev("error", "This browser can't play this format; the app plays everything through its own ffmpeg."));
        } else {
          P.timer = setInterval(() => {
            if (!P.playing) return;
            P.pos += 0.25;
            if (P.pos >= P.dur) { stopAudio(); pev("ended"); return; }
            pev("playing");
          }, 250);
        }
        pev("playing");
        return { from: "source", silent: !(t && t.url) };
      },
      async pause() { P.playing = false; if (P.audio) P.audio.pause(); pev("paused"); },
      async resume() { P.playing = true; if (P.audio) P.audio.play().catch(() => {}); pev("playing"); },
      async seek(pos) { P.pos = pos; if (P.audio) P.audio.currentTime = pos; pev(P.playing ? "playing" : "paused"); },
      async stop() { stopAudio(); P.playing = false; P.pos = 0; pev("stopped"); },
      async volume(v) { if (P.audio) P.audio.volume = v; },
      async about() { return { version: "demo", ffmpeg: null, dataDir: "", cache: await this.cacheInfo() }; },
      on: ev.on,
    };
  }

  P2U.api = T && T.core ? tauriApi() : demoApi();
  P2U.isAudioPath = (p) => AUDIO_EXT.includes((p.split(".").pop() || "").toLowerCase());
})(window.P2U = window.P2U || {});
