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

    // The first track: 2 s cut from its start and 1 s of silence after it.
    document.querySelector(`[data-pl="${S.playlists[0].id}"]`).click();
    const list = document.getElementById("list");
    const key = (init) => list.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
    list.focus();
    key({ key: "Home" });
    key({ key: "e" });
    const cut = await until(() => document.getElementById("edge-cutStart"));
    cut.value = "2";
    document.getElementById("edge-padEnd").value = "1";
    [...document.querySelectorAll("#modal .btn.primary")].find((b) => b.textContent === "Apply").click();
    const e0 = S.playlists[0].entries[0];
    check("the first track is cut and padded", e0.cutStart === 2 && e0.padEnd === 1, JSON.stringify(e0));

    // The second track, 5 semitones up (voices not kept natural: it's a tone).
    key({ key: "ArrowDown" });
    key({ key: "k" });
    const semis = await until(() => document.getElementById("key-semis"));
    semis.value = "5";
    semis.dispatchEvent(new Event("input", { bubbles: true }));
    document.getElementById("key-keep").click();
    check("the key dialog names the shift", document.getElementById("key-words").textContent === "5 semitones up (a fourth)", document.getElementById("key-words").textContent);
    [...document.querySelectorAll("#modal .btn.primary")].find((b) => b.textContent === "Apply").click();
    const e1 = S.playlists[0].entries[1];
    check("the second track is 5 semitones up", e1.pitch === 5 && e1.keepFormants === false, JSON.stringify(e1));

    // Three seconds of silence between the two playlists.
    const add2 = [...document.querySelectorAll("#stick select")].find((x) => x.getAttribute("aria-label") === "Add a playlist to this stick");
    add2.value = "__gap";
    add2.dispatchEvent(new Event("change", { bubbles: true }));
    (await until(() => document.querySelector('#stick button[aria-label="Move silence up"]'))).click();
    const secs = await until(() => document.querySelector('#stick input[aria-label="Seconds of silence"]'));
    secs.value = "3";
    secs.dispatchEvent(new Event("change", { bubbles: true }));
    check("silence sits between the playlists", set.playlistIds.length === 3 && set.playlistIds[1].startsWith("gap-") && set.gaps[0].seconds === 3, JSON.stringify(set.playlistIds));

    // The second playlist goes on as one track.
    document.querySelector(`[data-pl="${S.playlists[1].id}"]`).click();
    (await until(() => document.getElementById("join-btn"))).click();
    (await until(() => document.getElementById("join-on"))).click();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    check("the second playlist is joined into one track", S.playlists[1].join === true && A.planSet(set).folders[2].files.length === 1);
    document.querySelector(`[data-pl="${S.playlists[0].id}"]`).click();

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
    lines.push(`INFO names=${JSON.stringify(A.planSet(A.stickSet()).folders.map((f) => [f.folder, f.files.map((it) => it.name)]))}`);

    // The stick's layout window, from a double-click in the sidebar: a real
    // second window, fed by this one.
    let drawn = null;
    const stop = await T.event.listen("layout:rendered", (ev) => { drawn = ev.payload; });
    document.querySelector(`[data-set="${set.id}"]`).dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    await until(() => drawn, 15000);
    stop();
    check("the layout window opens and shows the stick", !!drawn && drawn.folders === 3 && drawn.files === 6 && drawn.joined === 1 && drawn.silences === 1, JSON.stringify(drawn));

    // The player (a silent output in the test): the position moves, then stop.
    await A.playEntry(p.entries[0].id, p.id);
    const moved = await until(() => S.player.pos > 0.5, 10000);
    check("the player plays", !!moved, `position ${S.player.pos}`);
    // The second track plays in its key; the key dialog changes it as it plays.
    await A.playEntry(p.entries[1].id, p.id);
    await until(() => S.player.pos > 0.3, 10000);
    A.keyDialog([p.entries[1].id]);
    const live = await until(() => document.getElementById("key-semis"));
    for (const v of ["7", "-3", "2"]) {
      live.value = v;
      live.dispatchEvent(new Event("input", { bubbles: true }));
      await sleep(250);
    }
    const p0 = S.player.pos;
    const still = await until(() => S.player.playing && S.player.pos > p0 + 0.5, 10000);
    check("the key changes while it plays, and it plays on", !!still && !S.player.error, `position ${S.player.pos}`);
    [...document.querySelectorAll("#modal .btn")].find((b) => b.textContent === "Cancel").click();
    check("Cancel leaves the entry's key", p.entries[1].pitch === 5);
    A.stopPlayback();
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(800); // the last saves
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
