// Searching your music: a song title, an artist, an album, a genre, a label,
// a year or a file name, in any mix. The best matches come first:
//
// * exact: every word of the search is a whole word of the track's tags (or
//   the search is a whole tag), ranked by where the words are (title and
//   artist first) and whether a whole tag matched;
// * close: every word of the search starts a word of the track's tags, is
//   part of one, or is spelled a letter or two differently.
//
// Accents and case don't matter. Pure functions, no DOM.
"use strict";
(function (P2U) {
  /** Fields searched, with how much a match there counts. */
  const FIELDS = [
    ["title", 3], ["artist", 3], ["albumartist", 2], ["album", 2], ["genre", 2], ["labels", 2],
    ["composer", 1], ["year", 1], ["filename", 1],
  ];
  const WEIGHT = Object.fromEntries(FIELDS);

  function norm(s) {
    return String(s == null ? "" : s)
      .normalize("NFD")
      .replace(/[̀-ͯ]/g, "")
      .toLowerCase()
      .replace(/[^\p{L}\p{N}]+/gu, " ")
      .trim();
  }
  function values(t, f) {
    if (f === "labels") return Array.isArray(t.labels) ? t.labels : [];
    if (f === "year") return t.year ? [String(t.year)] : [];
    return t[f] ? [t[f]] : [];
  }

  /** An index of the tracks: their words, and every word's tracks. */
  function build(tracks) {
    const docs = [];
    const vocab = new Map();
    for (const t of tracks) {
      const d = { t, fields: {}, words: new Map() };
      const i = docs.length;
      for (const [f, w] of FIELDS) {
        const vals = values(t, f).map(norm).filter(Boolean);
        d.fields[f] = vals;
        for (const v of vals) {
          for (const word of v.split(" ")) {
            if (!d.words.has(word) || d.words.get(word) < w) d.words.set(word, w);
            let set = vocab.get(word);
            if (!set) vocab.set(word, (set = new Set()));
            set.add(i);
          }
        }
      }
      docs.push(d);
    }
    return { docs, vocab, words: [...vocab.keys()] };
  }

  /** Edit distance with swapped neighbours counted as one edit, or max + 1 once past `max`. */
  function distance(a, b, max) {
    if (Math.abs(a.length - b.length) > max) return max + 1;
    const n = b.length;
    let prev2 = null;
    let prev = Array.from({ length: n + 1 }, (_, j) => j);
    for (let i = 1; i <= a.length; i++) {
      const cur = [i];
      let low = i;
      for (let j = 1; j <= n; j++) {
        let v = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
        if (prev2 && i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) v = Math.min(v, prev2[j - 2] + 1);
        cur.push(v);
        if (v < low) low = v;
      }
      if (low > max) return max + 1;
      prev2 = prev;
      prev = cur;
    }
    return prev[n];
  }

  /** How well a search word matches a word of the tags: 1 exact, less for close, 0 not at all. */
  function closeness(tok, word) {
    if (tok === word) return 1;
    if (tok.length >= 2 && word.startsWith(tok)) return 0.85;
    if (tok.length >= 3 && word.includes(tok)) return 0.7;
    if (tok.length >= 4) {
      const max = tok.length >= 8 ? 2 : 1;
      const d = distance(tok, word, max);
      if (d <= max) return d === 1 ? 0.6 : 0.45;
      // The same stem: "dreams" and "dreamer".
      let p = 0;
      while (p < tok.length && p < word.length && tok[p] === word[p]) p++;
      if (p >= 4 && p >= 0.75 * Math.min(tok.length, word.length)) return 0.5;
    }
    return 0;
  }

  /**
   * Search the index. Returns `{ exact, close }`, each a list of
   * `{ t, score, fields }` (the fields that matched), best first.
   */
  function search(index, query, opts = {}) {
    const limit = opts.limit || 200;
    const q = norm(query);
    const toks = [...new Set(q.split(" ").filter(Boolean))];
    if (!toks.length || !index) return { exact: [], close: [] };
    const { docs, vocab, words } = index;

    // Exact: every word is a whole word of the track's tags.
    let exactIds = null;
    for (const tok of toks) {
      const set = vocab.get(tok) || new Set();
      exactIds = exactIds ? new Set([...exactIds].filter((i) => set.has(i))) : new Set(set);
      if (!exactIds.size) break;
    }
    const exact = [];
    for (const i of exactIds || []) {
      const d = docs[i];
      let score = 0;
      const fields = new Set();
      for (const tok of toks) {
        let best = 0;
        for (const [f, w] of FIELDS) {
          if (d.fields[f].some((v) => (` ${v} `).includes(` ${tok} `))) {
            fields.add(f);
            if (w > best) best = w;
          }
        }
        score += best;
      }
      // A whole tag, or the words in order within one, counts for more.
      for (const [f, w] of FIELDS) {
        for (const v of d.fields[f]) {
          if (v === q) { score += 10 * w; fields.add(f); } else if (toks.length > 1 && (` ${v} `).includes(` ${q} `)) score += 3 * w;
        }
      }
      exact.push({ t: d.t, score, fields: [...fields] });
    }

    // Close: every word starts, is part of, or is nearly a word of the tags.
    let closeScores = null;
    for (const tok of toks) {
      const here = new Map();
      for (const w of words) {
        const c = closeness(tok, w);
        if (!c) continue;
        for (const i of vocab.get(w)) {
          if (exactIds && exactIds.has(i)) continue;
          const v = c * (docs[i].words.get(w) || 1);
          if (!here.has(i) || here.get(i) < v) here.set(i, v);
        }
      }
      if (!closeScores) closeScores = here;
      else {
        const next = new Map();
        for (const [i, v] of here) if (closeScores.has(i)) next.set(i, closeScores.get(i) + v);
        closeScores = next;
      }
      if (!closeScores.size) break;
    }
    const close = [...(closeScores || new Map())].map(([i, score]) => {
      const d = docs[i];
      const fields = FIELDS.map(([f]) => f).filter((f) => d.fields[f].some((v) => toks.some((tok) => v.split(" ").some((w) => closeness(tok, w)))));
      return { t: d.t, score, fields };
    });

    const order = (a, b) => b.score - a.score || norm(a.t.title || a.t.filename).localeCompare(norm(b.t.title || b.t.filename));
    exact.sort(order);
    close.sort(order);
    return { exact: exact.slice(0, limit), close: close.slice(0, Math.min(limit, 100)) };
  }

  P2U.search = { build, search, norm, distance, closeness, FIELDS, WEIGHT };
})(typeof window !== "undefined" ? (window.P2U = window.P2U || {}) : (globalThis.P2U = globalThis.P2U || {}));
