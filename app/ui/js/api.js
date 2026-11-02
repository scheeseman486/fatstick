// The window's one door to the outside, in two versions with the same shape:
//
// * In the desktop app, every call is a command of the Rust side (library,
//   ffmpeg, cache, player, sticks) and progress arrives as events.
// * In a plain browser (previews, the UI tests), a demo version serves the
//   built-in demo library and simulates the slow parts: preparation, the
//   sync and the player. It never touches a disk.
"use strict";
(function (Fatstick) {
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
      readTracks: (trackIds) => invoke("read_tracks", { trackIds }),
      prepareQueue: (jobs) => invoke("prepare_queue", { jobs }),
      cacheInfo: () => invoke("cache_info"),
      clearCache: () => invoke("clear_cache"),
      listSticks: () => invoke("list_sticks"),
      exclusions: () => invoke("exclusions"),
      setExcluded: (id, on) => invoke("set_excluded", { id, on }),
      includeAgain: (entry) => invoke("include_again", { entry }),
      sync: (plan) => invoke("sync", { plan }),
      cancelSync: () => invoke("cancel_sync"),
      play: (args) => invoke("player_play", { args }),
      playParts: (args) => invoke("player_play_parts", { args }),
      queue: (args) => invoke("player_queue", { args }),
      pause: () => invoke("player_pause"),
      playerKey: (pitch, keepFormants) => invoke("player_key", { pitch, keepFormants }),
      playerShape: (x) => invoke("player_shape", x),
      trackPeaks: (trackId) => invoke("track_peaks", { trackId }),
      printWindow: () => invoke("print_window"),
      resume: () => invoke("player_resume"),
      seek: (pos) => invoke("player_seek", { pos }),
      stop: () => invoke("player_stop"),
      volume: (volume) => invoke("player_volume", { volume }),
      about: () => invoke("about"),
      libraryInfo: () => invoke("library_info"),
      librarySetRoot: (path) => invoke("library_set_root", { path }),
      libraryScan: () => invoke("library_scan"),
      libraryForget: (del, hide) => invoke("library_forget", { delete: del, hide }),
      libraryWriteTags: (jobs) => invoke("library_write_tags", { jobs }),
      libraryRename: (jobs) => invoke("library_rename", { jobs }),
      youtubeStatus: () => invoke("youtube_status"),
      youtubeSetup: () => invoke("youtube_setup"),
      youtubeCancelSetup: () => invoke("youtube_cancel_setup"),
      youtubeUpdate: () => invoke("youtube_update"),
      youtubeSearch: (query, limit) => invoke("youtube_search", { query, limit }),
      youtubeFetch: (id) => invoke("youtube_fetch", { id }),
      youtubeCancel: (id) => invoke("youtube_cancel", { id }),
      youtubePlay: ({ id, token, from }) => invoke("youtube_play", { id, token, from }),
      youtubeKeep: (id) => invoke("youtube_keep", { id }),
      youtubeSearchLists: (query, limit) => invoke("youtube_search_lists", { query, limit }),
      youtubePlaylist: (link) => invoke("youtube_playlist", { link }),
      youtubeKeepOnAlbum: (id, slot) => invoke("youtube_keep_on_album", { id, slot }),
      youtubeSetFolder: (path) => invoke("youtube_set_folder", { path }),
      async pickFolder(title) {
        const r = await T.dialog.open({ directory: true, multiple: false, title });
        return r || null;
      },
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
    const D = Fatstick.demo;
    const ev = bus();
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const tracks = new Map(D.tracks.map((t) => [t.id, t]));
    const sticks = D.sticks.map((s) => ({ id: s.id, device: s.device, name: s.name, sizeBytes: s.size_bytes, bus: s.bus, volumes: s.volumes.map((v) => v.summary), image: false, note: s.note || null, serials: [], hwSerial: s.hw_serial || "" }));
    // Devices left out, as the app keeps them (see app/src-tauri/src/exclude.rs).
    let excluded = [];
    const exMatches = (x, d) => {
      if (x.serial && d.hwSerial) return x.serial === d.hwSerial && x.sizeBytes === d.sizeBytes;
      return !!x.name && x.name === d.name && x.sizeBytes === d.sizeBytes;
    };
    const isOut = (d) => excluded.some((x) => exMatches(x, d));
    const exView = () => ({
      devices: sticks.map((d) => ({ ...d, note: null, excluded: isOut(d) })),
      away: excluded.filter((x) => !sticks.some((d) => exMatches(x, d))),
      error: null,
    });
    // Tracks the demo's car setlist got at its last sync count as prepared already.
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

    // The music folder: the demo's ~/Music, "scanned" in a moment. Tag edits
    // and renames change the tracks in memory only.
    const music = { ...D.music };
    const inMusic = (t) => !!music.root && t.path.startsWith(`${music.root}/`);
    function fakeScan() {
      music.scanning = true;
      const files = [...tracks.values()].filter((t) => inMusic(t) && t.exists);
      let done = 0;
      ev.emit("library-progress", { phase: "walk", done: 0, total: 0 });
      const step = () => {
        done = Math.min(files.length, done + 9);
        ev.emit("library-progress", { phase: "read", done, total: files.length });
        if (done < files.length) { setTimeout(step, 60); return; }
        Object.assign(music, { scanning: false, lastScan: Date.now(), last: { files: files.length, changed: [], added: 0, moved: 0, missing: [...tracks.values()].filter((t) => inMusic(t) && !t.exists).length, failed: 0 } });
        ev.emit("library-scanned", { info: { ...music }, tracks: [] });
      };
      setTimeout(step, 200);
    }
    const NUM = { year: 9999, track: 999, disc: 99 };
    function applyEdit(t, e) {
      const next = { ...t };
      for (const [k, v] of Object.entries(e)) {
        if (v == null) continue;
        if (k === "labels") { next.labels = v.map((l) => l.trim().replace(/;/g, ",")).filter(Boolean).filter((l, i, a) => a.findIndex((x) => x.toLowerCase() === l.toLowerCase()) === i); continue; }
        if (k in NUM) {
          const s = String(v).trim();
          if (!s) { next[k] = null; continue; }
          const n = parseInt(s, 10);
          if (!(n > 0 && n <= NUM[k])) throw new Error(`The ${k === "track" ? "track number" : k === "disc" ? "disc number" : "year"} must be a number from 1 to ${NUM[k]}, not "${s}"`);
          next[k] = n;
          continue;
        }
        next[k] = String(v).trim();
      }
      return next;
    }

    // The silent player: demo tracks have no audio; dropped files play for real.
    const P = { audio: null, timer: null, token: "", pos: 0, dur: 0, playing: false, next: null };
    function pev(state, error) {
      ev.emit("player", { state, token: P.token, pos: P.pos, dur: P.dur, error: error || null });
    }
    function stopAudio() {
      clearInterval(P.timer);
      if (P.audio) { P.audio.pause(); P.audio.removeAttribute("src"); P.audio = null; }
    }
    /** The end of what's playing: what's queued, at once, or the end. */
    function reachedEnd() {
      stopAudio();
      const n = P.next;
      if (n) { P.next = null; startItem(n, 0); pev("next"); } else pev("ended");
    }
    /** An item: a dropped file plays for real, anything else silently, for as long as it would. */
    function startItem({ token, seconds = 0, parts = [] }, from) {
      stopAudio();
      const t = parts.length === 1 && parts[0].trackId ? tracks.get(parts[0].trackId) : null;
      Object.assign(P, { token, pos: from, dur: seconds, playing: true });
      if (t && t.url) {
        P.audio = new Audio(t.url);
        P.audio.currentTime = from;
        P.audio.addEventListener("timeupdate", () => { if (P.audio) { P.pos = P.audio.currentTime; P.dur = P.audio.duration || P.dur; pev("playing"); } });
        P.audio.addEventListener("ended", reachedEnd);
        P.audio.play().catch(() => pev("error", "This browser can't play this format; the app plays everything through its own ffmpeg."));
      } else {
        P.timer = setInterval(() => {
          if (!P.playing) return;
          P.pos += 0.25;
          if (P.pos >= P.dur) { reachedEnd(); return; }
          pev("playing");
        }, 250);
      }
    }

    // YouTube, made up: setting up takes a moment, a search answers with a few
    // versions of whatever was typed, and a song "arrives" in a second or so.
    const YT = { ready: false, cancelSetup: false, hits: new Map(), kept: new Map(), cancelled: new Set(), folder: "YouTube", lists: new Map(), albums: new Map() };
    const ytStatus = () => ({
      ytdlp: YT.ready ? { path: "tools/yt-dlp", version: "2026.10.26", managed: true } : null,
      deno: YT.ready ? { path: "tools/deno", version: "2.9.6", managed: true } : null,
      ytdlpOld: false, canGet: true, settingUp: false,
      folder: music.root ? `${music.root}/${YT.folder}` : null,
    });
    const YT_FORMS = [
      ["{q} - Official Audio", "The Night Shift - Topic", 214, 1840233],
      ["{q} (Official Video)", "The Night Shift", 231, 48210554],
      ["{q} (Live at the Roundhouse)", "Roundhouse Sessions", 276, 913402],
      ["{q} (Lyrics)", "Lyric Corner", 214, 6204118],
      ["The Night Shift - {q} (Remastered 2019)", "The Night Shift", 219, 402155],
      ["{q} - Piano Cover", "Keys at Home", 188, 77102],
      ["{q} (Slowed)", "Late Hours", 262, 15033],
      ["{q} - Acoustic Session", "Porch Tapes", 203, 241870],
    ];
    const ytPlain = (q) => q.replace(/\s+/g, " ").trim();
    /** The demo's playlists: some songs, made up from what's looked for. */
    function ytList(id, title, channel, n) {
      const songs = [];
      for (let i = 0; i < n; i++) {
        const song = { id: `${id.slice(-5)}${String(i).padStart(2, "0")}zzzzz`.slice(0, 11), title: `The Night Shift - Song ${i + 1}`, channel, duration: 170 + i * 11, views: 90000 - i * 700 };
        YT.hits.set(song.id, song);
        songs.push(song);
      }
      const list = { id, title, channel, songs };
      YT.lists.set(id, list);
      return list;
    }
    /** A demo song as a track in the library. */
    function ytTrack(id, name, more) {
      const hit = YT.hits.get(id);
      const split = hit.title.includes(" - ") ? hit.title.split(" - ") : [hit.channel.replace(" - Topic", ""), hit.title];
      const [artist, title] = [split[0], split.slice(1).join(" - ").replace(/\s*\((Official (Video|Audio)|Lyrics)\)/, "")];
      return {
        id: `t-${id}`, path: `${music.root}/${YT.folder}/${name ? name(artist, title) : `${artist} - ${title}`}.opus`, filename: `${artist} - ${title}`, title, artist, album: "", albumartist: "", year: null, track: null, disc: null,
        genre: "", composer: "", labels: ["youtube"], codec: "opus", bitrate: 128, peakBitrate: 128, vbr: true, sampleRate: 48000, bits: 0, channels: 2, duration: hit.duration, size: hit.duration * 16000,
        lufs: null, truePeak: null, cover: false, exists: true, quick: false, drm: false, ...more,
      };
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
          music: { ...music },
          openPaths: [],
        };
      },
      async saveDocs() {},
      async setSetting() {},
      async addFileObjects(files) {
        const ts = [];
        for (const f of files) {
          if (!Fatstick.sniff.isAudio(f.name)) continue;
          const t = await Fatstick.sniff.fromFile(f, `t${Math.random().toString(36).slice(2, 9)}`);
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
      async readTracks() {},
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
      async listSticks() { return { sticks: sticks.filter((d) => !isOut(d)), error: null }; },
      async exclusions() { return exView(); },
      async setExcluded(id, on) {
        const d = sticks.find((x) => x.id === id);
        if (!d) throw new Error("That device isn't plugged in any more.");
        excluded = excluded.filter((x) => !exMatches(x, d));
        if (on) excluded.push({ serial: d.hwSerial, name: d.name, sizeBytes: d.sizeBytes, image: "" });
        return exView();
      },
      async includeAgain(entry) {
        excluded = excluded.filter((x) => JSON.stringify(x) !== JSON.stringify(entry));
        return exView();
      },
      async sync(plan) {
        const target = sticks.find((s) => s.id === plan.stickId);
        if (target && isOut(target)) return { ok: false, cancelled: false, erased: false, error: "That device is excluded, so nothing is written to it. Nothing was written.", notes: [] };
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
        const label = Fatstick.names.volumeLabel(plan.label || plan.setName);
        if (dev) dev.volumes = [`${label || "no label"} (FAT32), ${plan.folders.length > 1 || plan.folders[0].name ? `${plan.folders.length} folder${plan.folders.length === 1 ? "" : "s"}, ` : ""}${files.length} files`];
        if (dev) dev.note = { app: "Fatstick", format: 1, libraryId: D.libraryId, setId: plan.setId, setName: plan.setName, ...plan.note, files: files.length, syncedAt: Date.now() };
        const record = { ...plan.record, setId: plan.setId, at: Date.now(), stickId: plan.stickId, stickName: dev ? dev.name : "", files: files.length, bytes: total };
        return { ok: true, cancelled: false, erased: true, error: null, notes: ["Demo: nothing was written."], files: files.length, bytes: total, ejected: true, record };
      },
      async cancelSync() { cancel = true; },
      async play({ trackId, token, from = 0 }) {
        stopAudio();
        P.next = null;
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
      async playParts(args) {
        P.next = null;
        startItem(args, args.from || 0);
        pev("playing");
        return { from: "source" };
      },
      async queue(args) {
        P.next = args || null;
        return args ? { from: "source" } : null;
      },
      async playerKey() {},
      async playerShape() {},
      async printWindow() { window.print(); },
      /** A made-up waveform for the demo: a quiet start, a body that moves
       * with the bars, and a fade at the end. The same for a track each time. */
      async trackPeaks(id) {
        const t = tracks.get(id);
        const step = 0.05;
        const n = Math.max(1, Math.round((t ? t.duration : 180) / step));
        let seed = [...String(id)].reduce((a, c) => (a * 31 + c.charCodeAt(0)) >>> 0, 7);
        const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296);
        const intro = 1.5 + rnd() * 6;
        const outro = 3 + rnd() * 8;
        const peaks = [];
        let swell = 0.7;
        for (let i = 0; i < n; i++) {
          const sec = i * step;
          if (i % 40 === 0) swell = 0.45 + rnd() * 0.5;
          const beat = 0.4 + 0.6 * Math.abs(Math.sin(sec * Math.PI * 2)) ** 3;
          const shape = Math.min(1, sec / intro) * Math.min(1, Math.max(0, (n * step - sec) / outro));
          const lead = sec < 0.4 ? 0 : 1;
          peaks.push(Math.round(255 * Math.min(1, lead * shape * swell * beat * (0.7 + rnd() * 0.3))));
        }
        return { step, peaks };
      },
      async pause() { P.playing = false; if (P.audio) P.audio.pause(); pev("paused"); },
      async resume() { P.playing = true; if (P.audio) P.audio.play().catch(() => {}); pev("playing"); },
      async seek(pos) { P.pos = pos; if (P.audio) P.audio.currentTime = pos; pev(P.playing ? "playing" : "paused"); },
      async stop() { stopAudio(); P.playing = false; P.pos = 0; pev("stopped"); },
      async volume(v) { if (P.audio) P.audio.volume = v; },
      async about() { return { version: "demo", ffmpeg: null, dataDir: "", cache: await this.cacheInfo() }; },
      async libraryInfo() { return { ...music }; },
      async librarySetRoot(path) {
        Object.assign(music, { root: path, watching: true, error: null, last: null, lastScan: null });
        fakeScan();
        return { ...music };
      },
      async libraryForget(del, hide) {
        const removed = [];
        const kept = [];
        for (const id of [...del, ...hide]) {
          const t = tracks.get(id);
          if (!t || t.exists) continue;
          removed.push(id);
          if (hide.includes(id)) { t.forgotten = true; kept.push({ ...t }); } else tracks.delete(id);
        }
        return { removed, kept };
      },
      async libraryScan() { if (music.root && !music.scanning) fakeScan(); },
      async libraryWriteTags(jobs) {
        const out = { tracks: [], failed: [] };
        await sleep(250);
        for (const j of jobs) {
          const t = tracks.get(j.trackId);
          try {
            if (!t || !inMusic(t)) throw new Error("That file isn't in your music folder, so it's left as it is.");
            if (t.codec === "wma") throw new Error("tags can't be written into WMA files");
            const next = applyEdit(t, j.edit);
            tracks.set(t.id, next);
            out.tracks.push(next);
          } catch (e) {
            out.failed.push({ trackId: j.trackId, error: e.message });
          }
        }
        return out;
      },
      async libraryRename(jobs) {
        const out = { tracks: [], failed: [] };
        await sleep(150);
        const taken = new Set([...tracks.values()].map((t) => t.path.toLowerCase()));
        for (const j of jobs) {
          const t = tracks.get(j.trackId);
          const dir = t ? t.path.slice(0, t.path.lastIndexOf("/") + 1) : "";
          const dot = j.name.lastIndexOf(".");
          const ext = (p) => p.slice(p.lastIndexOf(".")).toLowerCase();
          const dst = dir + j.name;
          if (!t || !inMusic(t)) out.failed.push({ trackId: j.trackId, error: "That file isn't in your music folder, so it's left as it is." });
          else if (ext(dst) !== ext(t.path)) out.failed.push({ trackId: j.trackId, error: `${j.name} would change the file's type.` });
          else if (dst.toLowerCase() !== t.path.toLowerCase() && taken.has(dst.toLowerCase())) out.failed.push({ trackId: j.trackId, error: `A file named ${j.name} is already there.` });
          else {
            taken.delete(t.path.toLowerCase());
            taken.add(dst.toLowerCase());
            const next = { ...t, path: dst, filename: j.name.slice(0, dot) };
            tracks.set(t.id, next);
            out.tracks.push(next);
          }
        }
        return out;
      },
      async pickFolder() { return "~/Music"; },
      async youtubeStatus() { return ytStatus(); },
      async youtubeSetup() {
        YT.cancelSetup = false;
        for (const [what, total] of [["yt-dlp", 41e6], ["Deno", 42e6]]) {
          for (let done = 0; done < total; done += total / 12) {
            if (YT.cancelSetup) throw new Error("Cancelled.");
            ev.emit("youtube-setup", { what, done, total });
            await sleep(60);
          }
        }
        YT.ready = true;
        return ytStatus();
      },
      async youtubeCancelSetup() { YT.cancelSetup = true; },
      async youtubeUpdate() { return { changed: false, status: ytStatus() }; },
      async youtubeSearch(query, limit) {
        await sleep(350);
        const q = ytPlain(query);
        if (/^nothing/i.test(q)) return [];
        if (/^fail/i.test(q)) throw new Error("Couldn't reach YouTube. Check the connection and try again.");
        const out = [];
        for (let i = 0; i < Math.min(limit, 40); i++) {
          const [form, channel, duration, views] = YT_FORMS[i % YT_FORMS.length];
          const round = Math.floor(i / YT_FORMS.length);
          const id = `demo${String(i).padStart(2, "0")}${[...q].reduce((a, c) => (a * 31 + c.charCodeAt(0)) >>> 0, 7).toString(36)}xxxxxxx`.slice(0, 11);
          const hit = { id, title: form.replace("{q}", q) + (round ? ` (take ${round + 1})` : ""), channel, duration, views: Math.floor(views / (round + 1)) };
          YT.hits.set(id, hit);
          out.push({ ...hit, trackId: YT.kept.get(id) || null });
        }
        return out;
      },
      async youtubeFetch(id) {
        const hit = YT.hits.get(id);
        if (!hit) throw new Error("That isn't a YouTube video.");
        const total = hit.duration * 16000;
        YT.cancelled.delete(id);
        if (!YT.ready) throw new Error("yt-dlp isn't set up yet.");
        if (!hit.fetched) {
          for (let done = 0; done < total; done += total / 10) {
            if (YT.cancelled.has(id)) throw new Error("Cancelled.");
            ev.emit("youtube-progress", { id, done, total });
            await sleep(70);
          }
          hit.fetched = true;
        }
        return { id, file: `${id}.opus`, duration: hit.duration, tags: {} };
      },
      async youtubeCancel(id) { YT.cancelled.add(id); },
      youtubePlay({ id, token, from = 0 }) { return this.playParts({ token, from, seconds: YT.hits.get(id).duration }); },
      async youtubeKeep(id) {
        if (!music.root) throw new Error("Choose your music folder first.");
        const hit = YT.hits.get(id);
        await sleep(120);
        const split = hit.title.includes(" - ") ? hit.title.split(" - ") : [hit.channel.replace(" - Topic", ""), hit.title];
        const [artist, title] = [split[0], split.slice(1).join(" - ").replace(/\s*\((Official (Video|Audio)|Lyrics)\)/, "")];
        const t = {
          id: `t-${id}`, path: `${music.root}/${YT.folder}/${artist} - ${title}.opus`, filename: `${artist} - ${title}`, title, artist, album: "", albumartist: "", year: null, track: null, disc: null,
          genre: "", composer: "", labels: ["youtube"], codec: "opus", bitrate: 128, peakBitrate: 128, vbr: true, sampleRate: 48000, bits: 0, channels: 2, duration: hit.duration, size: hit.duration * 16000,
          lufs: null, truePeak: null, cover: false, exists: true, quick: false, drm: false,
        };
        tracks.set(t.id, t);
        YT.kept.set(id, t.id);
        return t;
      },
      async youtubeSearchLists(query, limit) {
        await sleep(300);
        const q = ytPlain(query);
        if (/^nothing/i.test(q)) return [];
        if (/^fail/i.test(q)) throw new Error("Couldn't reach YouTube. Check the connection and try again.");
        const forms = [[`The Night Shift - ${q} (Official Album Playlist)`, "The Night Shift", 6], [`${q} - all the songs`, "Late Hours", 4], [`Best of ${q}`, "Lyric Corner", 5]];
        return forms.slice(0, limit).map(([title, channel, n], i) => {
          const id = `PLdemo${i}${[...q].reduce((a, c) => (a * 31 + c.charCodeAt(0)) >>> 0, 7).toString(36)}xxxxxxxxxx`.slice(0, 18);
          const l = ytList(id, title, channel, n);
          return { id: l.id, title, channel };
        });
      },
      async youtubePlaylist(link) {
        await sleep(200);
        const t = link.trim();
        const id = (t.match(/[?&]list=([\w-]+)/) || [])[1] || (/^(PL|OLAK5uy_)[\w-]{10,}$/.test(t) ? t : "");
        if (id.startsWith("RD")) throw new Error("That's one of YouTube's mixes, which go on without end, so it can't be fetched whole. Try a playlist or an album.");
        if (!/^[\w-]{12,64}$/.test(id)) throw new Error("That isn't a link to a YouTube playlist.");
        const l = YT.lists.get(id) || ytList(id, "The Night Shift - Pasted Album (Official Album Playlist)", "The Night Shift", 4);
        const clean = l.title.replace(/\s*\((Official[^)]*)\)/, "");
        const parts = clean.split(" - ");
        const album = parts.length > 1 && parts[0] === "The Night Shift" ? { title: parts.slice(1).join(" - "), artist: parts[0] } : { title: clean, artist: l.channel };
        return { id, title: l.title, channel: l.channel, name: clean, album, skipped: /Pasted/.test(l.title) ? 1 : 0, cut: false, songs: l.songs.map((s) => ({ ...s, trackId: YT.albums.get(`${id}/${s.id}`) || YT.kept.get(s.id) || null })) };
      },
      async youtubeKeepOnAlbum(id, slot) {
        if (!music.root) throw new Error("Choose your music folder first.");
        const key = `${slot.list}/${id}`;
        if (YT.albums.has(key)) return tracks.get(YT.albums.get(key));
        const single = YT.kept.get(id);
        let t;
        if (single && ![...YT.albums.values()].includes(single)) {
          t = { ...tracks.get(single), album: slot.album, albumartist: slot.artist, track: slot.track };
        } else {
          await this.youtubeFetch(id);
          const pad = String(slot.track).padStart(2, "0");
          t = ytTrack(id, (a, x) => `${slot.artist} - ${slot.album}/${pad} - ${a} - ${x}`, { id: `t-${slot.list}-${id}`, album: slot.album, albumartist: slot.artist, track: slot.track });
        }
        tracks.set(t.id, t);
        YT.albums.set(key, t.id);
        if (!single) YT.kept.set(id, t.id);
        return t;
      },
      async youtubeSetFolder(path) {
        if (!music.root || !path.startsWith(music.root)) throw new Error("Songs from YouTube go in your music folder, so that's a folder in there.");
        YT.folder = path.slice(music.root.length + 1) || "";
        return ytStatus();
      },
      on: ev.on,
    };
  }

  Fatstick.api = T && T.core ? tauriApi() : demoApi();
  Fatstick.isAudioPath = (p) => AUDIO_EXT.includes((p.split(".").pop() || "").toLowerCase());
})(window.Fatstick = window.Fatstick || {});
