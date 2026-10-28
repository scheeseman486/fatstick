// Phase 1 of tests/app_e2e.py, run inside the real app's window (a debug
// build started with FATSTICK_SELFTEST). From an empty library: add a folder of
// real audio, even out the volume, put a second playlist on the stick, pick
// the disk image as the stick, sync through the button and its confirmation,
// and play a track. Reports back through the selftest_report command.
(async () => {
  const cfg = window.__FATSTICK_SELFTEST_CONFIG;
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
    const A = await until(() => window.Fatstick.app && window.Fatstick.app.ready && window.Fatstick.app, 20000);
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

    // The fourth track fades in over 2 s and out over 3 s.
    key({ key: "ArrowDown" });
    key({ key: "ArrowDown" });
    key({ key: "f" });
    const fin = await until(() => document.getElementById("fade-in"));
    const slide = (el, v) => { el.value = v; el.dispatchEvent(new Event("input", { bubbles: true })); };
    slide(fin, "2");
    slide(document.getElementById("fade-out"), "3");
    [...document.querySelectorAll("#modal .btn.primary")].find((b) => b.textContent === "Apply").click();
    const e3 = S.playlists[0].entries[3];
    check("the fourth track fades in over 2 s and out over 3 s", e3.fadeIn === 2 && e3.fadeOut === 3, JSON.stringify(e3));

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

    // The stick's view, from a click in the sidebar: its folders in order.
    document.querySelector(`[data-set="${set.id}"]`).click();
    const shown = await until(() => !document.getElementById("stickview").hidden && document.querySelectorAll("#stickview .lay-folder").length, 5000);
    const kinds = [...document.querySelectorAll("#stickview .lay-folder")].map((f) => [...f.classList].find((c) => c.startsWith("k-")));
    check("the stick's view shows what's on it, in order", !!shown && JSON.stringify(kinds) === JSON.stringify(["k-playlist", "k-silence", "k-joined"]) && document.querySelectorAll("#stickview .lay-file").length >= 5, JSON.stringify(kinds));
    document.querySelector(`[data-pl="${p.id}"]`).click();

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
    // The fade dialog: the sliders change what plays, and the player bar,
    // usable meanwhile, seeks with the fades being tried.
    await A.playEntry(p.entries[3].id, p.id);
    await until(() => S.player.pos > 0.3, 10000);
    A.fadeDialog([p.entries[3].id]);
    const fo = await until(() => document.getElementById("fade-out"));
    check("the player bar stays usable while the dialog is open", document.body.classList.contains("modal-player") && getComputedStyle(document.getElementById("player")).zIndex === "60");
    for (const v of ["6", "1", "4"]) {
      fo.value = v;
      fo.dispatchEvent(new Event("input", { bubbles: true }));
      await sleep(250);
    }
    A.seekTo(1.0);
    const p1 = S.player.pos;
    const on = await until(() => S.player.playing && S.player.pos > p1 + 0.5, 10000);
    check("the fades change while it plays, and a seek keeps it playing", !!on && !S.player.error && S.player.entryId === p.entries[3].id, `position ${S.player.pos}`);
    [...document.querySelectorAll("#modal .btn")].find((b) => b.textContent === "Cancel").click();
    check("Cancel leaves the entry's fades", p.entries[3].fadeIn === 2 && p.entries[3].fadeOut === 3 && !document.body.classList.contains("modal-player"));
    // The start and end dialog: its waveform comes from the file, and the
    // silence being tried is heard before the track (the position waits at
    // the cut start while it plays).
    A.edgesDialog([p.entries[0].id]);
    const wave = await until(() => (document.querySelector(".edge-panel.start .wave").getAttribute("d") || "").length > 100 && document.querySelector(".edge-panel.start .wave"), 15000);
    check("the start and end dialog draws the track's waveform", !!wave);
    const padIn = document.getElementById("edge-padStart");
    padIn.value = "1.5";
    padIn.dispatchEvent(new Event("input", { bubbles: true }));
    [...document.querySelectorAll("#modal .btn")].find((b) => b.textContent === "Hear the start").click();
    await until(() => S.player.entryId === p.entries[0].id && S.player.playing, 10000);
    await sleep(900);
    const waited = S.player.pos;
    const went = await until(() => S.player.pos > 2.4, 10000);
    check("hearing the start plays the silence being tried, then the track from its cut", Math.abs(waited - 2) < 0.15 && !!went, `at 0.9 s: ${waited}; then ${S.player.pos}`);
    [...document.querySelectorAll("#modal .btn")].find((b) => b.textContent === "Cancel").click();
    check("Cancel leaves the entry's start and end", !p.entries[0].padStart && p.entries[0].cutStart === 2 && !S.player.entryId);
    A.stopPlayback();
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(800); // the last saves
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
