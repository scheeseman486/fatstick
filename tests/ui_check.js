// Clicks through the UI prototype in headless Chromium: selection, pointer
// drag sorting, undo, drops from the OS (real tagged files), the name
// template, the volume setting, profiles and a simulated sync.
//
//   npm install playwright && npx playwright install chromium   (once)
//   node tests/ui_check.js [screenshot dir]
//
// Serves app/ui itself; makes its two audio fixtures with ffmpeg.
const { chromium } = require('playwright');
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const { execFileSync } = require('child_process');

const UI = path.join(__dirname, '..', 'app', 'ui');
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'fatstick-ui-'));
const out = process.argv[2] || work;
fs.mkdirSync(out, { recursive: true });
const ff = (args) => execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', ...args]);
ff(['-f', 'lavfi', '-i', 'sine=frequency=440:duration=4', '-ac', '2', '-c:a', 'libmp3lame', '-b:a', '256k', '-metadata', 'title=Test Tone Á', '-metadata', 'artist=Lab Bench', '-metadata', 'album=Fixtures', '-metadata', 'track=2', '-id3v2_version', '3', path.join(work, 'tone.mp3')]);
ff(['-f', 'lavfi', '-i', 'sine=frequency=330:duration=5', '-ac', '2', '-c:a', 'flac', '-metadata', 'TITLE=Second Fixture', '-metadata', 'ARTIST=Lab Bench', '-metadata', 'TRACKNUMBER=1', path.join(work, 'fixture.flac')]);
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const f = path.join(UI, decodeURIComponent(req.url.split('?')[0]).replace(/^\/+/, '') || 'index.html');
  if (!f.startsWith(UI) || !fs.existsSync(f)) { res.writeHead(404); return res.end(); }
  res.writeHead(200, { 'Content-Type': TYPES[path.extname(f)] || 'application/octet-stream' });
  fs.createReadStream(f).pipe(res);
}).listen(0);
const URL = `http://localhost:${server.address().port}/index.html`;
let fails = 0;
const check = (name, ok, extra = '') => { console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${extra ? ' — ' + extra : ''}`); if (!ok) fails++; };

(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1280, height: 800 } });
  const errs = [];
  p.on('pageerror', e => errs.push(e.message));
  p.on('console', m => { if (m.type() === 'error') errs.push(m.text()); });
  await p.addInitScript(() => localStorage.setItem('fatstick-proto', JSON.stringify({ look: 'backlit', theme: 'light', shell: 0 })));
  await p.goto(URL);
  await p.waitForTimeout(600);
  const titles = () => p.$$eval('#list .row .c-title .t', els => els.map(e => e.textContent));
  const row = (i) => p.locator('#list .row').nth(i);
  const center = async (loc) => { const bb = await loc.boundingBox(); return [bb.x + bb.width / 2, bb.y + bb.height / 2]; };
  const dragTo = async (from, x, y) => {
    const [fx, fy] = await center(from);
    await p.mouse.move(fx, fy);
    await p.mouse.down();
    for (let k = 1; k <= 12; k++) await p.mouse.move(fx + (x - fx) * k / 12, fy + (y - fy) * k / 12);
    await p.waitForTimeout(60);
  };

  const t0 = await titles();
  // 1. Shift-click range, drag the block down.
  await row(2).click();
  await row(4).click({ modifiers: ['Shift'] });
  check('shift-click selects a range', (await p.locator('#list .row.sel').count()) === 3);
  const [, y9] = await center(row(9));
  const bb9 = await row(9).boundingBox();
  await dragTo(row(3), bb9.x + 100, bb9.y + bb9.height * 0.8);
  check('drop line shows while dragging', await p.locator('#drop-line').isVisible());
  check('badge says 3 tracks', (await p.locator('#drag-badge').textContent()) === '3 tracks');
  await p.screenshot({ path: `${out}/drag.png` });
  await p.mouse.up();
  const t1 = await titles();
  const expect1 = [...t0.slice(0, 2), ...t0.slice(5, 10), ...t0.slice(2, 5), ...t0.slice(10)];
  check('block lands after row 10', JSON.stringify(t1) === JSON.stringify(expect1), t1.slice(0, 12).join(' | '));

  // 2. Scattered ctrl-click selection dragged to the top lands as one block in order.
  await row(1).click();
  await row(6).click({ modifiers: ['Control'] });
  await row(11).click({ modifiers: ['Control'] });
  const picked = [t1[1], t1[6], t1[11]];
  const top = await row(0).boundingBox();
  await dragTo(row(6), top.x + 100, top.y + 3);
  await p.mouse.up();
  const t2 = await titles();
  check('scattered selection lands as a block', JSON.stringify(t2.slice(0, 3)) === JSON.stringify(picked), t2.slice(0, 4).join(' | '));

  // 3. Undo restores, redo reapplies.
  await p.keyboard.press('Control+z');
  check('undo restores the order', JSON.stringify(await titles()) === JSON.stringify(t1));
  await p.keyboard.press('Control+Shift+z');
  check('redo reapplies', JSON.stringify(await titles()) === JSON.stringify(t2));

  // 4. Delete removes from playlist with an undo toast.
  await row(0).click();
  await p.keyboard.press('Delete');
  check('delete removes one entry', (await titles()).length === t2.length - 1);
  check('toast says files are untouched', /untouched/.test(await p.locator('#toast').textContent()));
  await p.locator('#toast button').click();
  check('toast undo restores', (await titles()).length === t2.length);

  // 5. Alt+Down moves the selection one place.
  await row(0).click();
  await p.keyboard.press('Alt+ArrowDown');
  const t5 = await titles();
  check('Alt+Down moves the track down', t5[1] === t2[0] && t5[0] === t2[1]);

  // 6. Drag a selection onto the Gym playlist in the sidebar.
  const gymBefore = await p.locator('[data-pl="p-gym"] .mt').textContent();
  await row(3).click();
  await row(4).click({ modifiers: ['Shift'] });
  const [gx, gy] = await center(p.locator('[data-pl="p-gym"]'));
  await dragTo(row(3), gx, gy);
  check('badge offers to add to Gym', /to Gym/.test(await p.locator('#drag-badge').textContent()));
  await p.mouse.up();
  const gymAfter = await p.locator('[data-pl="p-gym"] .mt').textContent();
  check('Gym gains 2 tracks', gymBefore.startsWith('12') && gymAfter.startsWith('14'), `${gymBefore} -> ${gymAfter}`);

  // 7. File names, in the player profile: typing updates the list of names
  // and keeps focus; the stick panel shows an example.
  check('the stick panel shows an example name', /Files named like 01 /.test(await p.locator('.prof-names').textContent()));
  await p.getByRole('button', { name: 'Edit…' }).click();
  await p.locator('#tpl').fill('{n} {title|filename}');
  await p.locator('#tpl').type(' x');
  const focused = await p.evaluate(() => document.activeElement.id);
  check('template keeps focus while typing', focused === 'tpl');
  const pv = await p.locator('.preview li.file').first().textContent();
  check('preview follows the template', /^01 .* x\.mp3/.test(pv), pv);
  await p.locator('#tpl').fill('{n} - {foo}');
  check('bad field is reported', /isn't a field/.test(await p.locator('#tpl-help').textContent()));
  check('sync is blocked by a bad template', await p.locator('#sync-btn').isDisabled());
  await p.locator('.tpl-row select').selectOption({ label: '01 - Artist - Title' });
  check('preset restores a good template', !(await p.locator('#sync-btn').isDisabled()));
  await p.screenshot({ path: `${out}/profile-names.png` });
  await p.keyboard.press('Escape');
  check('the stick panel follows', /Files named like 01 - /.test(await p.locator('.prof-names').textContent()));

  // 8. Volume popover: switching off hides the Gain column.
  check('gain column shown', (await p.locator('#list-cols .h-gain').count()) === 1);
  await p.getByRole('button', { name: /Even volume/ }).click();
  await p.screenshot({ path: `${out}/volume.png` });
  await p.locator('#norm-on').uncheck();
  await p.keyboard.press('Escape');
  check('gain column hidden when off', (await p.locator('#list-cols .h-gain').count()) === 0);
  await p.keyboard.press('Control+z');

  // 9. Profile at 256 kbps turns 320 copies into encodes.
  const chips320 = await p.locator('#list .chip', { hasText: 'Copy' }).count();
  await p.locator('#profile-select').selectOption({ label: 'Car stereo, 256 kbps' });
  const enc256 = await p.locator('#list .chip', { hasText: 'MP3 256' }).count();
  check('256 profile encodes more tracks', enc256 > 0 && (await p.locator('#list .chip', { hasText: 'Copy' }).count()) < chips320);
  await p.locator('#profile-select').selectOption({ label: 'Generic' });

  // 10. Context menu.
  await row(2).click({ button: 'right' });
  check('context menu opens', await p.locator('#menu').isVisible());
  await p.screenshot({ path: `${out}/context.png` });
  await p.keyboard.press('Escape');

  // 11. Profile editor (in tabs) and all-names dialogs.
  await p.getByRole('button', { name: 'Edit…' }).click();
  await p.waitForTimeout(100);
  const tabNames = await p.getByRole('tab').allTextContents();
  check('the profile editor has three tabs', tabNames.join('|') === 'Files and folders|Audio|Tags and cover art', tabNames.join('|'));
  check('it opens on the tab last used', (await p.getByRole('tab', { selected: true }).textContent()) === 'Files and folders' && await p.locator('#tpl').isVisible());
  await p.screenshot({ path: `${out}/profile.png` });
  await p.getByRole('tab', { selected: true }).focus();
  await p.keyboard.press('ArrowRight');
  check('arrow keys move between tabs', (await p.getByRole('tab', { selected: true }).textContent()) === 'Audio'
    && await p.getByText('256 kbps, for players that stumble on 320').isVisible() && !(await p.locator('#tpl').isVisible()));
  await p.screenshot({ path: `${out}/profile-audio.png` });
  await p.getByRole('tab', { name: 'Tags and cover art' }).click();
  check('clicking a tab shows it', await p.getByText('ID3v2.3 (older players read it best)').isVisible());
  await p.screenshot({ path: `${out}/profile-tags.png` });
  await p.keyboard.press('Escape');
  await p.getByRole('button', { name: 'Edit…' }).click();
  check('it opens on the tab last used', (await p.getByRole('tab', { selected: true }).textContent()) === 'Tags and cover art');
  await p.getByRole('tab', { name: 'Files and folders' }).click();
  await p.keyboard.press('Escape');
  // "List all" shows the stick in the middle of the window.
  await p.getByRole('button', { name: 'List all' }).click();
  check('"List all" shows the stick in the middle of the window', await p.locator('#stickview').isVisible() && (await p.locator('#stickview .lay-folder').count()) === 2);
  await p.locator('[data-pl="p-road"]').click();
  check('a playlist on the left goes back to it', await p.locator('#main').isVisible() && await p.locator('#stickview').isHidden());

  // 12. Real files dropped onto the list.
  const files = ['tone.mp3', 'fixture.flac'].map(n => ({ n, b64: fs.readFileSync(path.join(work, n)).toString('base64') }));
  const before = (await titles()).length;
  const box = await row(1).boundingBox();
  await p.evaluate(async ({ files, x, y }) => {
    const dt = new DataTransfer();
    for (const f of files) {
      const bin = Uint8Array.from(atob(f.b64), c => c.charCodeAt(0));
      dt.items.add(new File([bin], f.n, { type: f.n.endsWith('mp3') ? 'audio/mpeg' : 'audio/flac' }));
    }
    const el = document.elementFromPoint(x, y);
    el.dispatchEvent(new DragEvent('dragover', { dataTransfer: dt, clientX: x, clientY: y, bubbles: true, cancelable: true }));
    el.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, clientX: x, clientY: y, bubbles: true, cancelable: true }));
  }, { files, x: box.x + 200, y: box.y + box.height * 0.8 });
  await p.waitForTimeout(1500);
  const t12 = await titles();
  check('two dropped files inserted after row 2', t12.length === before + 2 && t12[2] === 'Second Fixture' && t12[3] === 'Test Tone Á', t12.slice(0, 5).join(' | '));
  const fmt = await p.locator('#list .row').nth(3).locator('.c-format').textContent();
  check('MP3 bitrate read from the frames', fmt === 'MP3 256', fmt);
  await row(3).dblclick();
  await p.waitForTimeout(800);
  const playing = await p.locator('#disp-text').textContent();
  check('double-click plays, display shows PLAY', /^PLAY 04-/.test(playing), playing);
  await p.screenshot({ path: `${out}/playing.png` });
  await p.keyboard.press('Space');

  // 13. Sticks that remember what they hold: the Car stick's own stick is
  // marked in the sidebar and in the stick list; another computer's stick is
  // named; syncing the Gym clip onto the Car stick's stick needs a tick first.
  check('the plugged-in stick is marked in the sidebar', /plugged in/.test(await p.locator('[data-set="set-car"] .flag').textContent()));
  const holds = await p.locator('#stick .dev .dholds').allTextContents();
  check('the stick list says what each stick holds',
    holds.some(t => /^Holds this stick's playlists, synced 3 days ago/.test(t)) && holds.some(t => /^Holds Dad's car \(Road songs\), synced .* ago, set up on another computer\./.test(t)), holds.join(' / '));
  await p.locator('[data-set="set-gym"]').click();
  await p.locator('#stick .dev input[value="dev-sandisk"]').check();
  check('picking another stick (set)\'s stick warns', /This stick holds Car stick \(Road Trip, Kids' car\).*Syncing Gym clip to it replaces that/.test(await p.locator('#holds-other').textContent()));
  await p.locator('#sync-btn').click();
  check('the confirmation asks before replacing it', /Car stick will need a stick of its own/.test(await p.locator('#replace-warning').textContent()) && await p.locator('#modal .btn.primary').isDisabled());
  await p.locator('#replace-ok').check();
  check('ticking the box allows it', !(await p.locator('#modal .btn.primary').isDisabled()));
  await p.screenshot({ path: `${out}/replace.png` });
  await p.keyboard.press('Escape');
  await p.keyboard.press('Control+z');
  await p.locator('[data-set="set-car"]').click();
  await p.locator('[data-pl="p-road"]').click();

  // 14. A track's start and end: cut 2 s from the start of row 2 and add
  // 1.5 s of silence at its end. It's encoded rather than copied.
  await p.locator('#list .row').nth(1).click();
  await p.keyboard.press('e');
  check('E opens the start and end, with a panel for each end', /^Start and end of /.test(await p.locator('#modal-title').textContent()) && (await p.locator('.edge-panel').count()) === 2);
  check('the player bar stays usable while it is open', await p.evaluate(() => document.body.classList.contains('modal-player')));
  // Dragging on the waveform sets the cut there.
  const waveBox = await p.locator('.edge-panel.start .wave-svg').boundingBox();
  await p.mouse.move(waveBox.x + waveBox.width * 0.2, waveBox.y + waveBox.height / 2);
  await p.mouse.down();
  await p.mouse.move(waveBox.x + waveBox.width * 0.1, waveBox.y + waveBox.height / 2, { steps: 4 });
  await p.mouse.up();
  const dragged = Number(await p.locator('#edge-cutStart').inputValue());
  check('dragging on the waveform moves the cut', dragged > 2 && dragged < 4.5, String(dragged));
  check('the waveform is drawn, faint where it is cut', ((await p.locator('.edge-panel.start .wave').getAttribute('d')) || '').length > 100 && ((await p.locator('.edge-panel.start .wave-on').getAttribute('d')) || '').length > 100);
  await p.locator('#edge-cutStart').fill('2');
  await p.locator('#edge-padEnd').fill('1.5');
  check('the silence shows beside the end', /1.5 s/.test(await p.locator('.edge-panel.end .edge-gap').textContent()) && /1.5 s of silence after/.test(await p.locator('.edge-panel.end .edge-sum').textContent()));
  await p.getByRole('button', { name: 'Hear the end' }).click();
  await p.waitForTimeout(100);
  const heardEnd = await p.evaluate(() => ({ pos: Fatstick.app.S.player.pos, dur: Fatstick.app.S.player.dur, row: Fatstick.app.S.player.entryId === Fatstick.app.pl().entries[1].id }));
  check('"Hear the end" plays the last seconds of it', heardEnd.row && Math.abs(heardEnd.pos - (heardEnd.dur - 5)) < 0.6, JSON.stringify(heardEnd));
  await p.screenshot({ path: `${out}/edges.png` });
  await p.evaluate(() => Fatstick.app.stopPlayback());
  await p.getByRole('button', { name: 'Apply' }).click();
  const edgeCell = await p.locator('#list .row').nth(1).locator('.c-edges').textContent();
  check('the start / end column shows the cut and the silence', edgeCell === '−2 / +1.5 s', edgeCell);
  const r2 = await p.evaluate(() => { const A = window.Fatstick.app; const it = A.planSet(A.stickSet()).folders[0].files[1]; return [it.recipe.kind, it.recipe.parts[0].cutStart, it.recipe.parts[0].padEnd, it.d.action]; });
  check('a cut track is rendered (encoded) on the stick', JSON.stringify(r2) === JSON.stringify(['render', 2, 1.5, 'encode']), JSON.stringify(r2));

  // 15. Silence between playlists, 10 s.
  await p.locator('#stick select[aria-label="Add a playlist to this stick"]').selectOption('__gap');
  await p.locator('#stick input[aria-label="Seconds of silence"]').fill('10');
  await p.locator('#stick input[aria-label="Seconds of silence"]').press('Enter');
  await p.locator('#stick input[aria-label="Seconds of silence"]').blur();
  const gapPlan = await p.evaluate(() => { const A = window.Fatstick.app; const f = A.planSet(A.stickSet()).folders.at(-1); return [f.folder, f.files.length, f.files[0].name, f.files[0].secs]; });
  check('silence gets a folder of its own, with one file', JSON.stringify(gapPlan) === JSON.stringify(["03 Silence", 1, "01 - Silence.mp3", 10]), JSON.stringify(gapPlan));

  // 16. Kids' car joined into one track: still a playlist here, one file on the stick.
  await p.locator('[data-pl="p-kids"]').click();
  await p.locator('#join-btn').click();
  await p.locator('#join-on').check();
  await p.keyboard.press('Escape');
  const chips = await p.locator('#list .c-stick .chip').allTextContents();
  check('every track shows where it starts in the joined track', chips.length === 12 && chips[0] === 'Joined, 0:00' && chips.every((c) => c.startsWith('Joined, ')), chips.slice(0, 3).join(' | '));
  check('the sidebar says the playlist goes on as one track', /12 tracks as one/.test(await p.locator('[data-pl="p-kids"] .mt').textContent()));
  const joinPlan = await p.evaluate(() => { const A = window.Fatstick.app; const f = A.planSet(A.stickSet()).folders[1]; return [f.files.length, f.present.length, f.files[0].name, f.files[0].recipe ? f.files[0].recipe.parts.length : -1]; });
  check('the stick gets one file for it, made of its 12 tracks', JSON.stringify(joinPlan) === JSON.stringify([1, 12, "01 - Kids' car.mp3", 12]), JSON.stringify(joinPlan));
  check("the stick panel counts it as joined", /12 tracks joined into one/.test(await p.locator('#stick .folders').textContent()));

  // 17. The stick's view, from the sidebar: every folder in stick order,
  // collapsible, for organizing the stick's playlists.
  const carIds = () => p.evaluate(() => Fatstick.app.stickSet('set-car').playlistIds.slice());
  const folders = () => p.locator('#stickview .lay-folder');
  await p.locator('[data-set="set-car"]').click();
  check('clicking a stick shows it in the middle of the window', await p.locator('#stickview').isVisible() && await p.locator('#main').isHidden() && (await p.locator('[data-set="set-car"]').getAttribute('aria-current')) === 'page');
  const heads = await p.locator('#stickview .lay-folder > summary, #stickview .lay-sumline').allTextContents();
  check('it lists the folders in stick order', heads.length === 3 && /^01 Road Trip\//.test(heads[0]) && /12 tracks joined into one file/.test(heads[1]) && /^03 Silence\/s of silence/.test(heads[2]), heads.join(' | '));
  check('a joined track lists its tracks with their start times', (await p.locator('#stickview .lay-folder.k-joined .lay-parts li').count()) === 12);
  check('nothing in a playlist is edited here', (await p.locator('#stickview input[type="checkbox"]').count()) === 0);
  check('the stick panel leaves the list to the view', await p.locator('#stick [aria-label="Playlists on this stick"]').isHidden());
  check('the display shows the stick', /CAR STICK/.test((await p.locator('#disp-text').textContent()).toUpperCase()) && /DIR/.test(await p.locator('#disp-unit').textContent()));
  await p.screenshot({ path: `${out}/stick-view.png` });
  await folders().first().locator('summary .lay-fname').click();
  check('folders collapse', !(await folders().first().evaluate((d) => d.open)));
  // Alt+Down on a handle moves a playlist.
  const ids0 = await carIds();
  await folders().first().locator('.sv-grip').focus();
  await p.keyboard.press('Alt+ArrowDown');
  check('Alt+Down on the handle moves it down', (await carIds())[1] === ids0[0], JSON.stringify(await carIds()));
  await p.keyboard.press('Control+z');
  check('undo puts it back', JSON.stringify(await carIds()) === JSON.stringify(ids0));
  // Silence between the first two.
  await p.locator('.sv-insert').first().hover();
  await p.locator('.sv-insert').first().locator('.sv-ins-btn').click();
  const withGap = await carIds();
  check('"Silence here" puts silence between them', withGap.length === 4 && /^gap-/.test(withGap[1]), JSON.stringify(withGap));
  await p.waitForFunction(() => document.activeElement && document.activeElement.matches('#stickview input.secs'), null, { timeout: 2000 }).catch(() => {});
  await p.keyboard.type('5');
  await p.keyboard.press('Enter');
  check('and its seconds are ready to change', await p.evaluate((id) => Fatstick.app.stickSet('set-car').gaps.find((g) => g.id === id).seconds, withGap[1]) === 5);
  await p.keyboard.press('Control+z');
  await p.keyboard.press('Control+z');
  check('undo takes it off again', JSON.stringify(await carIds()) === JSON.stringify(ids0));
  await p.getByRole('button', { name: 'Collapse all' }).click();
  // Drag a playlist in from the left, above the first folder.
  const dragPl = async (from, to, frac = 0.25) => {
    const a = await from.boundingBox();
    const z = await to.boundingBox();
    await p.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
    await p.mouse.down();
    await p.mouse.move(a.x + a.width / 2 + 20, a.y + a.height / 2 + 10, { steps: 3 });
    await p.mouse.move(z.x + z.width / 2, z.y + Math.min(z.height * frac, 40), { steps: 8 });
    const badge = await p.locator('#drag-badge').textContent();
    await p.mouse.up();
    return badge;
  };
  const badge = await dragPl(p.locator('[data-pl="p-gym"]'), folders().first(), 0.2);
  check('dragging a playlist from the left puts it where it\'s dropped', (await carIds())[0] === 'p-gym' && /Put Gym here/.test(badge), `${badge}: ${JSON.stringify(await carIds())}`);
  check('the drop doesn\'t open the playlist', await p.locator('#stickview').isVisible());
  // ... and the handles move them by dragging too.
  await p.getByRole('button', { name: 'Collapse all' }).click();
  await dragPl(folders().first().locator('.sv-grip'), folders().last(), 0.9);
  check('dragging a handle moves it', (await carIds()).at(-1) === 'p-gym', JSON.stringify(await carIds()));
  await p.keyboard.press('Control+z');
  await p.keyboard.press('Control+z');
  check('undo puts the stick back as it was', JSON.stringify(await carIds()) === JSON.stringify(ids0), JSON.stringify(await carIds()));
  // Onto a stick in the sidebar: it goes on the end of that stick.
  await dragPl(p.locator('[data-pl="p-sleep"]'), p.locator('[data-set="set-gym"]'), 0.5);
  check('a playlist dropped on a stick on the left goes on it', await p.evaluate(() => Fatstick.app.stickSet('set-gym').playlistIds.includes('p-sleep')));
  await p.keyboard.press('Control+z');
  // Taken off, with undo.
  await folders().nth(1).hover();
  await folders().nth(1).getByRole('button', { name: /Take .* off this stick/ }).click();
  check('a playlist can be taken off', (await carIds()).length === 2 && !(await carIds()).includes('p-kids'));
  await p.keyboard.press('Control+z');
  check('and put back with undo', (await carIds()).includes('p-kids'));
  // A change to a playlist shows here too.
  await p.locator('[data-pl="p-kids"]').click();
  await p.locator('#join-btn').click();
  await p.locator('#join-on').uncheck();
  await p.keyboard.press('Escape');
  await p.locator('[data-set="set-car"]').click();
  check('the stick view follows edits made in the playlist', /12 files/.test(await folders().nth(1).locator('summary').textContent()));
  await p.locator('[data-pl="p-road"]').click();

  // 18. Sync flow.
  await p.locator('#sync-btn').click();
  check('confirmation dialog', /Erase .* and sync/.test(await p.locator('#modal-title').textContent()));
  await p.screenshot({ path: `${out}/confirm.png` });
  await p.locator('#modal .btn.primary').click();
  await p.waitForTimeout(2500);
  await p.screenshot({ path: `${out}/syncing.png` });
  await p.waitForSelector('.sync-done', { timeout: 30000 });
  const done = await p.locator('.sync-done').textContent();
  check('sync finishes', /^Synced\./.test(done), done);
  await p.getByRole('button', { name: 'Done' }).click();
  check('stick is up to date afterwards', /Up to date/.test(await p.locator('.st-status').textContent()));
  await p.screenshot({ path: `${out}/synced.png` });

  {
    // 19. A playlist row shows its tags when clicked, and hides them again.
    await p.locator('[data-pl="p-road"]').click();
    await row(0).click();
    check('clicking a track shows its tags under it', (await p.locator('#list .row.open .row-tags .tag.lbl').count()) >= 1 && /Genre/.test(await p.locator('#list .row.open .row-tags').textContent()));
    check('the row grows to fit them', (await p.locator('#list .row.open').boundingBox()).height > 50);
    await row(0).click();
    check('clicking again hides them', (await p.locator('#list .row.open').count()) === 0);
    await row(1).click({ modifiers: ['Shift'] });
    check('shift-click selects without opening', (await p.locator('#list .row.open').count()) === 0);
    await p.screenshot({ path: `${out}/row-tags.png` });

    // 20. The search panel: exact matches, a divider, close ones; drag into the list.
    await p.locator('#srch-btn').click();
    await p.waitForTimeout(350);
    check('the search button widens the sidebar into the search panel', await p.locator('#app').evaluate((a) => a.classList.contains('searching')) && (await p.locator('#side').boundingBox()).width > 280);
    await p.keyboard.type('dream');
    await p.waitForTimeout(250);
    const srTitles = async (sel) => p.locator(sel).allTextContents();
    const exact = await srTitles('.srch-res .sr:not(.close) .sr-t');
    const close = await srTitles('.srch-res .sr.close .sr-t');
    check('whole-word matches come first', exact.includes('Red Eyes') && exact.includes("Runnin' Down a Dream"), exact.join(' | '));
    check('then a divider and the close matches', (await p.locator('.srch-res .sr-div').count()) === 1 && close.includes('Dreams'), close.join(' | '));
    await p.fill('#srch-in', 'chill');
    await p.waitForTimeout(250);
    check('labels are searched, and shown as the reason', (await p.locator('.srch-res .sr').count()) === 9 && (await p.locator('.srch-res .sr .sr-hints .tag.lbl', { hasText: 'chill' }).count()) === 9);
    await p.screenshot({ path: `${out}/search.png` });
    await p.fill('#srch-in', 'go your own way');
    await p.waitForTimeout(250);
    const before19 = (await titles()).length;
    const [rx, ry] = await center(row(2));
    await dragTo(p.locator('.srch-res .sr').first(), rx, ry - 6);
    check('dragging a result shows where it goes', !(await p.locator('#drop-line').isHidden()));
    await p.mouse.up();
    await p.waitForTimeout(150);
    const after19 = await titles();
    check('a search result dragged into the list lands there', after19.length === before19 + 1 && after19[2] === 'Go Your Own Way', after19.slice(0, 4).join(' | '));
    await p.locator('.srch-res .sr').first().dblclick();
    await p.waitForTimeout(150);
    check('double-clicking a result plays it, on its own', (await p.locator('#player .np-t').textContent()) === 'Go Your Own Way' && /From the library/.test(await p.locator('#player .np-s').textContent()) && (await titles()).length === before19 + 1);
    check('the song playing is marked in the results', (await p.locator('.srch-res .sr.playing .eq').count()) === 1);
    check('a song on its own has no next', await p.locator('#player button[aria-label="Next"]').isDisabled());
    await p.evaluate(() => Fatstick.api.seek(Fatstick.app.S.player.dur - 0.3));
    await p.waitForFunction(() => !Fatstick.app.S.player.trackId, null, { timeout: 3000 }).catch(() => {});
    check('it plays once: then the player stops', (await p.locator('#player .np-t').textContent()) === 'Nothing playing');
    await p.locator('.srch-res .sr').first().click();
    await p.keyboard.press('Shift+Enter');
    await p.waitForTimeout(150);
    check('Shift+Enter adds it at the end of the playlist', (await titles()).length === before19 + 2 && (await titles()).pop() === 'Go Your Own Way');
    await p.locator('#srch-in').focus();
    await p.keyboard.press('Escape');
    await p.keyboard.press('Escape');
    check('Esc clears, then closes the search', !(await p.locator('#app').evaluate((a) => a.classList.contains('searching'))));
    await p.keyboard.press('Control+f');
    check('Ctrl+F opens it again', await p.locator('#srch-in').evaluate((x) => document.activeElement === x));
    await p.locator('#srch-btn').click();

    // 21. The library view: browse by tag, select, drag onto a playlist.
    await p.locator('#lib-btn').click();
    check('the Library button switches the middle of the window', await p.locator('#app').evaluate((a) => a.classList.contains('lib-mode')) && await p.locator('#main').isHidden() && await p.locator('#libview').isVisible());
    check('the library lists the music folder', /64 tracks/.test(await p.locator('.lib-status').textContent()), await p.locator('.lib-status').textContent());
    await p.locator('.fct', { hasText: 'kids' }).first().click();
    check('a label narrows the table', (await p.locator('.lr').count()) === 10 && (await p.locator('.lib-chips .tag', { hasText: 'kids' }).count()) === 1);
    await p.locator('.fct', { hasText: 'Pop' }).first().click();
    check('tags add up', (await p.locator('.lr').count()) === 8);
    await p.locator('.lib-chips .tag', { hasText: 'Pop' }).locator('.x').click();
    await p.locator('.lr').nth(0).click();
    await p.locator('.lr').nth(1).click({ modifiers: ['Shift'] });
    const gymWas = parseInt(await p.locator('[data-pl="p-gym"] .mt').textContent(), 10);
    const gym = p.locator('[data-pl="p-gym"]');
    const [gx, gy] = await center(gym);
    await dragTo(p.locator('.lr').nth(0), gx, gy);
    check('dragging library tracks offers the playlist', /Add 2 tracks to Gym/.test(await p.locator('#drag-badge').textContent()));
    await p.mouse.up();
    await p.waitForTimeout(150);
    check('the playlist gets them', parseInt(await p.locator('[data-pl="p-gym"] .mt').textContent(), 10) === gymWas + 2, `${gymWas} -> ${await p.locator('[data-pl="p-gym"] .mt').textContent()}`);

    // 22. Tags saved into the files as they're entered: no preview, no prompts, and undo.
    const tagsOf = (ids) => p.evaluate((ids) => ids.map((id) => { const t = Fatstick.app.S.tracks.get(id); return `${t.genre}|${t.labels.join(',')}|${t.year || ''}`; }), ids);
    const selIds = await p.evaluate(() => Fatstick.app.lib.state.L.ids.filter((x) => Fatstick.app.lib.state.L.sel.has(x)));
    const tagsBefore = await tagsOf(selIds);
    const toastSays = (re) => p.waitForFunction((src) => new RegExp(src).test(document.getElementById('toast').textContent), re.source, { timeout: 5000 }).catch(() => {});
    const settled = (re) => p.waitForFunction((src) => new RegExp(src).test(document.querySelector('.lib-edit .ted-status').textContent), re.source, { timeout: 5000 }).catch(() => {});
    check('the editor says edits save as they are made', /saved into the files as you make them/.test(await p.locator('.lib-edit .ted-status').textContent()));
    await p.locator('.lib-edit input[id^="ted-genre"]').fill('Kids');
    await p.keyboard.press('Enter');
    await settled(/Saved: Genre/);
    check('Enter in a field writes it into the files, without asking', (await tagsOf(selIds)).every((x) => /^Kids\|/.test(x)) && await p.locator('#modal-back').isHidden(), (await tagsOf(selIds)).join(' / '));
    check('and says what was saved', /Saved: Genre “Kids”, in 2 files\./.test(await p.locator('.lib-edit .ted-status').textContent()));
    await p.locator('.lib-edit input[id^="ted-lbl"]').fill('car');
    await p.keyboard.press('Enter');
    await settled(/label “car” added/);
    check('a label is written as soon as it is added', (await tagsOf(selIds)).every((x) => /^Kids\|.*car/.test(x)), (await tagsOf(selIds)).join(' / '));
    check('the table shows the new label', (await p.locator('.lr.sel .c-labels').evaluateAll((cs) => cs.filter((c) => c.title.split(', ').includes('car')).length)) === 2);
    await p.screenshot({ path: `${out}/tags-saved.png` });
    await p.locator('.lib-edit .ted-status .btn', { hasText: 'Undo' }).click();
    await p.waitForFunction(() => Fatstick.app.lib.state.L.ids.filter((x) => Fatstick.app.lib.state.L.sel.has(x)).every((id) => !Fatstick.app.S.tracks.get(id).labels.includes('car')), null, { timeout: 5000 }).catch(() => {});
    check('Undo beside it takes the label off again', (await tagsOf(selIds)).every((x) => /^Kids\|/.test(x) && !/car/.test(x)), (await tagsOf(selIds)).join(' / '));
    await p.locator('.lib-scroll').focus();
    await p.keyboard.press('Control+z');
    await p.waitForFunction((want) => JSON.stringify(Fatstick.app.lib.state.L.ids.filter((x) => Fatstick.app.lib.state.L.sel.has(x)).map((id) => { const t = Fatstick.app.S.tracks.get(id); return `${t.genre}|${t.labels.join(',')}|${t.year || ''}`; })) === want, JSON.stringify(tagsBefore), { timeout: 5000 }).catch(() => {});
    check('Ctrl+Z puts the genre back', JSON.stringify(await tagsOf(selIds)) === JSON.stringify(tagsBefore), (await tagsOf(selIds)).join(' / '));

    // 23. Clicking away saves what was typed: no warning.
    await p.locator('.lib-edit input[id^="ted-year"]').fill('1999');
    await p.locator('.lr').nth(4).click();
    await p.waitForTimeout(300);
    check('clicking another track saves the edit, without asking', await p.locator('#modal-back').isHidden() && (await tagsOf(selIds)).every((x) => /\|1999$/.test(x)), (await tagsOf(selIds)).join(' / '));
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(300);
    check('and Ctrl+Z puts it back', JSON.stringify(await tagsOf(selIds)) === JSON.stringify(tagsBefore), (await tagsOf(selIds)).join(' / '));
    await p.locator('.lr').nth(0).click();
    await p.locator('.lr').nth(1).click({ modifiers: ['Shift'] });

    // 24. File names cleaned up from the tags, with undo.
    const pathsOf = (ids) => p.evaluate((ids) => ids.map((id) => Fatstick.app.S.tracks.get(id).path), ids);
    const pathsBefore = await pathsOf(selIds);
    await p.locator('.lib-head .btn', { hasText: 'Clean up file names' }).click();
    await p.locator('#modal select').selectOption({ label: '01 - Artist - Title' });
    const ren = await p.locator('#modal .ren-row:not(.same) .new').allTextContents();
    check('the preview shows the new names', ren.length === 2 && ren.every((n) => /^\d\d - .+ - .+\.mp3$/.test(n)), ren.join(' | '));
    await p.screenshot({ path: `${out}/rename.png` });
    await p.locator('#modal .mfoot .btn.primary').click();
    await toastSays(/Renamed 2 files/);
    const pathsAfter = await pathsOf(selIds);
    check('renaming keeps each file in its folder', pathsAfter.every((x, i) => x !== pathsBefore[i] && x.slice(0, x.lastIndexOf('/')) === pathsBefore[i].slice(0, pathsBefore[i].lastIndexOf('/'))), pathsAfter.join(' | '));
    await p.locator('#toast .btn').click();
    await toastSays(/Put back the old names/);
    check('undo puts the old names back', JSON.stringify(await pathsOf(selIds)) === JSON.stringify(pathsBefore));

    // Double-click plays a library track on its own.
    const roadWas = await p.locator('[data-pl="p-road"] .mt').textContent();
    const playMe = (await p.locator('.lr').nth(3).locator('.c-title .t').textContent());
    await p.locator('.lr').nth(3).dblclick();
    await p.waitForTimeout(150);
    check('double-clicking a library track plays it', (await p.locator('#player .np-t').textContent()) === playMe && (await p.locator('.lr.playing').count()) === 1 && /^PLAY/.test(await p.locator('#disp-text').textContent()), playMe);
    check('without adding it anywhere', (await p.locator('[data-pl="p-road"] .mt').textContent()) === roadWas);
    await p.keyboard.press('Space');
    check('Space pauses it', !(await p.evaluate(() => Fatstick.app.S.player.playing)));
    await p.evaluate(() => Fatstick.app.stopPlayback());
    await p.screenshot({ path: `${out}/library.png` });

    // 25. Back to the playlist: click one on the left.
    await p.locator('[data-pl="p-road"]').click();
    check('a playlist on the left leaves the library', !(await p.locator('#app').evaluate((a) => a.classList.contains('lib-mode'))) && await p.locator('#main').isVisible());

    // 26. Edit tags from a playlist row.
    const first = (await titles())[0];
    await row(0).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Edit tags…' }).click();
    check('"Edit tags…" opens the editor in a dialog', /Edit tags/.test(await p.locator('#modal-title').textContent()) && (await p.locator('#modal input[id^="ted-title"]').inputValue()) === first, first);
    await p.keyboard.press('Escape');

  }

  // 27. A track's key: the dialog, its slider and fine tune, Apply, Cancel and undo.
  {
    await p.locator('[data-pl="p-road"]').click();
    await row(2).click();
    await row(2).click();
    const kid = await p.evaluate(() => Fatstick.app.pl().entries[2].id);
    await p.keyboard.press('k');
    check('K opens the key dialog', /^Key of /.test(await p.locator('#modal-title').textContent()) && (await p.locator('#key-words').textContent()) === 'The original key');
    for (let i = 0; i < 3; i++) await p.locator('#key-semis').press('ArrowRight');
    check('the slider moves in semitones, named as intervals', (await p.locator('#key-words').textContent()) === '3 semitones up (a minor third)');
    await p.locator('#key-cents').fill('15');
    check('fine tune in cents', /and 15 cents up/.test(await p.locator('#key-words').textContent()) && /\+3\+15C/.test((await p.locator('#key-seg').textContent()).replace(/[! ]/g, '')));
    await p.locator('#key-keep').uncheck();
    await p.screenshot({ path: `${out}/key.png` });
    await p.locator('#modal .btn.primary').click();
    const ent = await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id), kid);
    check('Apply sets the entry\'s key', ent.pitch === 3.15 && ent.keepFormants === false, JSON.stringify(ent));
    check('the Key column shows it', (await p.locator(`#row-${kid} .c-key`).textContent()) === '+3 +15¢');
    const plan = await p.evaluate((id) => {
      const A = Fatstick.app;
      const f = A.planSet(A.stickSet()).folders.find((x) => x.items.some((it) => it.entry && it.entry.id === id));
      const it = f.items.find((x) => x.entry && x.entry.id === id);
      return { recipe: it.recipe, reason: it.d.reason };
    }, kid);
    check('it goes on the stick rendered, in its key', plan.recipe && plan.recipe.kind === 'render' && plan.recipe.parts[0].pitch === 3.15 && plan.recipe.parts[0].keepFormants === false, JSON.stringify(plan));
    await p.keyboard.press('Control+z');
    check('undo puts it back in its own key', !(await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).pitch, kid)));
    await p.keyboard.press('Control+y');
    // Cancel leaves it as it was.
    await row(2).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Key…' }).click();
    await p.locator('#key-semis').press('ArrowLeft');
    await p.getByRole('button', { name: 'Cancel' }).click();
    check('Cancel changes nothing', (await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).pitch, kid)) === 3.15);
    // A joined playlist keeps each track's key.
    await p.evaluate((id) => { const A = Fatstick.app; A.pl().join = true; }, kid);
    const parts = await p.evaluate(() => { const A = Fatstick.app; const f = A.planSet(A.stickSet()).folders.find((x) => x.p.id === A.pl().id); return f.files[0].recipe.parts.map((x) => x.pitch || 0); });
    check('a joined playlist keeps each track\'s key', parts.filter((x) => x === 3.15).length === 1 && parts.length > 10, JSON.stringify(parts));
    await p.evaluate(() => { Fatstick.app.pl().join = false; });
  }

  // 28. Fades: the dialog, its sliders and curves, hearing it, Apply, Cancel and undo.
  {
    await p.locator('[data-pl="p-road"]').click();
    // A track long enough for both fades.
    const fi = await p.evaluate(() => { const A = Fatstick.app; return A.pl().entries.findIndex((e, i) => i > 2 && A.track(e.trackId).duration >= 40 && !e.pitch); });
    await row(fi).click();
    const fid = await p.evaluate((i) => Fatstick.app.pl().entries[i].id, fi);
    await p.keyboard.press('f');
    check('F opens the fade dialog', /^Fades of /.test(await p.locator('#modal-title').textContent()) && (await p.locator('.fade-panel').count()) === 2);
    const curve = () => p.locator('.fade-panel.in .fade-line').getAttribute('d');
    const flat = await curve();
    await p.locator('#fade-in').fill('4');
    check('the fade-in slider draws its curve', (await curve()) !== flat && (await p.locator('.fade-panel.in .fade-val input').inputValue()) === '4');
    await p.locator('.fade-panel.out .fade-val input').fill('6.5');
    check('a number sets the fade-out too', (await p.locator('#fade-out').inputValue()) === '6.5');
    check('the player bar stays usable over the dimmed window', await p.evaluate(() => {
      const b = document.querySelector('#player .pbtn');
      const r = b.getBoundingClientRect();
      const m = document.getElementById('modal').getBoundingClientRect();
      return document.body.classList.contains('modal-player') && document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2).closest('#player') && m.bottom <= document.getElementById('player').getBoundingClientRect().top;
    }));
    await p.getByRole('button', { name: 'Hear the end' }).click();
    await p.waitForTimeout(100);
    const heard = await p.evaluate(() => ({ id: Fatstick.app.S.player.entryId, pos: Fatstick.app.S.player.pos, dur: Fatstick.app.S.player.dur }));
    check('"Hear the end" plays it just before the fade out', heard.id === fid && Math.abs(heard.pos - (heard.dur - 6.5 - 3)) < 0.6, JSON.stringify(heard));
    await p.locator('#player .pbtn.play').click();
    check('the player bar works with the dialog open', !(await p.evaluate(() => Fatstick.app.S.player.playing)) && !(await p.locator('#modal-back').isHidden()));
    await p.screenshot({ path: `${out}/fades.png` });
    await p.locator('#modal .btn.primary').click();
    const fe = await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id), fid);
    check('Apply sets the entry\'s fades', fe.fadeIn === 4 && fe.fadeOut === 6.5, JSON.stringify(fe));
    check('the Fades column shows them', (await p.locator(`#row-${fid} .c-fades`).textContent()) === '4 / 6.5 s');
    check('the dialog let go of the player bar', !(await p.evaluate(() => document.body.classList.contains('modal-player'))));
    const fplan = await p.evaluate((id) => {
      const A = Fatstick.app;
      const f = A.planSet(A.stickSet()).folders.find((x) => x.items.some((it) => it.entry && it.entry.id === id));
      const it = f.items.find((x) => x.entry && x.entry.id === id);
      return { recipe: it.recipe, reason: it.d.reason };
    }, fid);
    check('it goes on the stick rendered, with its fades', fplan.recipe && fplan.recipe.kind === 'render' && fplan.recipe.parts[0].fadeIn === 4 && fplan.recipe.parts[0].fadeOut === 6.5 && !/Cut or padded/.test(fplan.reason), JSON.stringify(fplan));
    await p.keyboard.press('Control+z');
    check('undo takes the fades off', !(await p.evaluate((id) => { const e = Fatstick.app.pl().entries.find((x) => x.id === id); return e.fadeIn || e.fadeOut; }, fid)));
    await p.keyboard.press('Control+y');
    // Cancel leaves them; "No fades" then Apply takes them off.
    await row(fi).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Fade in and out…' }).click();
    await p.locator('#fade-in').fill('10');
    await p.getByRole('button', { name: 'Cancel' }).click();
    check('Cancel changes nothing', (await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).fadeIn, fid)) === 4);
    await p.keyboard.press('f');
    await p.getByRole('button', { name: 'No fades' }).click();
    await p.locator('#modal .btn.primary').click();
    check('"No fades" takes them off', !(await p.evaluate((id) => { const e = Fatstick.app.pl().entries.find((x) => x.id === id); return e.fadeIn || e.fadeOut; }, fid)) && (await p.locator('.c-fades').count()) === 0);
    await p.keyboard.press('Control+z');
    await p.evaluate(() => Fatstick.app.stopPlayback());
  }

  // 29. Repeat: play & stop, repeat one song, repeat playlist, from the player bar.
  {
    await p.locator('[data-pl="p-road"]').click();
    const mode = () => p.locator('#repeat').getAttribute('aria-label');
    check('repeat starts as play & stop', (await mode()) === 'Repeat: Play & stop' && (await p.locator('#repeat').getAttribute('aria-pressed')) === 'false');
    const ends = async () => {
      await p.evaluate(() => { const A = Fatstick.app; A.seekTo(A.S.player.dur - 0.3); });
      await p.waitForTimeout(900);
      return p.evaluate(() => ({ id: Fatstick.app.S.player.entryId, pos: Fatstick.app.S.player.pos }));
    };
    const ids = await p.evaluate(() => Fatstick.app.pl().entries.filter((e) => Fatstick.app.track(e.trackId).exists).map((e) => e.id));
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[ids.length - 1]);
    check('play & stop: the last track ends, and that is all', !(await ends()).id);
    await p.locator('#repeat').click();
    check('one click: repeat one song', (await mode()) === 'Repeat: Repeat one song');
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[2]);
    const again = await ends();
    check('repeat one song plays it again from its start', again.id === ids[2] && again.pos < 2, JSON.stringify(again));
    check('the display says so while it plays', /RPT1/.test(await p.locator('#disp-unit').textContent()));
    await p.locator('#repeat').click();
    check('two clicks: repeat playlist', (await mode()) === 'Repeat: Repeat playlist' && (await p.locator('#repeat').getAttribute('aria-pressed')) === 'true');
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[ids.length - 1]);
    check('repeat playlist goes from the last track back to the first', (await ends()).id === ids[0]);
    await p.screenshot({ path: `${out}/repeat.png` });
    await p.keyboard.press('r');
    check('R steps it round to play & stop', (await mode()) === 'Repeat: Play & stop');
    await p.evaluate(() => Fatstick.app.stopPlayback());
  }

  // 30. Nothing in the sidebar scrolls sideways, however long the names.
  {
    await p.evaluate(() => { const A = Fatstick.app; A.pl('p-sleep').name = 'A playlist with a name far too long for any sidebar to show in full'; A.stickSet('set-gym').name = 'A stick whose name is also much too long'; A.setView('playlist'); });
    await p.locator('[data-pl="p-road"]').click();
    const side = await p.evaluate(() => { const e = document.querySelector('.side'); return [e.scrollWidth, e.clientWidth, getComputedStyle(e).overflowX]; });
    check('the sidebar never scrolls sideways', side[0] <= side[1] && side[2] === 'hidden', JSON.stringify(side));
  }

  // 31. A stick's setlist to print: Ctrl+P, each playlist in full, by name
  // or left out, its silences as lines, a preview of the pages, black and white.
  {
    await p.locator('[data-pl="p-road"]').click();
    const car = await p.evaluate(() => { const A = Fatstick.app; const s = A.stickSet(); return { id: s.id, name: s.name, items: s.playlistIds.map((id) => (id.startsWith('gap-') ? null : A.pl(id).name)) }; });
    await p.keyboard.press('Control+p');
    await p.waitForSelector('.sl-thumb .sl-page', { timeout: 5000 });
    check('Ctrl+P opens the chosen stick\'s setlist with a preview of its pages', (await p.locator('#modal-title').textContent()) === 'Print a setlist' && (await p.locator('.sl-thumb .sl-h1').first().textContent()) === car.name, car.name);
    const named = car.items.filter(Boolean);
    const sets = await p.locator('.sl-thumb .sl-set:not(.gap) .sl-set-name').allTextContents();
    check('each playlist on it is headed by its name, in the stick\'s order', JSON.stringify(sets) === JSON.stringify(named), sets.join(' | '));
    check('its silences are lines of their own', (await p.locator('.sl-thumb .sl-set.gap').count()) === car.items.filter((x) => x === null).length);
    const songsOf = async () => p.locator('.sl-thumb .sl-song').count();
    const all = await songsOf();
    // The second playlist as filler: its name only.
    await p.locator('.sl-pl').nth(car.items.indexOf(named[1])).locator('label', { hasText: 'Name only' }).click();
    await p.waitForTimeout(150);
    const only = await p.locator('.sl-thumb .sl-set.only:not(.gap) .sl-set-name').allTextContents();
    check('a playlist can show just its name (filler between sets)', JSON.stringify(only) === JSON.stringify([named[1]]) && (await songsOf()) < all, `${only.join(' | ')}; ${await songsOf()} of ${all} songs`);
    // The first left out.
    await p.locator('.sl-pl').nth(car.items.indexOf(named[0])).locator('label', { hasText: 'Leave out' }).click();
    await p.waitForTimeout(150);
    check('and one can be left out', !(await p.locator('.sl-thumb .sl-set-name').allTextContents()).includes(named[0]) && (await songsOf()) === 0);
    await p.locator('.sl-pl').nth(car.items.indexOf(named[0])).locator('label', { hasText: 'Songs' }).click();
    await p.waitForTimeout(150);
    const pages1 = await p.locator('.sl-thumb').count();
    const colours = await p.evaluate(() => [...document.querySelectorAll('.sl-thumb .sl-page, .sl-thumb .sl-page *')].map((e) => { const c = getComputedStyle(e); return [c.color, c.backgroundColor, c.borderTopColor]; }).flat().filter((c) => !['rgb(0, 0, 0)', 'rgb(255, 255, 255)', 'rgba(0, 0, 0, 0)'].includes(c)));
    check('the pages are black on white, nothing else', colours.length === 0, [...new Set(colours)].join(' '));
    await p.locator('.sl-seg label', { hasText: 'Landscape' }).click();
    await p.locator('.sl-seg[aria-label="Columns"] label', { hasText: /^3$/ }).click();
    await p.waitForTimeout(150);
    check('three columns, landscape: fewer pages', (await p.locator('.sl-thumb').count()) <= pages1 && (await p.locator('.sl-thumb').first().locator('.sl-col').count()) === 3);
    await p.locator('.sl-controls input[aria-label="Heading"]').fill('Saturday set');
    await p.locator('.sl-controls input[aria-label="Under the heading"]').fill('The Fox, 9pm');
    await p.locator('.sl-controls input[aria-label="Title size, points"]').fill('40');
    await p.waitForTimeout(150);
    check('the heading, the line under it and the size follow the settings', (await p.locator('.sl-thumb .sl-h1').first().textContent()) === 'Saturday set' && /The Fox, 9pm/.test(await p.locator('.sl-thumb .sl-h2').first().textContent())
      && Math.abs(parseFloat(await p.locator('.sl-thumb .sl-title').first().evaluate((e) => getComputedStyle(e).fontSize)) - 40 * 96 / 72) < 1);
    await p.getByRole('checkbox', { name: 'Numbers', exact: true }).uncheck();
    await p.waitForTimeout(150);
    check('numbers can be left off', (await p.locator('.sl-thumb .sl-n').count()) === 0);
    await p.screenshot({ path: `${out}/setlist.png` });
    // What prints: only those pages, each the paper's size.
    const n = await p.evaluate((id) => Fatstick.app.setlist.preparePrint(Fatstick.app.setlist.data(id)), car.id);
    await p.emulateMedia({ media: 'print' });
    const pdf = await p.pdf({ preferCSSPageSize: true });
    await p.emulateMedia({ media: 'screen' });
    const pdfPages = (pdf.toString('latin1').match(/\/Type\s*\/Page[^s]/g) || []).length;
    check('printing gives just the setlist, a page per page', pdfPages === n && n >= 1, `${pdfPages} printed, ${n} prepared`);
    // Rows too: two by two in landscape, quarters, with lines between them.
    await p.locator('.sl-seg[aria-label="Columns"] label', { hasText: /^2$/ }).click();
    await p.locator('.sl-seg[aria-label="Rows"] label', { hasText: /^2$/ }).click();
    await p.locator('.sl-controls input[aria-label="Title size, points"]').fill('20');
    await p.waitForTimeout(150);
    const q = p.locator('.sl-thumb').first();
    check('rows split the page: quarters in landscape, with lines between', (await q.locator('.sl-col').count()) === 4 && (await q.locator('.sl-div').count()) === 2 && /2 × 2 parts/.test(await p.locator('.sl-info').textContent()), await p.locator('.sl-info').textContent());
    const filled = await q.locator('.sl-col').evaluateAll((cs) => cs.map((c) => c.children.length));
    check('the songs fill the quarters in turn', filled[0] > 0 && filled.indexOf(0) === -1 || filled.slice(0, filled.indexOf(0)).every((x) => x > 0), JSON.stringify(filled));
    await p.screenshot({ path: `${out}/setlist-quarters.png` });
    await p.getByRole('button', { name: 'Back to the defaults' }).click();
    check('back to the defaults: one portrait column again', (await p.locator('.sl-seg:not(.small) input:checked + span').allTextContents()).join(',') === 'Portrait,1,1,Normal');
    await p.getByRole('button', { name: 'Close' }).click();
    // From the stick's view too.
    await p.locator('[data-set="set-car"]').click();
    await p.getByRole('button', { name: 'Print a setlist…' }).click();
    check('the stick\'s view prints its setlist', (await p.locator('#modal-title').textContent()) === 'Print a setlist' && await p.locator('.sl-thumb').first().isVisible());
    await p.getByRole('button', { name: 'Close' }).click();
    await p.locator('[data-pl="p-road"]').click();
  }

  check('no console errors', errs.length === 0, errs.join(' / '));
  await b.close();
  server.close();
  console.log(fails ? `${fails} FAILED` : 'ALL PASSED');
  process.exit(fails ? 1 : 0);
})();
