// Phase 3 of tests/app_e2e.py: the music library, in the real app. Choose a
// music folder (it holds phase 1's playlist folders and a few more), scan it,
// browse and search it, write tags into two files and rename them through the
// library view, and see a file that appears in the folder (tests/app_e2e.py
// copies one in once the renames are on disk) show up by itself. The files
// are checked with ffprobe afterwards.
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
  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => [...document.querySelectorAll(sel)];
  // A click on a row of the library table, as the pointer would make it.
  const clickRow = (row, mods = {}) => {
    const r = row.getBoundingClientRect();
    const at = { bubbles: true, cancelable: true, button: 0, pointerId: 7, pointerType: "mouse", clientX: r.left + 20, clientY: r.top + r.height / 2, ...mods };
    row.dispatchEvent(new PointerEvent("pointerdown", at));
    $(".lib-scroll").dispatchEvent(new PointerEvent("pointerup", at));
  };
  const typeIn = (input, value) => {
    input.value = value;
    input.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const rowOf = (title) => $$(".lr").find((r) => r.querySelector(".t").textContent === title);
  const toast = () => ($("#toast").hidden ? "" : $("#toast").textContent);
  try {
    const A = await until(() => window.Fatstick.app && window.Fatstick.app.ready && window.Fatstick.app, 20000);
    if (!check("the window started", !!A)) throw new Error("no app");
    const S = A.S;
    const before = new Map(S.playlists[0].entries.map((e) => [e.trackId, A.track(e.trackId).path]));

    // The folder, chosen and scanned.
    A.setView("library");
    check("the library asks for the folder first", /Choose the folder your music is in/.test($(".lib-start").textContent));
    await A.lib.useFolder(cfg.root);
    const scanned = await until(() => S.music && S.music.root && !S.music.scanning && S.music.lastScan, 60000);
    check("choosing the music folder scans it", !!scanned, JSON.stringify(S.music));
    const inLib = () => [...S.tracks.values()].filter((t) => A.lib.inMusic(t));
    check("every audio file in it is in the library", inLib().length === cfg.libraryCount, `${inLib().length} of ${cfg.libraryCount}`);
    check("tracks already in a playlist kept their place in it", S.playlists[0].entries.every((e) => before.get(e.trackId) === A.track(e.trackId).path && A.lib.inMusic(A.track(e.trackId))));
    const lanterns = inLib().find((t) => t.title === "Lanterns");
    check("labels are read from the Grouping tag", lanterns && JSON.stringify(lanterns.labels) === JSON.stringify(["chill", "road trip"]), lanterns && JSON.stringify(lanterns.labels));
    check("the folder is watched", S.music.watching === true);
    // Reading the folder must not look like a change to it.
    const firstScan = S.music.lastScan;
    await sleep(4500);
    check("the scan doesn't set off another scan", S.music.lastScan === firstScan && !S.music.scanning, `${firstScan} -> ${S.music.lastScan}`);
    await until(() => $$(".lr").length === cfg.libraryCount, 5000);
    check("the library view lists them", $$(".lr").length === cfg.libraryCount, `${$$(".lr").length} rows`);
    const chill = $$(".fct").find((b) => /^chill/.test(b.textContent));
    check("the labels are there to browse by", chill && /2$/.test(chill.textContent), chill && chill.textContent);

    // The scan read the MP3 quickly (not every frame); in a playlist, it's read in full.
    check("the scan read the MP3 quickly", lanterns.quick === true);
    clickRow(rowOf("Lanterns"));
    $(".lib-scroll").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true, bubbles: true, cancelable: true }));
    check("Shift+Enter adds the selection to the open playlist", A.pl().entries.some((e) => e.trackId === lanterns.id));
    const full = await until(() => A.track(lanterns.id).quick === false, 30000);
    check("then it's read in full, in the background", !!full);

    // Double-click plays a song on its own, once (the test's audio output is silent).
    const harbour = inLib().find((t) => t.title === "Harbour Song");
    const counts = S.playlists.map((p) => p.entries.length).join();
    const row = rowOf("Harbour Song");
    clickRow(row);
    clickRow(row);
    const started = await until(() => S.player.trackId === harbour.id && S.player.playing, 10000);
    check("double-clicking a library track plays it", !!started && /Harbour Song/.test($("#player .np-t").textContent));
    check("without adding it to a playlist", S.playlists.map((p) => p.entries.length).join() === counts);
    const moved = await until(() => S.player.pos > 1, 10000);
    check("the player plays through it", !!moved, `${S.player.pos} s`);
    const stopped = await until(() => !S.player.trackId, 15000);
    check("and stops when it ends: one and done", !!stopped && $("#player .np-t").textContent === "Nothing playing");

    // Search, from the sidebar.
    A.lib.openSearch();
    typeIn($("#srch-in"), "harbor");
    await until(() => $$(".srch-res .sr").length > 0, 5000);
    const hits = $$(".srch-res .sr .sr-t").map((x) => x.textContent);
    check("search finds a song by a near spelling", hits[0] === "Harbour Song", hits.join(" | "));
    A.lib.closeSearch();
    A.setView("library");

    // Tags for two tracks: a genre, a label added and one taken off.
    clickRow(rowOf("Lanterns"));
    clickRow(rowOf("Harbour Song"), { ctrlKey: true });
    check("two tracks selected", $$(".lr.sel").length === 2 && /2 tracks/.test($(".lib-edit h3").textContent));
    const genre = $('.lib-edit input[id^="ted-genre"]');
    typeIn(genre, "Night Folk");
    genre.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    const lbl = $('.lib-edit input[id^="ted-lbl"]');
    lbl.value = "kitchen";
    lbl.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    const road = $$(".lib-edit .tag.lbl").find((t) => /road trip/.test(t.textContent));
    road.querySelector('button[aria-label^="Remove"]').click();
    const wrote = await until(() => /“road trip” taken off/.test($(".lib-edit .ted-status").textContent), 30000);
    check("each change is written as it's made, with no prompt", !!wrote && $("#modal-back").hidden, $(".lib-edit .ted-status").textContent);
    const now = (title) => inLib().find((t) => t.title === title);
    check("the library reads them back", ["Lanterns", "Harbour Song"].every((x) => now(x).genre === "Night Folk" && now(x).labels.includes("kitchen") && now(x).labels.includes("chill") && !now(x).labels.includes("road trip")),
      ["Lanterns", "Harbour Song"].map((x) => `${now(x).genre} [${now(x).labels}]`).join(" / "));

    // File names from the tags (the template "01 - Title").
    $$(".lib-head .btn").find((b) => /Clean up file names/.test(b.textContent)).click();
    await until(() => $(".ren-row"), 5000);
    const news = $$("#modal .ren-row:not(.same) .new").map((x) => x.textContent);
    check("the rename preview shows the new names", JSON.stringify(news.sort()) === JSON.stringify(["01 - Lanterns.mp3", "02 - Harbour Song.flac"]), news.join(" | "));
    $("#modal .mfoot .btn.primary").click();
    const renamed = await until(() => /Renamed 2 files/.test(toast()), 30000);
    check("the files are renamed", !!renamed, toast());
    const paths = ["Lanterns", "Harbour Song"].map((x) => now(x).path);
    check("each keeps its folder", paths.every((p) => p.startsWith(cfg.owls)), paths.join(" | "));
    lines.push(`INFO tagged=${JSON.stringify(paths)}`);

    // A file copied into the folder shows up by itself (the folder is watched).
    const arrived = await until(() => inLib().find((t) => t.title === "New Arrival"), 45000, 250);
    check("a new file in the folder shows up without a scan", !!arrived);
    check("and is in the library view", !!(await until(() => rowOf("New Arrival"), 5000)));
    check("the renamed tracks weren't taken for new ones", inLib().length === cfg.libraryCount + 1, `${inLib().length}`);
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(800);
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
