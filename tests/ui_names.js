// The name template and FAT clean-up rules in app/ui/js/names.js.
//   node tests/ui_names.js
const assert = require("assert");
require("../app/ui/js/names.js");
const N = globalThis.P2U.names;

const e = (title, artist, filename = "x") => ({ tags: { title, artist }, filename });
const names = (entries, opts) => N.fileNames(entries, opts).names.map((x) => x.name);
const DEF = "{n} - [{artist} - ]{title|filename}";

// The design doc's own examples.
assert.deepStrictEqual(names([e("Teardrop", "Massive Attack")], { template: DEF }), ["01 - Massive Attack - Teardrop.mp3"]);
const bare = N.fileNames([e("", "", "massive_attack-teardrop")], { template: DEF }).names[0];
assert.strictEqual(bare.name, "01 - massive_attack-teardrop.mp3");
assert.deepStrictEqual(bare.notes, ["no title tag, used the file name"]);
// {n} pads to the playlist's length, at least two digits; {n:3} pads to three.
assert.strictEqual(names(Array.from({ length: 120 }, () => e("T", "")), { template: "{n}" })[0], "001.mp3");
assert.strictEqual(names([e("T", "")], { template: "{n:3} {title}" })[0], "001 T.mp3");
// FAT-forbidden characters, spaces, trailing dots.
assert.strictEqual(names([e('What? / "Now"...', "")], { template: "{title}" })[0], "What_ _ _Now_.mp3");
// Windows device names.
assert.strictEqual(names([e("CON", "")], { template: "{title}" })[0], "CON_.mp3");
// ASCII and maximum length keep the number and the extension.
const r = N.fileNames([e("Shine On You Crazy Diamond (Parts I–V)", "Sigur Rós")], { template: DEF, ascii: true, maxLen: 32 }).names[0];
assert.strictEqual(r.name, "01 - Sigur Ros - Shine On.mp3");
assert.ok(r.notes.includes("ó became o") && r.notes.includes("shortened"));
// Clashing names (FAT is case-insensitive) get (2), (3).
assert.deepStrictEqual(names([e("a", ""), e("A", ""), e("a", "")], { template: "{title}" }), ["a.mp3", "A (2).mp3", "a (3).mp3"]);
// Errors and the missing-number warning.
assert.deepStrictEqual(N.parse("{n} {foo} [{artist}").errors, ["{foo} isn't a field.", "A [ has no closing ]."]);
assert.strictEqual(N.fileNames([e("T", "")], { template: "{title}" }).numbered, false);
// Folders and the volume label.
assert.deepStrictEqual(N.folderNames([{ name: "Road Trip" }, { name: "Gym" }], { template: "{n} {playlist}" }).names.map((x) => x.name), ["01 Road Trip", "02 Gym"]);
assert.strictEqual(N.volumeLabel("Kids' car stick"), "KIDS' CAR S");
// File names in the music folder: own extensions, own folders, {n} is the track.
{
  const lib = (title, artist, track, filename, ext, dir) => ({ tags: { title, artist, track }, filename, ext, dir });
  const r = N.libraryNames([
    lib("Dreams", "Fleetwood Mac", 2, "dreams", ".flac", "/m/a"),
    lib("Dreams", "Fleetwood Mac", 2, "dreams copy", ".flac", "/m/a"),
    lib("Dreams", "Fleetwood Mac", 2, "dreams", ".mp3", "/m/b"),
    lib("", "", null, "track07", ".opus", "/m/c"),
    lib("AC/DC: Live?", "", 1, "x", ".mp3", "/m/d"),
  ], { template: "[{n} - ]{title|filename}" });
  assert.deepStrictEqual(r.names.map((x) => x.name), ["02 - Dreams.flac", "02 - Dreams (2).flac", "02 - Dreams.mp3", "track07.opus", "01 - AC_DC_ Live_.mp3"]);
  assert.ok(r.names[1].notes.some((x) => /same name/.test(x)));
}
console.log("ui_names: all passed");
