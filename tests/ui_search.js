// The search rules in app/ui/js/search.js.
//   node tests/ui_search.js
const assert = require("assert");
require("../app/ui/js/search.js");
const Q = globalThis.P2U.search;

const t = (id, title, artist, extra = {}) => ({ id, title, artist, album: "", genre: "", labels: [], filename: title, ...extra });
const tracks = [
  t("1", "Dreams", "Fleetwood Mac", { album: "Rumours", genre: "Rock", year: 1977, labels: ["road trip"] }),
  t("2", "Go Your Own Way", "Fleetwood Mac", { album: "Rumours", genre: "Rock", year: 1977 }),
  t("3", "Dream On", "Aerosmith", { album: "Aerosmith", genre: "Hard Rock" }),
  t("4", "Sweet Dreams (Are Made of This)", "Eurythmics", { labels: ["chill", "80s"] }),
  t("5", "Hoppípolla", "Sigur Rós", { album: "Takk...", labels: ["chill"] }),
  t("6", "Teardrop", "Massive Attack", { album: "Mezzanine", genre: "Trip Hop", labels: ["chill"] }),
  t("7", "Dreamer", "Supertramp"),
];
const idx = Q.build(tracks);
const ids = (r) => r.map((x) => x.t.id);

// A title: the track with that whole title first, then other exact ones.
let r = Q.search(idx, "dreams");
assert.deepStrictEqual(ids(r.exact), ["1", "4"]);
assert.ok(ids(r.close).includes("7") && ids(r.close).includes("3"), "Dreamer and Dream On are close");
// Accents and case don't matter.
assert.deepStrictEqual(ids(Q.search(idx, "HOPPIPOLLA").exact), ["5"]);
assert.deepStrictEqual(ids(Q.search(idx, "sigur ros").exact), ["5"]);
// An artist, then a mix of artist and title words in any order.
assert.deepStrictEqual(ids(Q.search(idx, "fleetwood mac").exact).sort(), ["1", "2"]);
assert.deepStrictEqual(ids(Q.search(idx, "dreams fleetwood").exact), ["1"]);
// A label, a genre, a year.
assert.deepStrictEqual(ids(Q.search(idx, "chill").exact).sort(), ["4", "5", "6"]);
assert.deepStrictEqual(ids(Q.search(idx, "trip hop").exact), ["6"]);
assert.ok(ids(Q.search(idx, "road trip").exact)[0] === "1", "a whole label");
assert.deepStrictEqual(ids(Q.search(idx, "1977").exact).sort(), ["1", "2"]);
// Typos and beginnings of words are close matches.
assert.deepStrictEqual(ids(Q.search(idx, "fleetwod").close).sort(), ["1", "2"]);
assert.deepStrictEqual(ids(Q.search(idx, "teadrop").close), ["6"]);
assert.deepStrictEqual(ids(Q.search(idx, "mezz").close), ["6"]);
assert.deepStrictEqual(Q.search(idx, "mezz").exact, []);
// Exact matches aren't repeated among the close ones.
r = Q.search(idx, "rock");
assert.ok(!ids(r.close).some((i) => ids(r.exact).includes(i)));
// A whole-title match ranks above a word inside a longer title.
r = Q.search(idx, "dream on");
assert.strictEqual(r.exact[0].t.id, "3");
// Nothing to search, nothing found.
assert.deepStrictEqual(Q.search(idx, "  "), { exact: [], close: [] });
assert.deepStrictEqual(Q.search(idx, "zzzzzz"), { exact: [], close: [] });
assert.strictEqual(Q.distance("teardrop", "teadrop", 2), 1);
assert.strictEqual(Q.distance("form", "from", 1), 1, "a swap is one edit");
// Fast enough for a big library: 20,000 tracks.
const big = [];
for (let i = 0; i < 20000; i++) big.push(t(`b${i}`, `Song number ${i} of the ${["red", "blue", "green", "gold"][i % 4]} sky`, `Artist ${i % 900}`, { album: `Album ${i % 1500}`, labels: i % 7 ? [] : ["chill"] }));
const t0 = Date.now();
const bigIdx = Q.build(big);
const t1 = Date.now();
const br = Q.search(bigIdx, "artst 42 gren");
const t2 = Date.now();
assert.ok(br.close.length > 0);
assert.ok(t1 - t0 < 3000 && t2 - t1 < 1000, `index ${t1 - t0} ms, search ${t2 - t1} ms`);
console.log(`ui_search: all passed (20,000 tracks: index ${t1 - t0} ms, search ${t2 - t1} ms)`);
