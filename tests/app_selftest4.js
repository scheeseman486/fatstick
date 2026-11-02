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
    await until(() => $$(".yr").length >= 11);
    check("a search lists what YouTube has", $$(".yr").length === 11, `${$$(".yr").length}`);
    const row = (id) => document.getElementById(`yr-${id}`);
    check("each with its channel and length", /The Band VEVO/.test(row("aaaaaaaaaa1").textContent) && /0:04/.test(row("aaaaaaaaaa1").textContent));
    await search("nothing");
    await until(() => /Nothing on YouTube/.test($(".srch-scope").textContent));
    check("a search with no results says so", $$(".yr").length === 0, $(".srch-scope").textContent);
    await search("the band");
    await until(() => $$(".yr").length >= 11);
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

    // A playlist: found by a search, opened and kept as an album.
    $("#yt-kind-lists").click();
    await search("the big album");
    await until(() => $$(".yp").length >= 3);
    check("a search for playlists finds them", $$(".yp").length === 3 && /The Big Album/.test($$(".yp")[0].textContent), `${$$(".yp").length}`);
    $(".yp .yp-open").click();
    await until(() => $(".yp-head"));
    await until(() => $$(".yr").length === 3);
    check("opening one lists its songs, without the private one", $$(".yr").length === 3 && /1 song can't be fetched/.test($(".yp-head").textContent), $(".yp-head").textContent);
    check("and what it'd be as an album", /As an album: The Big Album, by The Band/.test($(".yp-head").textContent));
    const plsAtFirst = S.playlists.length;
    $("#yp-album").click();
    check("adding it says it's on its way", !!(await until(() => $("#yp-prog-text"), 5000)));
    const whole = await until(() => /The Big Album is in your library: 3 songs/.test(toast()), 60000);
    check("all of it arrives", !!whole, toast());
    const onAlbum = [...S.tracks.values()].filter((t) => t.album === "The Big Album").sort((a, b) => a.track - b.track);
    check("as an album in the library, in order", onAlbum.map((t) => `${t.track} ${t.title}`).join() === "1 Album Song One,2 Album Song Two,3 Album Song Three", onAlbum.map((t) => `${t.track} ${t.title}`).join());
    check("by its artist, read back from the files", onAlbum.every((t) => t.albumartist === "The Band" && t.artist === "The Band" && JSON.stringify(t.labels) === '["youtube"]'), JSON.stringify(onAlbum[0]));
    check("and no playlist is made of it", S.playlists.length === plsAtFirst);
    const noRefetch = await T.core.invoke("youtube_keep_on_album", { id: "albumsong01", slot: { list: "PLfakealbum000001", album: "The Big Album", artist: "The Band", track: 1, channel: "The Band" } });
    check("asking again is the same track", noRefetch.id === onAlbum[0].id);

    // Import: the same, and a playlist, of songs from several channels, one kept on its own earlier.
    const single = [...S.tracks.values()].find((t) => t.title === "Second Song");
    const beforeImport = S.tracks.size;
    await A.yt.importDialog();
    const box = await until(() => $("#yt-import"));
    check("Import from YouTube asks for a playlist", !!box);
    box.value = "https://www.youtube.com/watch?v=aaaaaaaaaa1&list=RDaaaaaaaaaa1";
    box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const mixRefused = await until(() => { const e = $("#yt-import-err"); return e && !e.hidden && e.textContent; }, 8000);
    check("a mix is refused", /without end/.test(mixRefused || ""), String(mixRefused));
    box.value = "https://www.youtube.com/playlist?list=PLfakemix00000002";
    box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const imported = await until(() => /Road Trip Tunes is in your library and your playlists: 2 songs/.test(toast()), 60000);
    check("a playlist's songs arrive", !!imported, toast());
    const madePl = S.playlists.find((x) => x.name === "Road Trip Tunes");
    check("and it's in the playlist list, in its order", madePl && madePl.entries.map((e) => A.track(e.trackId).title).join() === "Mixed One,Second Song", madePl && madePl.entries.map((e) => A.track(e.trackId).title).join());
    const mixed = [...S.tracks.values()].filter((t) => t.album === "Road Trip Tunes").sort((a, b) => a.track - b.track);
    check("and an album of many artists in the library", mixed.length === 2 && mixed.every((t) => t.albumartist === "Various Artists") && mixed[0].artist === "Other Band", JSON.stringify(mixed.map((t) => [t.artist, t.albumartist, t.track])));
    check("a song kept on its own joined it rather than being filed twice", mixed[1].id === single.id && S.tracks.size === beforeImport + 1 && mixed[1].track === 2, `${S.tracks.size} after ${beforeImport}`);

    // One that fails: says so, and keeps the rest.
    A.lib.closeSearch();
    A.yt.open();
    const half = await T.core.invoke("youtube_playlist", { link: "PLfakebroken00003" });
    check("the app reads a playlist by its id", half.songs.length === 2 && half.album.title === "Half Broken");
    const blocked = await T.core.invoke("youtube_keep_on_album", { id: "blockedsong", slot: { list: half.id, album: "Half Broken", artist: "The Band", track: 2, channel: "The Band" } }).then(() => "", (e) => String(e));
    check("a song that can't be had says why", /Updating yt-dlp/.test(blocked), blocked);
    const another = await T.core.invoke("youtube_keep_on_album", { id: "albumsong01", slot: { list: half.id, album: "Half Broken", artist: "The Band", track: 1, channel: "The Band" } });
    check("a song on one album gets a copy of its own on another", another.id !== onAlbum[0].id && another.album === "Half Broken" && A.track(onAlbum[0].id).exists);
    const nomix = await T.core.invoke("youtube_playlist", { link: "not a playlist" }).then(() => "", (e) => String(e));
    check("anything else isn't a playlist", /isn't a link to a YouTube playlist/.test(nomix), nomix);
    $("#yt-kind-songs") && $("#yt-kind-songs").click();

    // The player bar goes to what's playing. (Heard in the library: it's in the music folder.)
    A.lib.closeSearch();
    A.setView("playlist");
    A.playTrack(onAlbum[1].id);
    const shown = await until(() => $("#player .np-album"), 5000);
    check("the player bar links its album and artist", !!shown && $("#player .np-album").textContent === "The Big Album" && $("#player .np-artist").textContent === "The Band");
    $("#player .np-album").click();
    await sleep(100);
    check("its album opens the library on all of it", S.view === "library" && $$(".lr").length === 3, `${S.view} ${$$(".lr").length}`);
    $("#player button.np-t").click();
    await sleep(100);
    check("its title finds it there", A.lib.state.L.sel.has(onAlbum[1].id));
    A.stopPlayback();
    A.setView("playlist");

    // The other side of the panel is as it was.
    A.yt.open();
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
