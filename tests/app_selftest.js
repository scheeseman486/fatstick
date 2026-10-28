// Phase 1 of tests/app_e2e.py, run inside the real app's window (a debug
// build started with P2U_SELFTEST). From an empty library: add a folder of
// real audio, even out the volume, put a second playlist on the stick, pick
// the disk image as the stick, sync through the button and its confirmation,
// and play a track. Reports back through the selftest_report command.
(async () => {
  const cfg = window.__P2U_SELFTEST_CONFIG;
  const T = window.__TAURI__;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const lines = [];
  let ok = true;
  const check = (name, cond, extra) => {
    lines.push(`${cond ? "PASS" : "FAIL"} ${name}${extra ? ` — ${extra}` : ""}`);
    if (!cond) ok = false;
    return cond;
  };
  const until = async (fn, ms = 60000, step = 100) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      try {
        const v = fn();
        if (v) return v;
      } catch { /* not yet */ }
      await sleep(step);
    }
    return null;
  };
  const $ = (sel) => document.querySelector(sel);
  try {
    const A = await until(() => window.P2U.app && window.P2U.app.ready && window.P2U.app, 20000);
    if (!check("the window started", !!A)) throw new Error("no app");
    const S = A.S;
    check("first run: one empty playlist and one stick", S.playlists.length === 1 && S.sets.length === 1 && S.playlists[0].entries.length === 0);
    check("the bundled ffmpeg was found", !S.ffmpegError, S.ffmpegError || "");

    // A folder, read by the library and added in album order.
    await A.addPaths([cfg.music], { kind: "list", index: null });
    const p = S.playlists[0];
    const title = (e) => A.track(e.trackId).title || A.track(e.trackId).filename;
    check("the folder's tracks are added in album order", JSON.stringify(p.entries.map(title)) === JSON.stringify(cfg.titles), p.entries.map(title).join(" | "));
    const codecs = p.entries.map((e) => A.track(e.trackId).codec);
    check("ffprobe read every format", JSON.stringify(codecs) === JSON.stringify(cfg.codecs), codecs.join(","));

    // Even out the volume, through the popover.
    [...document.querySelectorAll("#pl-head button")].find((b) => /Even volume/.test(b.textContent)).click();
    (await until(() => $("#norm-on"))).click();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    check("evening the volume is on", p.norm.on === true);
    const measured = await until(() => p.entries.every((e) => A.track(e.trackId).lufs != null), 90000);
    check("loudness measured for every track", !!measured, p.entries.map((e) => A.track(e.trackId).lufs).join(", "));

    // A second folder becomes a second playlist, put on the same stick.
    await A.addPaths([cfg.music2], { kind: "new" });
    check("a second playlist was made", S.playlists.length === 2 && S.playlists[1].entries.length === cfg.titles2.length);
    A.S.current = S.playlists[0].id;
    const set = A.stickSet();
    const add = await until(() => [...document.querySelectorAll("#stick select")].find((s) => s.getAttribute("aria-label") === "Add a playlist to this stick"));
    add.value = S.playlists[1].id;
    add.dispatchEvent(new Event("change", { bubbles: true }));
    check("both playlists are on the stick", set.playlistIds.length === 2);

    // The disk image, offered as a stick.
    await A.refreshSticks(true);
    const radio = await until(() => [...document.querySelectorAll("#stick .dev input")].find((i) => i.value.startsWith("image:")), 10000);
    check("the disk image is offered as a stick", !!radio);
    radio.click();
    check("the stick is chosen", (A.stickSet().stickId || "").startsWith("image:"));

    // Preparation in the background, then the sync through the window.
    const prepared = await until(() => {
      const plan = A.planSet(A.stickSet());
      return plan.waiting.length === 0 && plan.pending.length === 0;
    }, 180000, 250);
    check("background preparation finished before the sync", !!prepared, ($(".prep") || {}).textContent);
    const btn = $("#sync-btn");
    check("Erase and sync is enabled", btn && !btn.disabled, ($(".why-not") || {}).textContent);
    btn.click();
    const confirm = await until(() => [...document.querySelectorAll("#modal .btn.primary")].find((b) => b.textContent === "Erase and sync"));
    check("the confirmation lists what's on the stick now", /Now on it/.test($("#modal").textContent));
    // Every progress event, as the window gets them (a small image is written
    // faster than the window could be polled).
    const stages = new Set();
    const unlisten = await T.event.listen("sync-progress", (ev) => stages.add(ev.payload.stage));
    confirm.click();
    const done = await until(() => $(".sync-done"), 300000, 50);
    unlisten();
    const sawWrite = stages.has("write");
    check("the sync finished", !!done && /^Synced\./.test(done.textContent), done ? done.textContent : `timed out: ${($(".sync-msg") || {}).textContent}`);
    check("progress went through the write stage", sawWrite, [...stages].join(","));
    check("the stick is up to date afterwards", A.staleness(A.planSet(A.stickSet())).current);
    const known = await until(() => { const hd = A.holder(S.sticks.find((d) => d.image)); return hd && hd.set && hd.set.id === set.id; }, 10000);
    check("the stick now carries a note naming this stick (set)", !!known);
    lines.push(`INFO names=${JSON.stringify(A.planSet(A.stickSet()).folders.map((f) => [f.folder, f.present.map((it) => it.name)]))}`);

    // The player (a silent output in the test): the position moves, then stop.
    await A.playEntry(p.entries[0].id, p.id);
    const moved = await until(() => S.player.pos > 0.5, 10000);
    check("the player plays", !!moved, `position ${S.player.pos}`);
    A.stopPlayback();
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(800); // the last saves
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
