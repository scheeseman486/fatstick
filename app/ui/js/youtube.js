// YouTube: the other side of the search panel. Search it (or paste a link),
// hear a song, and add it to a playlist or the library. Only ever the audio:
// app/src-tauri and crates/fatstick-youtube see to that.
//
// A song is fetched into the cache when it's heard, so nothing lands in the
// music folder until it's asked for. Adding it, by dragging it onto a playlist
// or with its + button, tags it and copies it into the music folder, where it's
// a track like any other. A video that's already been kept is that track.
//
// A playlist can be found the same way, or by its link. Its songs can be added one
// by one, or all of it as an album in the library (see getAlbum), and "Import from
// YouTube" does that and makes a playlist of it too.
//
// The results use the rows, selection and dragging of the music search. Each
// has a key "yt:<video id>", which track() knows too (S.previews), so the player
// can show a song that isn't in the library.
"use strict";
(function (Fatstick) {
  const { h, fill, icon, clock, length, plural, menu, modal, closeModal, toast } = Fatstick.ui;

  Fatstick.youtubeSetup = function (core) {
    const { S, api, lib } = core;
    const SIZE = "about 85 MB";
    const FIRST = 25;
    const MORE = 25;
    const Y = {
      status: null, q: "", ran: "", limit: FIRST, busy: false, error: "", seq: 0,
      hits: [], sel: new Set(), anchor: null, focus: null, lastTap: null,
      /** Songs being fetched or kept, by video id. */
      jobs: new Map(),
      /** The tracks videos became, by video id. */
      kept: new Map(),
      /** What the person last asked to hear, and what was playing then. */
      want: null,
      setup: null,
      /** What to search for, "songs" or "lists". */
      kind: "songs",
      lists: [], listsRan: "", listLimit: FIRST,
      /** The playlist being looked at. */
      open: null,
      /** Playlists coming into the library as albums, by playlist id. */
      albums: new Map(),
    };
    const SONGS_AT_ONCE = 3;
    const listLink = (id) => `https://www.youtube.com/playlist?list=${id}`;
    /** A link to a playlist (a mix never ends, so it's not one), or a playlist's id. */
    const isListText = (q) => /[?&]list=(?!RD)[\w-]{12,}/.test(q) || /^(PL|OLAK5uy_)[\w-]{10,}$/.test(q);
    const isLink = (q) => /^(https?:\/\/)?((www|m|music)\.)?(youtube\.com|youtu\.be)\//i.test(q);
    /** The songs on show: the open playlist's, or a search's. */
    const shown = () => (Y.open ? Y.open.songs : Y.kind === "lists" ? [] : Y.hits);
    const keyOf = (id) => `yt:${id}`;
    /** An error's words: the app sends them as text, the demo as an Error. */
    const said = (e) => (e && e.message) || String(e);
    const idOf = (key) => key.slice(3);

    // ---- selecting and dragging, as in the music search ----------------------------------
    const X = {
      get sel() { return Y.sel; },
      get anchor() { return Y.anchor; },
      set anchor(v) { Y.anchor = v; },
      get focus() { return Y.focus; },
      get lastTap() { return Y.lastTap; },
      set lastTap(v) { Y.lastTap = v; },
      get box() { return Y.list; },
      ids: () => shown().map((x) => keyOf(x.id)),
      ordered: () => X.ids().filter((x) => Y.sel.has(x)),
      range: (a, b, into) => lib.rangeOf(X.ids(), a, b, into),
      set(next, focus) { Y.sel = next; Y.focus = focus; syncRows(); return true; },
      reveal(key) { const el = rowEl(idOf(key)); if (el) el.scrollIntoView({ block: "nearest" }); },
      up() { Y.input.focus(); },
      escape() { Y.input.focus(); },
      play: (key) => hear(key),
      add: (keys) => addTo(keys, core.pl() ? { kind: "playlist", id: S.current } : { kind: "new" }),
      drop: (keys, target) => addTo(keys, target),
    };

    // ---- the panel ---------------------------------------------------------------------------
    function build() {
      Y.tabs = h("div", { class: "srch-tabs", role: "tablist", "aria-label": "Search in" });
      Y.input = h("input", {
        type: "search", class: "input srch-in", id: "yt-in", placeholder: "Search YouTube, or paste a link", "aria-label": "Search YouTube",
        autocomplete: "off", spellcheck: "false",
        oninput: () => { Y.q = Y.input.value; },
        onkeydown: (e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            run(FIRST);
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            const ids = X.ids();
            if (ids.length) { Y.anchor = ids[0]; X.set(new Set([ids[0]]), ids[0]); Y.list.focus(); X.reveal(ids[0]); }
            else { const first = Y.list.querySelector(".yp-open"); if (first) first.focus(); }
          } else if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            if (Y.open) closeList();
            else if (Y.input.value) { Y.input.value = Y.q = ""; refresh(); } else lib.closeSearch();
          }
        },
      });
      Y.scope = h("div", { class: "srch-scope", "aria-live": "polite" });
      Y.list = h("div", {
        class: "srch-res yt-res", role: "listbox", tabindex: "0", "aria-label": "Results from YouTube", "aria-multiselectable": "true",
        onpointerdown: (e) => lib.rowsDown(e, X),
        onkeydown: (e) => { if (lib.listKey(e, X, 8)) e.stopPropagation(); },
        oncontextmenu: rowMenu,
      });
      Y.foot = h("div", { class: "yt-foot" });
      const kind = (key, label) => h("button", { type: "button", class: "btn small yt-kind", id: `yt-kind-${key}`, text: label, onclick: () => setKind(key) });
      Y.kinds = h("div", { class: "yt-kinds", role: "group", "aria-label": "Search for" }, kind("songs", "Songs"), kind("lists", "Playlists"));
      Y.root = h("div", { class: "srch yt" },
        h("div", { class: "srch-box" },
          h("span", { class: "srch-ic" }, icon("search")),
          Y.input,
          h("button", { type: "button", class: "btn quiet small icon srch-close", title: "Close the search (Esc)", "aria-label": "Close the search", onclick: () => lib.closeSearch() }, icon("close"))),
        Y.tabs, Y.kinds, Y.scope, Y.list, Y.foot);
    }
    function root() {
      if (!Y.root) build();
      return Y.root;
    }
    /** The sidebar in search mode, as the music search has it. */
    function renderSearch(side) {
      const r = root();
      if (r.parentNode !== side) fill(side, r);
      fill(Y.tabs, core.searchTabs());
      refresh();
    }
    /** `text`: what was typed to open it, to go on from. */
    function open(text) {
      if (S.view === "library") core.setView("playlist");
      S.searching = true;
      S.searchSource = "youtube";
      core.renderSide();
      document.getElementById("app").classList.add("searching");
      Y.input.focus();
      if (typeof text === "string" && text) {
        Y.input.value = Y.q = text;
        Y.input.setSelectionRange(text.length, text.length);
      } else Y.input.select();
      loadStatus();
    }
    async function loadStatus() {
      try {
        Y.status = await api.youtubeStatus();
      } catch (e) {
        Y.error = said(e);
      }
      refresh();
    }

    function refresh() {
      if (!Y.root) return;
      const st = Y.status;
      const needed = !!st && !st.ytdlp;
      Y.input.disabled = needed;
      Y.kinds.hidden = !st || needed;
      for (const k of ["songs", "lists"]) document.getElementById(`yt-kind-${k}`)?.setAttribute("aria-pressed", String(Y.kind === k));
      Y.input.placeholder = Y.kind === "lists" ? "Search for playlists, or paste a link" : "Search YouTube, or paste a link";
      if (!st) { fill(Y.scope, "Looking for yt-dlp…"); fill(Y.list); fill(Y.foot); return; }
      if (needed) { fill(Y.scope); fill(Y.list, setupCard()); renderFoot(); return; }
      renderScope();
      renderRows();
      renderFoot();
    }
    function renderScope() {
      const st = Y.status;
      const notes = [];
      if (st.ytdlpOld && st.canGet) notes.push(h("p", { class: "yt-note" }, "Your yt-dlp is too old to work with YouTube now. ", getButton("Get a current one")));
      else if (!st.deno && st.canGet) notes.push(h("p", { class: "yt-note" }, "Deno isn't here, so some songs may not come through. ", getButton("Get Deno")));
      let line;
      if (Y.setup) line = setupLine();
      else if (Y.busy) line = h("p", { class: "yt-line busy", text: "Searching YouTube…" });
      else if (Y.error) line = h("p", { class: "yt-line yt-err", role: "alert" }, Y.error, needsUpdate(Y.error) ? h("button", { type: "button", class: "btn quiet small", text: "Update yt-dlp", onclick: update }) : null);
      else if (Y.open) line = null;
      else if (Y.kind === "lists") {
        if (!Y.listsRan) line = h("p", { class: "yt-line", text: "Press Enter to search for playlists. A whole one can go in your library as an album." });
        else if (Y.lists.length) line = h("p", { class: "yt-line", text: `${plural(Y.lists.length, "playlist")} for “${Y.listsRan}”.` });
        else line = h("p", { class: "yt-line", text: `No playlists on YouTube for “${Y.listsRan}”.` });
      } else if (!Y.ran) line = h("p", { class: "yt-line", text: "Press Enter to search. Only the audio is ever fetched, never the video." });
      else if (Y.hits.length) line = h("p", { class: "yt-line", text: `${plural(Y.hits.length, "result")} for “${Y.ran}”.` });
      else line = h("p", { class: "yt-line", text: `Nothing on YouTube for “${Y.ran}”.` });
      fill(Y.scope, notes, line);
    }
    const needsUpdate = (msg) => /updating yt-dlp/i.test(msg) && Y.status && Y.status.ytdlp && Y.status.ytdlp.managed;

    function renderRows() {
      if (Y.open) { fill(Y.list, listHead(Y.open), Y.open.songs.map(row)); return; }
      if (Y.kind === "lists") {
        fill(Y.list, Y.lists.map(listRow), Y.lists.length && Y.lists.length >= Y.listLimit
          ? h("button", { type: "button", class: "btn quiet small yt-more", text: "Show more", onclick: () => run(Y.listLimit + MORE) }) : null);
        return;
      }
      fill(Y.list, Y.hits.map(row), Y.hits.length && Y.ran && Y.hits.length >= Y.limit
        ? h("button", { type: "button", class: "btn quiet small yt-more", text: "Show more", onclick: () => run(Y.limit + MORE) }) : null);
    }

    // ---- playlists: in a search, and opened ------------------------------------------------------
    const running = (j) => !!j && (j.state === "listing" || j.state === "getting");
    function listRow(x) {
      return h("div", { class: "sr yp", id: `yp-${x.id}`, dataset: { list: x.id } },
        h("button", { type: "button", class: "yp-open sr-txt", title: "See its songs", onclick: () => openList(listLink(x.id)) },
          h("span", { class: "sr-t", text: x.title }),
          h("span", { class: "sr-s", text: x.channel })),
        h("span", { class: "sr-r yp-act" }, listAct(x.id)),
        albumBar(x.id));
    }
    /** How far along an album is. */
    function albumBar(id) {
      const j = Y.albums.get(id);
      const pct = j && j.total ? Math.min(100, (100 * j.done) / j.total) : 0;
      return h("span", { class: "yr-bar yp-bar", hidden: !running(j) }, h("i", { style: { width: `${pct}%` } }));
    }
    function listAct(id) {
      const j = Y.albums.get(id);
      if (running(j)) {
        return [
          h("span", { class: "yr-pct num", text: j.state === "listing" ? "…" : `${j.done} of ${j.total}` }),
          h("button", { type: "button", class: "btn quiet small icon yp-stop", title: "Stop getting it", "aria-label": `Stop getting ${j.name || "this playlist"}`, onclick: () => stopAlbum(Y.albums.get(id)) }, icon("close")),
        ];
      }
      if (j && j.state === "done") return h("span", { class: "sr-in", title: "In your library", "aria-label": "In your library" }, icon("check"));
      const failed = j && j.state === "failed";
      return h("button", {
        type: "button", class: `btn quiet small icon yr-add yp-add${failed ? " failed" : ""}`,
        title: failed ? `${j.errors[0].msg} Press to try again.` : "Add to the library as an album", "aria-label": `Add ${(Y.lists.find((x) => x.id === id) || {}).title || "this playlist"} to the library as an album`,
        onclick: () => albumOf(id),
      }, icon("get"));
    }
    function listHead(p) {
      const j = Y.albums.get(p.id);
      const secs = p.songs.reduce((a, s) => a + (s.duration || 0), 0);
      const left = [p.skipped ? `${plural(p.skipped, "song")} can't be fetched (private or deleted).` : "", p.cut ? `Only the first ${p.songs.length} are taken.` : ""].filter(Boolean).join(" ");
      return h("div", { class: "yp-head" },
        h("button", { type: "button", class: "btn quiet small yp-back", text: "‹ Back", onclick: closeList }),
        h("h3", { text: p.title }),
        h("p", { class: "yp-sub", text: `${[p.channel, plural(p.songs.length, "song")].filter(Boolean).join(", ")}, ${length(secs)}. ${left}`.trim() }),
        h("p", { class: "yp-sub", text: `As an album: ${p.album.title}, by ${p.album.artist}.` }),
        running(j) ? h("div", { class: "yt-prog", role: "status" },
          h("span", { id: "yp-prog-text", text: `Getting them: ${j.done} of ${j.total}.` }),
          albumBar(p.id),
          h("button", { type: "button", class: "btn quiet small yp-stop", text: "Stop", onclick: () => stopAlbum(Y.albums.get(p.id)) }))
          : h("div", { class: "yp-do" },
            h("button", { type: "button", class: "btn small primary", id: "yp-album", text: j && j.state === "done" ? "It's in your library" : j && (j.state === "failed" || j.state === "stopped") ? "Get the rest" : "Add to the library as an album", disabled: !!j && j.state === "done", onclick: () => getAlbum(p) }),
            h("button", { type: "button", class: "btn small", id: "yp-import", text: "And as a playlist", title: "Into your library as an album, and into your playlists too", onclick: () => getAlbum(p, { asPlaylist: true }) })));
    }
    /** Something about an album changed: its row, or its head while it's open. */
    function paintAlbum(id) {
      const j = Y.albums.get(id);
      const pct = j && j.total ? `${Math.min(100, (100 * j.done) / j.total)}%` : "0%";
      // While it's going only the numbers change, so "Stop" stays under the pointer.
      const going = j && j.state === "getting";
      const el = document.getElementById(`yp-${id}`);
      if (el) {
        const count = going && el.querySelector(".yp-act .yr-pct");
        if (count) {
          count.textContent = `${j.done} of ${j.total}`;
          el.querySelector(".yp-bar i").style.width = pct;
        } else {
          fill(el.querySelector(".yp-act"), listAct(id));
          el.querySelector(".yp-bar").replaceWith(albumBar(id));
        }
      }
      if (Y.open && Y.open.id === id && Y.list) {
        const text = going && document.getElementById("yp-prog-text");
        if (text) {
          text.textContent = `Getting them: ${j.done} of ${j.total}.`;
          Y.list.querySelector(".yp-head .yp-bar i").style.width = pct;
        } else {
          const head = Y.list.querySelector(".yp-head");
          if (head) head.replaceWith(listHead(Y.open));
        }
      }
    }
    function setKind(kind) {
      Y.kind = kind;
      Y.open = null;
      Y.error = "";
      const q = Y.input.value.trim();
      const ran = kind === "lists" ? Y.listsRan : Y.ran;
      if (q && q !== ran && !isLink(q)) run(FIRST);
      else refresh();
    }
    /** Remember a song found, for the player and the rows. */
    function found(x) {
      if (x.trackId) Y.kept.set(x.id, x.trackId);
      else Y.kept.delete(x.id);
      S.previews.set(keyOf(x.id), { id: keyOf(x.id), title: x.title, artist: x.channel, album: "", filename: x.title, duration: x.duration, exists: true, path: "", codec: "", labels: [] });
    }
    async function listing(link) {
      const p = await api.youtubePlaylist(link);
      p.songs.forEach(found);
      return p;
    }
    async function openList(link) {
      const seq = ++Y.seq;
      Y.busy = true;
      Y.error = "";
      refresh();
      try {
        const p = await listing(link);
        if (seq !== Y.seq) return;
        Y.open = p;
        Y.sel = new Set();
        Y.anchor = Y.focus = null;
        Y.list.scrollTop = 0;
      } catch (e) {
        if (seq !== Y.seq) return;
        Y.error = said(e);
      }
      Y.busy = false;
      refresh();
    }
    function closeList() {
      Y.open = null;
      Y.sel = new Set();
      Y.anchor = Y.focus = null;
      refresh();
      Y.input.focus();
    }
    function listMenu(e, id) {
      e.preventDefault();
      const j = Y.albums.get(id);
      menu({ x: e.clientX, y: e.clientY }, [
        { label: "See its songs", action: () => openList(listLink(id)) },
        "-",
        { label: "Add to the library as an album", action: () => albumOf(id), disabled: running(j) || (!!j && j.state === "done") },
        { label: "Add as an album and a playlist", action: () => albumOf(id, { asPlaylist: true }), disabled: running(j) },
        running(j) ? { label: "Stop getting it", action: () => stopAlbum(j) } : null,
        "-",
        { label: "Copy the link", action: () => navigator.clipboard.writeText(listLink(id)).then(() => toast("Link copied."), () => toast("The link couldn't be copied.")) },
      ].filter(Boolean));
    }
    const views = (n) => {
      if (n == null) return "";
      const f = (x, u) => `${x < 10 ? x.toFixed(1).replace(/\.0$/, "") : Math.round(x)}${u}`;
      if (n >= 1e9) return `${f(n / 1e9, "B")} views`;
      if (n >= 1e6) return `${f(n / 1e6, "M")} views`;
      if (n >= 1e3) return `${f(n / 1e3, "K")} views`;
      return `${n} views`;
    };
    const rowEl = (id) => document.getElementById(`yr-${id}`);
    function row(x) {
      const key = keyOf(x.id);
      const on = Y.sel.has(key);
      const playing = S.player.trackId === key;
      return h("div", {
        class: `sr yr${on ? " sel" : ""}${Y.focus === key ? " focus" : ""}${playing ? " playing" : ""}`, role: "option", id: `yr-${x.id}`,
        "aria-selected": String(on), dataset: { tid: key },
      },
      h("div", { class: "sr-txt" },
        h("span", { class: "sr-t" }, playing ? h("span", { class: `eq${S.player.playing ? "" : " paused"}`, "aria-label": S.player.playing ? "Playing" : "Paused" }, h("i"), h("i"), h("i")) : null, x.title),
        h("span", { class: "sr-s", text: [x.channel, views(x.views)].filter(Boolean).join(", ") }),
        barOf(x.id)),
      h("span", { class: "sr-r" },
        h("span", { class: "yr-act" }, act(x.id)),
        h("span", { class: "sr-time num", text: clock(x.duration) })));
    }
    /** The thin bar under a row while its song is on its way. */
    function barOf(id) {
      const j = Y.jobs.get(id);
      const pct = j && j.total ? Math.min(100, (100 * j.done) / j.total) : 0;
      return h("span", { class: "yr-bar", hidden: !(j && j.stage === "getting") }, h("i", { style: { width: `${pct}%` } }));
    }
    /** The row's right side: what's happening to it, and what can be done. */
    function act(id) {
      const j = Y.jobs.get(id);
      if (keptTrack(id)) return h("span", { class: "sr-in", title: "In your library", "aria-label": "In your library" }, icon("check"));
      if (j && (j.stage === "getting" || j.stage === "waiting")) {
        return [
          h("span", { class: "yr-pct num", text: j.total ? `${Math.floor((100 * j.done) / j.total)}%` : "…" }),
          h("button", { type: "button", class: "btn quiet small icon", title: "Stop getting it", "aria-label": `Stop getting ${titleOf(id)}`, onclick: () => cancel(id) }, icon("close")),
        ];
      }
      if (j && j.stage === "saving") return h("span", { class: "yr-pct", text: "Saving" });
      const failed = j && j.stage === "failed";
      return h("button", {
        type: "button", class: `btn quiet small icon yr-add${failed ? " failed" : ""}`,
        title: failed ? `${j.error} Press to try again.` : "Add to the library", "aria-label": `Add ${titleOf(id)} to the library`,
        onclick: () => keepAll([keyOf(id)]),
      }, icon("get"));
    }
    function paint(id) {
      const el = rowEl(id);
      if (!el) return;
      fill(el.querySelector(".yr-act"), act(id));
      el.querySelector(".yr-bar").replaceWith(barOf(id));
    }
    function syncRows() {
      if (!Y.list) return;
      for (const r of Y.list.querySelectorAll(".yr")) {
        const on = Y.sel.has(r.dataset.tid);
        r.classList.toggle("sel", on);
        r.classList.toggle("focus", r.dataset.tid === Y.focus);
        r.setAttribute("aria-selected", String(on));
      }
      if (Y.focus) Y.list.setAttribute("aria-activedescendant", `yr-${idOf(Y.focus)}`);
      else Y.list.removeAttribute("aria-activedescendant");
    }
    /** The player started, paused or stopped: mark the song. */
    function markPlaying() {
      if (S.searching && S.searchSource === "youtube" && Y.root && Y.status && Y.status.ytdlp) renderRows();
    }

    function renderFoot() {
      const st = Y.status;
      if (!st || !st.ytdlp) { fill(Y.foot); return; }
      const hint = Y.kind === "lists" && !Y.open ? "Open a playlist to see its songs, or add it all as an album." : "Double-click a song to hear it. Drag it into a playlist to add it.";
      const root = S.music && S.music.root;
      // The folder as it sits in the music folder ("YouTube", "Incoming/YouTube"), if it isn't that itself.
      const where = st.folder && root && st.folder.startsWith(root) ? st.folder.slice(root.length + 1) : "";
      fill(Y.foot,
        h("p", { class: "srch-foot", text: hint }),
        h("p", { class: "yt-where" },
          !st.folder ? "Added songs go in your music folder, once you've chosen it. " : where ? `Added songs go in ${where}, in your music folder. ` : "Added songs go straight into your music folder. ",
          st.folder ? h("button", { type: "button", class: "btn quiet small inline", text: "Change…", onclick: changeFolder }) : null),
        h("p", { class: "yt-where" },
          `yt-dlp ${st.ytdlp.version}${st.deno ? `, Deno ${st.deno.version}` : ""}. `,
          st.ytdlp.managed ? h("button", { type: "button", class: "btn quiet small inline", id: "yt-update", text: "Update", onclick: update, disabled: !!Y.setup }) : null));
    }
    async function changeFolder() {
      const path = await api.pickFolder("Where songs from YouTube go");
      if (!path) return;
      try {
        Y.status = await api.youtubeSetFolder(path);
        renderFoot();
      } catch (e) {
        toast(said(e));
      }
    }

    // ---- getting yt-dlp and Deno ----------------------------------------------------------------
    function setupCard() {
      const st = Y.status;
      if (!st.canGet) {
        return h("div", { class: "yt-setup" },
          h("h3", { text: "YouTube needs yt-dlp" }),
          h("p", { text: "There's no ready-made copy for this system. Install yt-dlp yourself and put it on the PATH, then come back here." }));
      }
      return h("div", { class: "yt-setup" },
        h("h3", { text: "YouTube needs two programs" }),
        h("p", { text: "yt-dlp searches YouTube and fetches the audio. Deno runs the code YouTube asks it to run. They don't come with Fatstick, because YouTube changes often and yt-dlp has to keep up with it." }),
        Y.setup ? setupLine() : getButton(`Get them (${SIZE})`, true),
        Y.error ? h("p", { class: "yt-err", role: "alert", text: Y.error }) : null,
        h("p", { class: "yt-small", text: "From their official releases on GitHub, checked against the published checksums, and kept in Fatstick's own folder. A copy of yt-dlp on your PATH is used if you already have one." }));
    }
    function getButton(label, primary) {
      return h("button", { type: "button", class: `btn ${primary ? "primary" : "quiet small"}`, id: "yt-get", text: label, onclick: setUp, disabled: !!Y.setup });
    }
    function setupLine() {
      const p = Y.setup;
      return h("div", { class: "yt-prog", role: "status" },
        h("span", { id: "yt-prog-text", text: setupText(p) }),
        h("span", { class: "yr-bar" }, h("i", { id: "yt-prog-bar" })),
        h("button", { type: "button", class: "btn quiet small", text: "Cancel", onclick: () => api.youtubeCancelSetup() }));
    }
    const mb = (n) => `${Math.round(n / 1e6)} MB`;
    const setupText = (p) => (p.what ? `Getting ${p.what}${p.total ? `: ${mb(p.done)} of ${mb(p.total)}` : ""}…` : "Starting…");
    function onSetupProgress(p) {
      if (!Y.setup) return;
      Y.setup = p;
      const t = document.getElementById("yt-prog-text");
      if (t) t.textContent = setupText(p);
      const b = document.getElementById("yt-prog-bar");
      if (b && p.total) b.style.setProperty("width", `${Math.min(100, (100 * p.done) / p.total)}%`);
    }
    async function setUp() {
      if (Y.setup) return;
      Y.setup = { what: "", done: 0, total: 0 };
      Y.error = "";
      refresh();
      try {
        Y.status = await api.youtubeSetup();
        Y.setup = null;
        Y.error = "";
        refresh();
        if (Y.input.disabled === false) Y.input.focus();
      } catch (e) {
        Y.setup = null;
        Y.error = /^Cancelled/.test(said(e)) ? "" : said(e);
        await loadStatus();
      }
    }
    async function update() {
      if (Y.setup) return;
      Y.setup = { what: "", done: 0, total: 0 };
      Y.error = "";
      refresh();
      try {
        const r = await api.youtubeUpdate();
        Y.status = r.status;
        toast(r.changed ? `yt-dlp is now ${r.status.ytdlp.version}.` : `yt-dlp ${r.status.ytdlp.version} is the newest there is.`);
      } catch (e) {
        if (!/^Cancelled/.test(said(e))) toast(said(e));
      }
      Y.setup = null;
      refresh();
    }

    // ---- searching ------------------------------------------------------------------------------
    async function run(limit) {
      const q = Y.input.value.trim();
      if (!q || !Y.status || !Y.status.ytdlp) return;
      if (isListText(q)) { await openList(q); return; }
      // A link to a song is a song, whichever is being searched for.
      if (isLink(q)) Y.kind = "songs";
      const seq = ++Y.seq;
      Y.busy = true;
      Y.error = "";
      Y.open = null;
      refresh();
      try {
        if (Y.kind === "lists") {
          const lists = await api.youtubeSearchLists(q, limit);
          if (seq !== Y.seq) return;
          Y.lists = lists;
          Y.listsRan = q;
          Y.listLimit = limit;
        } else {
          const hits = await api.youtubeSearch(q, limit);
          if (seq !== Y.seq) return;
          hits.forEach(found);
          Y.hits = hits;
          Y.ran = q;
          Y.limit = limit;
          Y.sel = new Set();
          Y.anchor = Y.focus = null;
        }
        Y.list.scrollTop = 0;
      } catch (e) {
        if (seq !== Y.seq) return;
        Y.error = said(e);
      }
      Y.busy = false;
      refresh();
    }

    // ---- fetching, hearing and keeping ----------------------------------------------------------
    const titleOf = (id) => (S.previews.get(keyOf(id)) || { title: id }).title;
    /** The library's track for a video, if it's still there. */
    function keptTrack(id) {
      const tid = Y.kept.get(id);
      return tid && S.tracks.has(tid) && S.tracks.get(tid).exists ? tid : null;
    }
    /** The song's audio in the cache; asking again joins the same download.
     * Once it's done the app is asked each time, as the cache can be cleared. */
    function fetched(id) {
      const was = Y.jobs.get(id);
      if (was && was.promise) return was.promise;
      const j = { stage: "waiting", done: 0, total: 0, error: "" };
      Y.jobs.set(id, j);
      j.promise = api.youtubeFetch(id).then(
        (f) => { j.promise = null; j.stage = "ready"; paint(id); return f; },
        (e) => {
          j.promise = null;
          if (/^Cancelled/.test(said(e))) Y.jobs.delete(id);
          else { j.stage = "failed"; j.error = said(e); }
          paint(id);
          throw e;
        },
      );
      paint(id);
      return j.promise;
    }
    function onProgress({ id, done, total }) {
      const j = Y.jobs.get(id);
      if (!j || j.stage === "ready" || j.stage === "saving") return;
      Object.assign(j, { stage: "getting", done, total });
      paint(id);
    }
    function cancel(id) {
      if (Y.want === keyOf(id)) Y.want = null;
      api.youtubeCancel(id).catch(() => {});
    }
    /** What went wrong, saying what can be done about it. */
    function failed(e, id) {
      const msg = said(e);
      if (/^Cancelled/.test(msg)) return;
      const what = id ? `${titleOf(id)}: ` : "";
      if (needsUpdate(msg)) toast(`${what}${msg}`, { label: "Update yt-dlp", run: update });
      else toast(`${what}${msg}`);
    }
    /** Hear a song: from the library if it's kept, or once it's fetched. It
     * doesn't cut in on anything started while it was on its way. */
    async function hear(key) {
      const id = idOf(key);
      const tid = keptTrack(id);
      if (tid) { core.playTrack(tid); return; }
      const then = S.player.token;
      Y.want = key;
      try {
        await fetched(id);
      } catch (e) {
        failed(e, id);
        return;
      }
      if (Y.want === key && S.player.token === then) core.playTrack(key);
      if (Y.want === key) Y.want = null;
    }
    /** Songs go in the music folder: choose it now if it hasn't been. */
    async function needFolder() {
      if (S.music && S.music.root) return true;
      toast("Songs from YouTube go in your music folder. Choose it first.");
      const path = await api.pickFolder("Your music folder");
      if (!path) return false;
      const ok = await lib.useFolder(path);
      if (ok) Y.status = await api.youtubeStatus();
      refresh();
      return ok;
    }
    /** One song into the library; its track's id. */
    async function keep(id) {
      const tid = keptTrack(id);
      if (tid) return tid;
      await fetched(id);
      const j = Y.jobs.get(id);
      if (j) j.stage = "saving";
      paint(id);
      try {
        const t = await api.youtubeKeep(id);
        Y.kept.set(id, t.id);
        core.putTracks([t]);
        if (j) j.stage = "ready";
        paint(id);
        return t.id;
      } catch (e) {
        if (j) { j.stage = "failed"; j.error = said(e); }
        paint(id);
        throw e;
      }
    }
    /** Several at once, in order. Returns the tracks that came through, and
     * says what didn't. */
    async function keepEach(ids) {
      const got = await Promise.allSettled(ids.map(keep));
      const bad = got.map((r, i) => [r, ids[i]]).filter(([r]) => r.status === "rejected");
      if (bad.length === 1) failed(bad[0][0].reason, bad[0][1]);
      else if (bad.length) failed(`${bad.length} songs didn't come through. ${bad[0][0].reason}`);
      return got.filter((r) => r.status === "fulfilled").map((r) => r.value);
    }
    async function keepAll(keys) {
      const ids = keys.map(idOf);
      if (!(await needFolder())) return;
      const tids = await keepEach(ids);
      if (tids.length === 1) toast(`In your library: ${titleOf(ids[0])}.`, { label: "Show", run: () => lib.revealInLibrary(tids[0]) });
      else if (tids.length) toast(`${tids.length} songs added to your library.`);
    }
    /** Into a playlist when they've arrived. A place in the list stays that
     * playlist's place, whatever is open by then. */
    async function addTo(keys, target) {
      const ids = keys.map(idOf);
      if (!ids.length || !(await needFolder())) return;
      const name = ids.length === 1 ? titleOf(ids[0]) : "From YouTube";
      let to = target;
      if (target.kind === "list") to = { kind: "playlist", id: S.current, index: target.index };
      const where = to.kind === "new" ? "a new playlist" : (core.pl(to.id) || { name: "the playlist" }).name;
      if (ids.some((id) => !keptTrack(id))) toast(`Getting ${ids.length === 1 ? titleOf(ids[0]) : plural(ids.length, "song")}, for ${where}.`);
      const tids = await keepEach(ids);
      if (!tids.length) return;
      // The playlist may have gone in the meantime.
      core.insertTracks(tids, to.kind === "playlist" && !core.pl(to.id) ? { kind: "new" } : to, name);
    }
    // ---- a playlist as an album -----------------------------------------------------------------
    /** One song of a playlist into the library, in its place on the album. */
    async function keepOnAlbum(song, slot) {
      const id = song.id;
      const j = { stage: "waiting", done: 0, total: 0, error: "" };
      Y.jobs.set(id, j);
      paint(id);
      try {
        const t = await api.youtubeKeepOnAlbum(id, slot);
        Y.kept.set(id, t.id);
        core.putTracks([t]);
        j.stage = "ready";
        paint(id);
        return t.id;
      } catch (e) {
        if (/^Cancelled/.test(said(e))) Y.jobs.delete(id);
        else { j.stage = "failed"; j.error = said(e); }
        paint(id);
        throw e;
      }
    }
    function stopAlbum(job) {
      job.stop = true;
      for (const id of job.going) api.youtubeCancel(id).catch(() => {});
    }
    /** The playlist a row is for, into the library. */
    async function albumOf(id, opts) {
      if (running(Y.albums.get(id))) return;
      const was = Y.albums.get(id);
      const job = { state: "listing", name: "", done: 0, total: 0, errors: [], going: new Set(), stop: false };
      Y.albums.set(id, job);
      paintAlbum(id);
      let p;
      try {
        p = await listing(listLink(id));
      } catch (e) {
        if (was) Y.albums.set(id, was);
        else Y.albums.delete(id);
        paintAlbum(id);
        failed(e);
        return;
      }
      if (job.stop) { Y.albums.delete(id); paintAlbum(id); return; }
      getAlbum(p, opts);
    }
    /**
     * A whole playlist into the library as one album, a few songs at a time: each
     * tagged with the album, its artist and its place, in the album's own folder.
     * What's there already stays, so asking again gets what didn't come through.
     * `asPlaylist`: it goes in the playlist list too, filling up as songs arrive.
     */
    async function getAlbum(p, { asPlaylist = false } = {}) {
      const before = Y.albums.get(p.id);
      if (before && before.state === "getting") return;
      if (!(await needFolder())) { if (before && before.state === "listing") Y.albums.delete(p.id); paintAlbum(p.id); return; }
      const job = {
        state: "getting", name: p.album.title, done: 0, total: p.songs.length, errors: [], going: new Set(), stop: false,
        // Trying an import again goes on filling the same playlist.
        playlistId: before && before.playlistId && core.pl(before.playlistId) ? before.playlistId : null,
      };
      job.cancel = () => stopAlbum(job);
      Y.albums.set(p.id, job);
      if (job.playlistId) core.importing(job.playlistId, job);
      else if (asPlaylist) job.playlistId = core.beginImport(p.name || p.album.title, job);
      paintAlbum(p.id);
      const tids = new Array(p.songs.length).fill(null);
      let next = 0;
      const worker = async () => {
        while (!job.stop && next < p.songs.length) {
          const i = next++;
          const song = p.songs[i];
          job.going.add(song.id);
          try {
            tids[i] = await keepOnAlbum(song, { list: p.id, album: p.album.title, artist: p.album.artist, track: i + 1, channel: song.channel || "" });
            job.done++;
            if (job.playlistId) core.importArrived(job.playlistId, tids[i], tids.slice(0, i).filter(Boolean).length);
          } catch (e) {
            if (!/^Cancelled/.test(said(e))) job.errors.push({ title: song.title, msg: said(e) });
          }
          job.going.delete(song.id);
          paintAlbum(p.id);
          if (job.playlistId) core.importChanged(job.playlistId);
        }
      };
      await Promise.all(Array.from({ length: SONGS_AT_ONCE }, worker));
      job.state = job.stop ? "stopped" : job.errors.length ? "failed" : "done";
      if (job.playlistId) core.endImport(job.playlistId);
      paintAlbum(p.id);
      const show = { label: "Show", run: () => lib.showBy("album", p.album.title) };
      const where = job.playlistId ? "your library and your playlists" : "your library";
      if (job.stop) toast(`Stopped. ${job.done} of ${plural(job.total, "song")} from ${p.album.title} are in ${where}.`, job.done ? show : undefined);
      else if (job.errors.length) {
        const e = job.errors[0];
        toast(`${job.done} of ${plural(job.total, "song")} from ${p.album.title} came through. ${job.errors.length === 1 ? `${e.title}: ` : ""}${e.msg}`, needsUpdate(e.msg) ? { label: "Update yt-dlp", run: update } : job.done ? show : undefined);
      } else toast(`${p.album.title} is in ${where}: ${plural(job.done, "song")}.`, show);
    }
    /** "Import from YouTube": asks for a playlist, and fetches it as an album and a playlist. */
    async function importDialog() {
      await loadStatus();
      if (!Y.status || !Y.status.ytdlp) {
        open();
        toast("YouTube needs two programs first. Get them here, then import again.");
        return;
      }
      if (!(await needFolder())) return;
      let gone = false;
      const err = h("p", { class: "yt-err", id: "yt-import-err", role: "alert", hidden: true });
      const go = h("button", { type: "button", class: "btn primary", id: "yt-import-go", text: "Import", onclick: () => start() });
      const input = h("input", {
        type: "text", class: "input", id: "yt-import", "data-autofocus": "", autocomplete: "off", spellcheck: "false", placeholder: "https://www.youtube.com/playlist?list=…",
        onkeydown: (e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); start(); } },
      });
      async function start() {
        const text = input.value.trim();
        if (!text || go.disabled) { input.focus(); return; }
        go.disabled = true;
        go.textContent = "Looking it up…";
        err.hidden = true;
        try {
          const p = await listing(text);
          if (gone) return;
          closeModal();
          getAlbum(p, { asPlaylist: true });
        } catch (e) {
          if (gone) return;
          err.textContent = said(e);
          err.hidden = false;
          go.disabled = false;
          go.textContent = "Import";
          input.focus();
        }
      }
      modal({
        title: "Import from YouTube",
        body: [
          h("p", { text: "The playlist's songs go in your library as an album, and in your playlists as a playlist. Only the audio is fetched." }),
          h("label", { class: "yt-import-f" }, h("span", { text: "Link to a YouTube playlist" }), input),
          err,
          h("p", { class: "yt-small" }, "Don't have a link? ", h("button", { type: "button", class: "btn quiet small inline", id: "yt-import-find", text: "Search YouTube's playlists", onclick: () => { closeModal(); open(); setKind("lists"); Y.input.focus(); } })),
        ],
        buttons: [h("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal }), go],
        onClose: () => { gone = true; },
      });
    }

    function rowMenu(e) {
      const pl = e.target.closest("[data-list]");
      if (pl) { listMenu(e, pl.dataset.list); return; }
      const r = e.target.closest("[data-tid]");
      if (!r) return;
      e.preventDefault();
      const key = r.dataset.tid;
      if (!Y.sel.has(key)) X.set(new Set([key]), key);
      const keys = X.ordered();
      const n = keys.length;
      const id = idOf(key);
      const p = core.pl();
      const others = S.playlists.filter((x) => !p || x.id !== p.id);
      const tid = keptTrack(id);
      const j = Y.jobs.get(id);
      const going = j && (j.stage === "getting" || j.stage === "waiting");
      menu({ x: e.clientX, y: e.clientY }, [
        { label: "Play", key: "Enter", action: () => hear(key) },
        going ? { label: "Stop getting it", action: () => cancel(id) } : null,
        "-",
        p ? { label: `Add to ${p.name}`, key: "Shift+Enter", action: () => addTo(keys, { kind: "playlist", id: p.id }) } : null,
        ...(others.length ? [{ heading: n === 1 ? "Add to playlist" : `Add ${n} songs to` }, ...others.map((x) => ({ label: x.name, action: () => addTo(keys, { kind: "playlist", id: x.id }) }))] : []),
        { label: "New playlist from these", action: () => addTo(keys, { kind: "new" }) },
        "-",
        { label: n === 1 ? "Add to the library" : `Add ${n} songs to the library`, action: () => keepAll(keys), disabled: keys.every((k) => keptTrack(idOf(k))) },
        tid ? { label: "Show in the library", action: () => lib.revealInLibrary(tid) } : null,
        { label: "Copy the link", action: () => navigator.clipboard.writeText(`https://www.youtube.com/watch?v=${id}`).then(() => toast("Link copied."), () => toast("The link couldn't be copied.")) },
      ].filter(Boolean));
    }

    api.on("youtube-progress", onProgress);
    api.on("youtube-setup", onSetupProgress);
    api.on("youtube-updated", (st) => { Y.status = st; if (Y.root) renderFoot(); });

    /** The player's song, in the panel. */
    function showHeard(key) {
      open();
      const el = rowEl(idOf(key));
      if (el) el.scrollIntoView({ block: "nearest" });
    }

    return { open, renderSearch, refresh, markPlaying, addTo, importDialog, showHeard, state: Y };
  };
})(window.Fatstick = window.Fatstick || {});
