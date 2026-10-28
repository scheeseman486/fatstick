// Phase 2 of tests/app_e2e.py: the app started again on the same library.
// Everything from phase 1 must still be there, the cache must still hold the
// prepared files, and an edit must make the stick out of date (undo: up to
// date again).
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
  const until = async (fn, ms = 30000, step = 100) => {
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
  try {
    const A = await until(() => window.Fatstick.app && window.Fatstick.app.ready && window.Fatstick.app, 20000);
    const S = A.S;
    const $sync = () => document.getElementById("sync-btn");
    check("both playlists were kept", S.playlists.length === 2 && S.playlists[0].entries.length === cfg.titles.length && S.playlists[1].entries.length === cfg.titles2.length);
    const p = S.playlists[0];
    check("the volume setting was kept", p.norm.on === true);
    check("loudness was kept", p.entries.every((e) => A.track(e.trackId).lufs != null));
    const set = S.sets[0];
    check("the stick kept its playlists, its silence and its stick", set.playlistIds.length === 3 && set.gaps.length === 1 && set.gaps[0].seconds === 3 && (set.stickId || "").startsWith("image:"));
    check("the joined playlist and the cut track were kept", S.playlists[1].join === true && S.playlists[0].entries[0].cutStart === 2 && S.playlists[0].entries[0].padEnd === 1);
    check("the sync record was kept: up to date", A.staleness(A.planSet(set)).current, JSON.stringify(A.staleness(A.planSet(set)).parts));
    const ready = await until(() => A.planSet(set).pending.length === 0 && A.planSet(set).keys.length > 0, 10000);
    check("the cache still holds every prepared file", !!ready, `${A.planSet(set).pending.length} pending`);
    const status = document.querySelector(".st-status");
    check("the window says so", status && /Up to date/.test(status.textContent), status && status.textContent);

    // Move a track with the keyboard: out of date. Undo: up to date again.
    const list = document.getElementById("list");
    list.focus();
    list.dispatchEvent(new FocusEvent("focus"));
    // Sent to the list (as a key press would be); it bubbles to the window's handler.
    const key = (init) => list.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
    key({ key: "Home" });
    key({ key: "ArrowDown", altKey: true });
    const st = A.staleness(A.planSet(set));
    check("moving a track makes the stick out of date", !st.current && st.parts.includes("order changed"), JSON.stringify(st.parts));
    key({ key: "z", ctrlKey: true });
    check("undo makes it up to date again", A.staleness(A.planSet(set)).current);

    // Another stick plugged in where this one was (same device, other size):
    // not taken for it, so it can't be erased without being picked. (Undo
    // restored the sets from a snapshot: take the live one.)
    const live = A.stickSet();
    check("the stick's model and size were kept", !!live.stickModel && live.stickSize > 0, `${live.stickModel}, ${live.stickSize}`);
    const size = live.stickSize;
    live.stickSize = size + 512;
    await A.refreshSticks(false);
    const warned = [...document.querySelectorAll("#stick .st-note.warn")].find((n) => /syncs to/.test(n.textContent));
    check("another stick in its place isn't taken for it", !A.planSet(live).dev && $sync().disabled && warned && /isn't the stick this one syncs to/.test(warned.textContent),
      `dev ${!!A.planSet(live).dev}, button ${$sync().disabled}, note ${warned && warned.textContent}`);
    live.stickSize = size;
    await A.refreshSticks(false);
    check("the stick itself is taken again", !!A.planSet(live).dev && !$sync().disabled);

    // The stick remembers what it holds: the note phase 1's sync left on it.
    const img = S.sticks.find((d) => d.image);
    const hd = A.holder(img);
    check("the stick's note names this stick (set)", !!hd && hd.ours && hd.set && hd.set.id === live.id, JSON.stringify(img.note));
    const flag = document.querySelector(`[data-set="${live.id}"] .flag`);
    check("the sidebar marks it plugged in", flag && /plugged in/.test(flag.textContent), flag && flag.textContent);
    // A new stick (set) on the same stick: a warning, and the sync needs a tick.
    document.querySelector('button[aria-label="New stick"]').click();
    const nameIn = await until(() => document.querySelector("#side input.input"));
    nameIn.value = "Gym clip";
    nameIn.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const gym = await until(() => S.sets.find((x) => x.name === "Gym clip"));
    check("a second stick (set) is made and selected", !!gym && A.stickSet().id === gym.id);
    const radio = await until(() => [...document.querySelectorAll("#stick .dev input")].find((i) => i.value === img.id));
    radio.click();
    const warn = await until(() => document.getElementById("holds-other"));
    check("picking the first stick's stick warns", warn && /This stick holds My stick .*Syncing Gym clip to it replaces that/.test(warn.textContent), warn && warn.textContent);
    $sync().click();
    const rw = await until(() => document.getElementById("replace-warning"));
    const go = [...document.querySelectorAll("#modal .btn.primary")][0];
    check("the confirmation asks before replacing it", rw && /My stick will need a stick of its own/.test(rw.textContent) && go && go.disabled, rw && rw.textContent);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    A.selectSet(live.id);
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(800);
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
