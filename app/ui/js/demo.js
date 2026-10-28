// Demo data for the window in a browser: a small library, four playlists,
// three profiles, two sticks to sync and four USB sticks, two of them written
// by Playlist2USB. Tags and lengths are real; paths, sizes and loudness
// figures are made up but plausible.
"use strict";
(function (P2U) {
  const KBPS = { flac16: 880, flac24: 2900, alac: 860, wav: 1411 };

  function secs(mss) {
    const [m, s] = mss.split(":").map(Number);
    return m * 60 + s;
  }
  function slug(s) {
    return s.replace(/[\\/:*?"<>|]/g, "").trim();
  }

  // fmt: "mp3 320", "mp3 vbr 245/320", "flac 16/44.1", "flac 24/96", "alac", "wav",
  //      "aac 256", "opus 160", "vorbis 192", "wma 192"
  function track(id, artist, title, album, year, no, fmt, len, lufs, truePeak, more = {}) {
    const [codec, a, b] = fmt.split(" ");
    const t = {
      id, title, artist, album, albumartist: artist, year, track: no, disc: null, genre: more.genre || "",
      codec, bitrate: 0, peakBitrate: 0, vbr: false, sampleRate: 44100, bits: 16, channels: 2,
      duration: secs(len), lufs, truePeak, cover: codec !== "wav", exists: true, drm: false,
    };
    if (codec === "mp3" && a === "vbr") {
      const [avg, peak] = b.split("/").map(Number);
      Object.assign(t, { vbr: true, bitrate: avg, peakBitrate: peak });
    } else if (["mp3", "aac", "opus", "vorbis", "wma"].includes(codec)) {
      t.bitrate = Number(a);
      t.peakBitrate = t.bitrate;
      if (codec === "opus") t.sampleRate = 48000;
    } else if (codec === "flac") {
      const [bits, khz] = a.split("/").map(Number);
      Object.assign(t, { bits, sampleRate: Math.round(khz * 1000) });
    }
    Object.assign(t, more);
    const rate = t.bitrate || (t.codec === "flac" ? (t.bits === 24 ? KBPS.flac24 : KBPS.flac16) : KBPS[t.codec] || 900) * (t.channels / 2);
    t.size = Math.round((rate * 1000 / 8) * t.duration) + (t.cover ? 180000 : 0);
    const ext = { mp3: "mp3", flac: "flac", alac: "m4a", aac: "m4a", opus: "opus", vorbis: "ogg", wma: "wma", wav: "wav" }[codec];
    const file = more.filename || `${String(no || 1).padStart(2, "0")} ${slug(title)}`;
    t.filename = file;
    t.path = more.path || `~/Music/${slug(artist || "Unknown")}/${slug(album || "Unknown")}/${file}.${ext}`;
    return t;
  }

  const tracks = [
    // Road Trip
    track("t01", "Fleetwood Mac", "Dreams", "Rumours", 1977, 2, "flac 16/44.1", "4:17", -13.9, -0.9),
    track("t02", "Tom Petty", "Runnin' Down a Dream", "Full Moon Fever", 1989, 6, "mp3 320", "4:23", -10.8, -0.2),
    track("t03", "The War on Drugs", "Red Eyes", "Lost in the Dream", 2014, 2, "mp3 vbr 251/320", "4:59", -9.6, 0.3),
    track("t04", "Kraftwerk", "Autobahn (Single Edit)", "Autobahn", 1974, 1, "flac 24/96", "3:28", -17.2, -2.1),
    track("t05", "Sigur Rós", "Hoppípolla", "Takk...", 2005, 2, "opus 160", "4:28", -12.4, -0.4),
    track("t06", "LCD Soundsystem", "All My Friends", "Sound of Silver", 2007, 5, "aac 256", "7:37", -9.1, 0.1),
    track("t07", "Massive Attack", "Teardrop", "Mezzanine", 1998, 3, "mp3 320", "5:29", -11.2, -0.3),
    track("t08", "", "", "", null, null, "mp3 192", "3:52", -15.0, -1.2,
      { filename: "radio_rip_2004-07-11_track3", path: "~/Downloads/radio_rip_2004-07-11_track3.mp3", albumartist: "", cover: false }),
    track("t09", "Bruce Springsteen", "Born to Run", "Born to Run", 1975, 5, "mp3 256", "4:31", -11.9, -0.1),
    track("t10", "Khruangbin", "María También", "Con Todo El Mundo", 2018, 3, "flac 16/44.1", "3:28", -12.8, -0.6),
    track("t11", "Daft Punk", "Contact", "Random Access Memories", 2013, 13, "wav", "6:21", -10.4, -0.2),
    track("t12", "Talking Heads", "Road to Nowhere", "Little Creatures", 1985, 9, "wma 192", "4:20", -12.1, -0.5),
    track("t13", "Röyksopp", "Eple", "Melody A.M.", 2001, 2, "mp3 320", "3:30", -12.6, -0.4),
    track("t14", "Steely Dan", "Kid Charlemagne", "The Royal Scam", 1976, 1, "alac", "4:38", -15.3, -1.4),
    track("t15", "Boards of Canada", "Roygbiv", "Music Has the Right to Children", 1998, 9, "mp3 320", "2:31", -13.0, -0.8, { exists: false }),
    track("t16", "The Cure", "Just Like Heaven", "Kiss Me Kiss Me Kiss Me", 1987, 7, "mp3 320", "3:32", -12.9, -0.7),
    track("t17", "Stereolab", "French Disko", "Refried Ectoplasm", 1995, 9, "vorbis 192", "3:44", -11.5, -0.3),
    track("t18", "Pink Floyd", "Shine On You Crazy Diamond (Parts I–V)", "Wish You Were Here", 1975, 1, "flac 24/96", "13:31", -19.5, -3.2, { channels: 6 }),
    track("t19", "Phoenix", "Lisztomania", "Wolfgang Amadeus Phoenix", 2009, 1, "mp3 vbr 221/256", "4:01", -8.9, 0.4),
    track("t20", "M83", "Midnight City", "Hurry Up, We're Dreaming", 2011, 2, "mp3 320", "4:03", -8.4, 0.6),
    track("t21", "Arcade Fire", "No Cars Go", "Neon Bible", 2007, 10, "aac 256", "5:43", -10.0, -0.1),
    // Kids' car
    track("t30", "The Beatles", "Here Comes the Sun", "Abbey Road", 1969, 7, "mp3 320", "3:05", -13.2, -0.9),
    track("t31", "ABBA", "Dancing Queen", "Arrival", 1976, 2, "mp3 256", "3:51", -11.6, -0.3),
    track("t32", "Electric Light Orchestra", "Mr. Blue Sky", "Out of the Blue", 1977, 13, "flac 16/44.1", "5:03", -12.0, -0.4),
    track("t33", "Queen", "Don't Stop Me Now", "Jazz", 1978, 12, "mp3 320", "3:29", -12.3, -0.5),
    track("t34", "Katrina and the Waves", "Walking on Sunshine", "Walking on Sunshine", 1985, 1, "mp3 256", "3:58", -11.1, -0.2),
    track("t35", "The Monkees", "I'm a Believer", "More of the Monkees", 1967, 9, "mp3 192", "2:47", -12.7, -0.8),
    track("t36", "Pharrell Williams", "Happy", "G I R L", 2014, 5, "aac 256", "3:53", -8.6, 0.2),
    track("t37", "Bob Marley & The Wailers", "Three Little Birds", "Exodus", 1977, 9, "mp3 320", "3:00", -13.5, -1.0),
    track("t38", "Bill Withers", "Lovely Day", "Menagerie", 1977, 1, "flac 16/44.1", "4:15", -14.4, -1.1),
    track("t39", "Earth, Wind & Fire", "September", "The Best of Earth, Wind & Fire, Vol. 1", 1978, 2, "mp3 320", "3:35", -10.9, -0.1),
    track("t40", "The Beatles", "Yellow Submarine", "Revolver", 1966, 6, "mp3 320", "2:38", -13.8, -1.2),
    track("t41", "Bruno Mars", "Count on Me", "Doo-Wops & Hooligans", 2010, 7, "mp3 vbr 236/320", "3:17", -9.4, 0.0),
    // Gym
    track("t50", "Eminem", "Lose Yourself", "8 Mile", 2002, 1, "mp3 320", "5:26", -8.1, 0.4),
    track("t51", "Macklemore & Ryan Lewis", "Can't Hold Us", "The Heist", 2012, 2, "mp3 320", "4:18", -7.6, 0.7),
    track("t52", "Kanye West", "Stronger", "Graduation", 2007, 3, "aac 256", "5:11", -8.4, 0.3),
    track("t53", "Daft Punk", "Harder, Better, Faster, Stronger", "Discovery", 2001, 4, "flac 16/44.1", "3:44", -9.0, 0.1),
    track("t54", "Fort Minor", "Remember the Name", "The Rising Tied", 2005, 15, "mp3 256", "3:50", -8.3, 0.2),
    track("t55", "Muse", "Uprising", "The Resistance", 2009, 1, "mp3 vbr 248/320", "5:03", -8.0, 0.5),
    track("t56", "The White Stripes", "Seven Nation Army", "Elephant", 2003, 1, "mp3 320", "3:52", -10.2, -0.3),
    track("t57", "Beastie Boys", "Sabotage", "Ill Communication", 1994, 9, "mp3 256", "2:59", -9.8, -0.1),
    track("t58", "Survivor", "Eye of the Tiger", "Eye of the Tiger", 1982, 1, "mp3 320", "4:04", -11.4, -0.6),
    track("t59", "David Guetta", "Titanium (feat. Sia)", "Nothing but the Beat", 2011, 8, "mp3 320", "4:05", -7.9, 0.8),
    track("t60", "Run the Jewels", "Legend Has It", "Run the Jewels 3", 2016, 4, "opus 160", "3:25", -8.7, 0.2),
    track("t61", "Rage Against the Machine", "Guerrilla Radio", "The Battle of Los Angeles", 1999, 3, "mp3 320", "3:26", -8.8, 0.3),
    // Sleep
    track("t70", "Brian Eno", "An Ending (Ascent)", "Apollo: Atmospheres and Soundtracks", 1983, 11, "flac 16/44.1", "4:26", -22.4, -6.0),
    track("t71", "Max Richter", "On the Nature of Daylight", "The Blue Notebooks", 2004, 2, "flac 24/96", "6:12", -21.0, -4.1),
    track("t72", "Ólafur Arnalds", "Saman", "Living Room Songs", 2011, 4, "flac 16/44.1", "3:08", -20.3, -3.5),
    track("t73", "Hiroshi Yoshimura", "Blink", "Music for Nine Post Cards", 1982, 3, "mp3 320", "5:30", -19.8, -3.0),
    track("t74", "Nils Frahm", "Says", "Spaces", 2013, 2, "flac 24/96", "8:17", -17.9, -1.6),
    track("t75", "Aphex Twin", "#3", "Selected Ambient Works Volume II", 1994, 3, "wma 192", "7:47", -23.1, -6.4),
    track("t76", "Harold Budd & Brian Eno", "First Light", "Ambient 2: The Plateaux of Mirror", 1980, 1, "mp3 256", "7:01", -24.0, -7.2),
    track("t77", "Stars of the Lid", "Requiem for Dying Mothers, Part 2", "The Tired Sounds of Stars of the Lid", 2001, 4, "flac 16/44.1", "6:08", -22.7, -5.5),
  ];

  // In the music folder but in no playlist yet: something to find in the library.
  tracks.push(
    track("t90", "Fleetwood Mac", "Go Your Own Way", "Rumours", 1977, 5, "flac 16/44.1", "3:38", -12.1, -0.4),
    track("t91", "Fleetwood Mac", "The Chain", "Rumours", 1977, 7, "flac 16/44.1", "4:30", -13.0, -0.6),
    track("t92", "Fleetwood Mac", "Everywhere", "Tango in the Night", 1987, 4, "mp3 320", "3:48", -10.9, -0.2),
    track("t93", "The Cure", "Lovesong", "Disintegration", 1989, 8, "mp3 320", "3:29", -11.8, -0.4),
    track("t94", "Khruangbin", "Time (You and I)", "Mordechai", 2020, 9, "flac 16/44.1", "6:24", -12.2, -0.5),
    track("t95", "Daft Punk", "Get Lucky (feat. Pharrell Williams)", "Random Access Memories", 2013, 8, "flac 24/96", "6:09", -9.6, 0.1),
    track("t96", "ABBA", "Mamma Mia", "ABBA", 1975, 1, "mp3 256", "3:32", -11.0, -0.3),
    track("t97", "Brian Eno", "Deep Blue Day", "Apollo: Atmospheres and Soundtracks", 1983, 9, "flac 16/44.1", "3:59", -21.3, -5.2),
    track("t98", "Massive Attack", "Angel", "Mezzanine", 1998, 1, "mp3 320", "6:18", -11.6, -0.4),
    track("t99", "Stereolab", "Cybele's Reverie", "Emperor Tomato Ketchup", 1996, 2, "vorbis 192", "4:37", -11.2, -0.3),
    track("t9a", "", "", "", null, null, "mp3 256", "4:12", -12.4, -0.6, { filename: "track07", path: "~/Music/Unsorted/track07.mp3", albumartist: "", cover: false }),
    track("t9b", "Max Richter", "Vladimir's Blues", "The Blue Notebooks", 2004, 6, "flac 16/44.1", "1:18", -22.6, -6.1),
  );

  // Genres by artist, and the labels someone gave their music.
  const GENRES = {
    "Fleetwood Mac": "Rock", "Tom Petty": "Rock", "The War on Drugs": "Indie Rock", Kraftwerk: "Electronic", "Sigur Rós": "Post-Rock",
    "LCD Soundsystem": "Dance", "Massive Attack": "Trip Hop", "Bruce Springsteen": "Rock", Khruangbin: "Psychedelic", "Daft Punk": "Electronic",
    "Talking Heads": "New Wave", "Röyksopp": "Electronic", "Steely Dan": "Jazz Rock", "Boards of Canada": "Electronic", "The Cure": "New Wave",
    Stereolab: "Indie Pop", "Pink Floyd": "Progressive Rock", Phoenix: "Indie Pop", M83: "Synth-Pop", "Arcade Fire": "Indie Rock",
    "The Beatles": "Pop", ABBA: "Pop", "Electric Light Orchestra": "Rock", Queen: "Rock", "Katrina and the Waves": "Pop", "The Monkees": "Pop",
    "Pharrell Williams": "Pop", "Bob Marley & The Wailers": "Reggae", "Bill Withers": "Soul", "Earth, Wind & Fire": "Funk", "Bruno Mars": "Pop",
    Eminem: "Hip Hop", "Macklemore & Ryan Lewis": "Hip Hop", "Kanye West": "Hip Hop", "Fort Minor": "Hip Hop", Muse: "Rock",
    "The White Stripes": "Rock", "Beastie Boys": "Hip Hop", Survivor: "Rock", "David Guetta": "Dance", "Run the Jewels": "Hip Hop",
    "Rage Against the Machine": "Rock", "Brian Eno": "Ambient", "Max Richter": "Modern Classical", "Ólafur Arnalds": "Modern Classical",
    "Hiroshi Yoshimura": "Ambient", "Nils Frahm": "Modern Classical", "Aphex Twin": "Ambient", "Harold Budd & Brian Eno": "Ambient",
    "Stars of the Lid": "Ambient",
  };
  const LABELS = {
    "driving": ["t01", "t02", "t03", "t04", "t06", "t09", "t12", "t19", "t20", "t21", "t90", "t92"],
    "singalong": ["t01", "t09", "t16", "t30", "t31", "t32", "t33", "t34", "t35", "t40", "t90", "t96"],
    "kids": ["t30", "t31", "t32", "t34", "t35", "t36", "t37", "t40", "t41", "t96"],
    "chill": ["t05", "t07", "t10", "t13", "t17", "t38", "t94", "t98", "t99"],
    "workout": ["t50", "t51", "t52", "t53", "t54", "t55", "t56", "t57", "t58", "t59", "t60", "t61", "t95"],
    "sleep": ["t70", "t71", "t72", "t73", "t74", "t75", "t76", "t77", "t97", "t9b"],
    "favourite": ["t01", "t06", "t11", "t16", "t32", "t53", "t71", "t91"],
    "70s": ["t01", "t04", "t09", "t14", "t18", "t31", "t32", "t33", "t37", "t38", "t39", "t90", "t91", "t96"],
  };
  for (const t of tracks) {
    t.genre = t.genre || GENRES[t.artist] || "";
    t.composer = t.composer || "";
    t.labels = Object.keys(LABELS).filter((l) => LABELS[l].includes(t.id));
  }

  let n = 0;
  const entries = (ids) => ids.map((id) => ({ id: `e${++n}`, trackId: id }));
  const range = (a, b) => Array.from({ length: b - a + 1 }, (_, i) => `t${String(a + i).padStart(2, "0")}`);

  const playlists = [
    { id: "p-road", name: "Road Trip", entries: entries(range(1, 21)), norm: { on: true, target: -18, mode: "track" } },
    { id: "p-kids", name: "Kids' car", entries: entries(range(30, 41)), norm: { on: true, target: -18, mode: "track" } },
    { id: "p-gym", name: "Gym", entries: entries(range(50, 61)), norm: { on: false, target: -16, mode: "track" } },
    { id: "p-sleep", name: "Sleep", entries: entries(range(70, 77)), norm: { on: false, target: -20, mode: "album" } },
  ];

  const generic = {
    id: "prof-generic", name: "Generic", builtIn: true,
    bitrateCap: 320, mode: "cbr", sampleRates: [44100, 48000], tags: "id3v23", id3v1: false, cover: "small",
    layout: "folders", fileTemplate: "{n} - [{artist} - ]{title|filename}", folderTemplate: "{n} {playlist}",
    ascii: false, maxLen: 0, trackTagIsPosition: true, numberInTitle: false,
  };
  const profiles = [
    generic,
    { ...generic, id: "prof-car256", name: "Car stereo, 256 kbps", builtIn: false, bitrateCap: 256 },
    {
      ...generic, id: "prof-clip", name: "Clip player", builtIn: false, bitrateCap: 256, layout: "single",
      fileTemplate: "{n} - {title|filename}", ascii: true, maxLen: 32, cover: "none", tags: "id3v23", id3v1: true,
    },
  ];

  const DAY = 86400000;
  // What each stick held at its last sync: the entry ids, in order.
  const roadAtSync = playlists[0].entries.filter((e) => !["t15", "t19", "t20"].includes(e.trackId)).map((e) => e.trackId);
  [roadAtSync[3], roadAtSync[4]] = [roadAtSync[4], roadAtSync[3]];
  const stickSets = [
    {
      id: "set-car", name: "Car stick", profileId: "prof-generic", playlistIds: ["p-road", "p-kids"], stickId: "dev-sandisk", label: "CAR",
      lastSync: { at: Date.now() - 3 * DAY, stickId: "dev-sandisk", content: [roadAtSync, playlists[1].entries.map((e) => e.trackId)], recipe: "320|-18|track" },
    },
    {
      id: "set-gym", name: "Gym clip", profileId: "prof-clip", playlistIds: ["p-gym"], stickId: "dev-generic", label: "GYM",
      lastSync: null,
    },
  ];

  const sticks = [
    {
      id: "dev-sandisk", device: "/dev/sdb", name: "SanDisk Cruzer Blade", size_bytes: 15_631_122_432, bus: "usb",
      volumes: [{ label: "CAR", fs_type: "vfat", size_bytes: 15_630_073_856, summary: "2 folders, 31 files, 1.4 GB" }],
      // The note the Car stick's last sync left on it.
      note: {
        app: "Playlist2USB", format: 1, libraryId: "lib-demo", setId: "set-car", setName: "Car stick",
        profile: { id: "prof-generic", name: "Generic" },
        playlists: [{ id: "p-road", name: "Road Trip", tracks: 19 }, { id: "p-kids", name: "Kids' car", tracks: 12 }],
        files: 31, syncedAt: Date.now() - 3 * DAY,
      },
    },
    {
      id: "dev-generic", device: "/dev/sdc", name: "Generic Flash Disk", size_bytes: 7_864_320_000, bus: "usb",
      volumes: [{ label: "NO NAME", fs_type: "vfat", size_bytes: 7_863_271_424, summary: "DCIM, Documents and 214 other files, 3.2 GB" }],
    },
    {
      id: "dev-kingston", device: "/dev/sdd", name: "Kingston DataTraveler Exodia", size_bytes: 30_943_995_904, bus: "usb",
      volumes: [{ label: "BACKUP", fs_type: "exfat", size_bytes: 30_942_947_328, summary: "Backups, 12.4 GB" }],
    },
    {
      id: "dev-verbatim", device: "/dev/sde", name: "Verbatim Store n Go", size_bytes: 7_747_397_632, bus: "usb",
      volumes: [{ label: "DADS CAR", fs_type: "vfat", size_bytes: 7_746_349_056, summary: "1 folder, 40 files, 310 MB" }],
      // Set up by Playlist2USB on someone else's computer.
      note: {
        app: "Playlist2USB", format: 1, libraryId: "lib-elsewhere", setId: "set-dad", setName: "Dad's car",
        profile: { id: "prof-x", name: "Car stereo, 256 kbps" },
        playlists: [{ id: "p-x", name: "Road songs", tracks: 40 }],
        files: 40, syncedAt: Date.now() - 41 * DAY,
      },
    },
  ];

  /** The demo's music folder, chosen and scanned three hours ago. */
  const music = {
    root: "~/Music", scanning: false, watching: true, lastScan: Date.now() - 3 * 3600000, error: null,
    last: { files: tracks.filter((t) => t.path.startsWith("~/Music/") && t.exists).length, changed: [], added: 0, moved: 0, missing: 1, failed: 0 },
  };

  P2U.demo = { tracks, playlists, profiles, stickSets, sticks, music, libraryId: "lib-demo", nextEntry: () => `e${++n}` };
})(window.P2U = window.P2U || {});
