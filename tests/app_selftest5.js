// Phases 5 and 6 of tests/app_e2e.py: the lists can be empty. Phase 5 removes every
// playlist and setlist from their own menus and folds one list away. Phase 6 starts
// again on the same library: nothing has come back, and the window works without them.
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
  const until = async (fn, ms = 30000, step = 50) => {
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
  const $$ = (sel) => [...document.querySelectorAll(sel)];
  const fromMenu = async (row, label) => {
    row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 200 }));
    const item = await until(() => $$("#menu [role=menuitem]").find((b) => b.textContent.includes(label)), 3000);
    if (!item) throw new Error(`no menu item "${label}"`);
    item.click();
  };
  try {
    const A = await until(() => window.Fatstick.app && window.Fatstick.app.ready && window.Fatstick.app, 20000);
    if (!check("the window started", !!A)) throw new Error("no app");
    const S = A.S;
    if (cfg.stage === "remove") {
      check("it starts with playlists and setlists from before", S.playlists.length > 0 && S.sets.length > 0);
      while (S.playlists.length) {
        const n = S.playlists.length;
        await fromMenu($("[data-pl]"), "Remove playlist");
        await until(() => S.playlists.length < n, 3000);
      }
      check("the last playlist can be removed", S.playlists.length === 0 && /No playlists/.test($("#pl-name").textContent));
      while (S.sets.length) {
        const n = S.sets.length;
        await fromMenu($("[data-set]"), "Remove setlist");
        await until(() => S.sets.length < n, 3000);
      }
      check("the last setlist can be removed", S.sets.length === 0 && A.stickSet() === undefined && S.view === "playlist");
      $("#fold-setlists").click();
      check("a list folds away", $("#fold-setlists").getAttribute("aria-expanded") === "false" && !$("#side-sets"));
      // Saving is on a timer and each save is awaited: long enough for both.
      await sleep(1500);
    } else {
      check("what was removed stayed removed", S.playlists.length === 0 && S.sets.length === 0);
      check("the folded list stayed folded, the other open", $("#fold-setlists").getAttribute("aria-expanded") === "false" && !$("#side-sets") && $("#fold-playlists").getAttribute("aria-expanded") === "true");
      check("the window says so", /No playlists/.test($("#pl-name").textContent) && /No playlists/.test($("#side-pls").textContent));
      // Space with nothing to play, and the library and back.
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true }));
      A.setView("library");
      A.setView("playlist");
      check("the library and back work with no playlist", /No playlists/.test($("#pl-name").textContent));
      $("#fold-setlists").click();
      check("the list opens again, saying it's empty", /No setlists/.test($("#side-sets").textContent));
      $('#sec-setlists [aria-label="New setlist"]').click();
      const input = await until(() => $("#side ul .input"), 3000);
      check("a setlist can be made with no playlist to put in it", !!input && S.sets.length === 1 && S.sets[0].playlistIds.length === 0);
      input.blur();
    }
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(500);
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
