// Phase 4 of tests/app_e2e.py: YouTube, in the real app, with tests/fake_ytdlp.py
// standing in for yt-dlp and the music folder from phase 3. Search, hear a song
// without keeping it, add others to the library and a playlist, and see what
// happens when one can't be had. tests/app_e2e.py then checks the files with
// ffprobe, and the stand-in's log for what it was asked for.
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
  const toast = () => ($("#toast").hidden ? "" : $("#toast").textContent);
  const key = (el, k, more = {}) => el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...more }));
  const search = async (q) => {
    const input = $("#yt-in");
    input.value = q;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    key(input, "Enter");
  };
  try {
    const A = await until(() => window.Fatstick.app && window.Fatstick.app.ready && window.Fatstick.app, 20000);
    if (!check("the window started", !!A)) throw new Error("no app");
    const S = A.S;
    const tracksBefore = S.tracks.size;

    // The panel: what it found to work with.
    A.yt.open();
    check("Search YouTube opens that side of the search panel", S.searching && S.searchSource === "youtube" && $("#srch-tab-youtube").getAttribute("aria-selected") === "true");
    await until(() => A.yt.state.status);
    const st = A.yt.state.status;
    check("it finds yt-dlp and Deno", st.ytdlp && st.ytdlp.version === "2026.10.26" && st.deno && st.deno.version === "2.9.6", JSON.stringify(st));
    check("neither is the app's own copy, so there's nothing to update", !st.ytdlp.managed && !$("#yt-update"));
    check("nothing asks to set up", !$(".yt-setup"));
    check("it says where songs will go", st.folder === `${cfg.root}/YouTube` && /Added songs go in YouTube/.test($(".yt-foot").textContent), st.folder);

    // Searching.
    await search("the band");
    await until(() => $$(".yr").length >= 7);
    check("a search lists what YouTube has", $$(".yr").length === 7, `${$$(".yr").length}`);
    const row = (id) => document.getElementById(`yr-${id}`);
    check("each with its channel and length", /The Band VEVO/.test(row("aaaaaaaaaa1").textContent) && /0:04/.test(row("aaaaaaaaaa1").textContent));
    await search("nothing");
    await until(() => /Nothing on YouTube/.test($(".srch-scope").textContent));
    check("a search with no results says so", $$(".yr").length === 0, $(".srch-scope").textContent);
    await search("the band");
    await until(() => $$(".yr").length >= 7);
    const link = await T.core.invoke("youtube_search", { query: "https://youtu.be/aaaaaaaaaa2?t=3", limit: 25 });
    check("a pasted link is that video", link.length === 1 && link[0].id === "aaaaaaaaaa2", JSON.stringify(link));

    // Hearing one: from the keyboard, as a double-click does it.
    key($("#yt-in"), "ArrowDown");
    check("the arrow key goes down into the results", row("aaaaaaaaaa1").classList.contains("sel"));
    key($(".yt-res"), "Enter");
    const heard = await until(() => S.player.trackId === "yt:aaaaaaaaaa1" && S.player.playing && S.player.pos > 0.2, 30000);
    check("Enter fetches the song and plays it", !!heard, JSON.stringify({ t: S.player.trackId, p: S.player.pos, e: S.player.error, toast: toast() }));
    check("the player names it, from YouTube", /First Song|The Band/.test($(".np-t").textContent) && /From YouTube/.test($(".np-s").textContent), $(".np-s").textContent);
    check("the row shows it's playing", !!(await until(() => row("aaaaaaaaaa1").querySelector(".eq"))));
    check("hearing it doesn't add it to the library", S.tracks.size === tracksBefore && !row("aaaaaaaaaa1").querySelector(".sr-in"));
    A.seekTo(2);
    check("it can be sought in", !!(await until(() => S.player.pos >= 2, 5000)), `${S.player.pos}`);
    const ended = await until(() => !S.player.trackId, 15000);
    check("and it plays once and stops", !!ended);

    // Adding one to the library, with its button.
    row("aaaaaaaaaa2").querySelector(".yr-add").click();
    const inLib = await until(() => row("aaaaaaaaaa2").querySelector(".sr-in"));
    check("the + button adds it to the library", !!inLib, toast());
    const second = [...S.tracks.values()].find((t) => t.title === "Second Song");
    check("as a track in the music folder, with clean tags", second && second.path.startsWith(`${cfg.root}/YouTube/`) && second.artist === "Some Channel" && second.exists, JSON.stringify(second));
    check("a toast says so", /In your library: Second Song/.test(toast()), toast());
    check("it has the label \"youtube\", read back from the file", second && JSON.stringify(second.labels) === '["youtube"]', second && JSON.stringify(second.labels));
    lines.push(`INFO second=${second ? second.path : ""}`);

    // Adding to the open playlist: Shift+Enter, as in the music search.
    const pl = A.pl();
    const n0 = pl.entries.length;
    key($(".yt-res"), "Enter", { shiftKey: true });
    const added = await until(() => A.pl().entries.length === n0 + 1);
    check("Shift+Enter adds it to the open playlist", !!added, `${A.pl().entries.length} from ${n0}; ${toast()}`);
    const first = A.track(A.pl().entries[n0].trackId);
    check("the artist comes from the title, without its \"(Official Video)\"", first.title === "First Song" && first.artist === "The Band", `${first.artist} / ${first.title}`);
    check("and it can be undone like any other add", /Added 1 track/.test(toast()), toast());
    lines.push(`INFO first=${first.path}`);
    // It's now a song like any other, already fetched: heard from the library.
    await A.playTrack(first.id);
    check("it plays as a track of the library", !!(await until(() => S.player.trackId === first.id && S.player.pos > 0.1, 10000)));
    A.stopPlayback();

    // A drop in the list goes in at that place, in the playlist it was dropped on.
    const before = A.pl().entries.length;
    const dropping = A.yt.addTo(["yt:aaaaaaaaaa3"], { kind: "list", index: 0 });
    await dropping;
    check("a drop in the list goes in at that place", A.pl().entries.length === before + 1 && A.track(A.pl().entries[0].trackId).title === "Third Song Proper", A.track(A.pl().entries[0].trackId).title);
    const third = A.track(A.pl().entries[0].trackId);
    check("what YouTube knows about a song goes in its tags", third.album === "The Album" && third.year === 1975 && third.artist === "The Band", JSON.stringify([third.album, third.year, third.artist]));
    lines.push(`INFO third=${third.path}`);

    // Once kept, always that track: no second file, nor a second download.
    const tid = first.id;
    await search("the band");
    await sleep(600);
    check("a new search still marks what's in the library", !!(await until(() => row("aaaaaaaaaa1").querySelector(".sr-in"))));
    const again = await T.core.invoke("youtube_keep", { id: "aaaaaaaaaa1" });
    check("keeping it again gives the same track", again.id === tid);
    const n1 = A.pl().entries.length;
    A.yt.addTo(["yt:aaaaaaaaaa1"], { kind: "playlist", id: A.pl().id });
    await until(() => A.pl().entries.length === n1 + 1, 8000);
    check("adding it to a playlist again uses that track", A.track(A.pl().entries[n1].trackId).id === tid);

    // Ones that can't be had: nothing fetched, and it says why.
    row("noaudiostrm").querySelector(".yr-add").click();
    await until(() => row("noaudiostrm").querySelector(".yr-add.failed"));
    check("a video with no audio-only stream isn't fetched", /no audio-only stream/.test(toast()) && !row("noaudiostrm").querySelector(".sr-in"), toast());
    row("blockedsong").querySelector(".yr-add").click();
    await until(() => row("blockedsong").querySelector(".yr-add.failed"));
    check("a refused download says what usually fixes it", /Updating yt-dlp/.test(toast()), toast());

    // Stopping a download.
    row("slowsongslo").querySelector(".yr-add").click();
    await until(() => row("slowsongslo").querySelector(".yr-pct"));
    const stop = row("slowsongslo").querySelector(".yr-act button");
    check("a song on its way shows its progress, and a way to stop it", !!stop);
    stop.click();
    check("stopping it stops it", !!(await until(() => row("slowsongslo").querySelector(".yr-add:not(.failed)"), 8000)));

    // The list of what's been kept is the app's own business.
    const init = await T.core.invoke("init");
    check("the window isn't handed the list of songs kept", !("youtubeKept" in init.settings));
    const refused = await T.core.invoke("set_setting", { key: "youtubeKept", value: "{}" }).then(() => false, () => true);
    check("nor can it change it", refused);
    const outside = await T.core.invoke("youtube_set_folder", { path: cfg.outside }).then(() => false, (e) => String(e));
    check("songs can't be sent outside the music folder", !!outside && /music folder/.test(outside), String(outside));

    // A song heard and not kept goes with the cache; kept ones don't.
    await T.core.invoke("youtube_fetch", { id: "aaaaaaaaaa1" });
    await T.core.invoke("clear_cache");
    const gone = await T.core.invoke("youtube_play", { id: "aaaaaaaaaa1", token: "t", from: 0 }).then(() => false, () => true);
    check("clearing the cache clears what was only heard", gone);
    check("the kept song is still in the library", A.track(tid).exists);

    // The other side of the panel is as it was.
    $("#srch-tab-music").click();
    check("the Your music tab goes back to searching your music", S.searchSource === "music" && !!$("#srch-in"));
    A.lib.closeSearch();
    check("the search closes", !S.searching);
  } catch (e) {
    check("no exception", false, String((e && e.stack) || e));
  }
  await sleep(500);
  await T.core.invoke("selftest_report", { ok, report: lines.join("\n") });
})();
