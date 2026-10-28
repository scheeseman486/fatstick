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
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'p2u-ui-'));
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
  await p.addInitScript(() => localStorage.setItem('p2u-proto', JSON.stringify({ look: 'backlit', theme: 'light', shell: 0 })));
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
  // "List all" opens the stick's layout window.
  const [lay0] = await Promise.all([p.waitForEvent('popup'), p.getByRole('button', { name: 'List all' }).click()]);
  await lay0.waitForSelector('.lay-folder', { timeout: 5000 });
  check('"List all" opens the layout window', (await lay0.locator('.lay-folder').count()) === 2);
  await lay0.close();

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

  // 14. A track's start and end: cut 2 s from the start of row 2 and add
  // 1.5 s of silence at its end. It's encoded rather than copied.
  await p.locator('#list .row').nth(1).click();
  await p.keyboard.press('e');
  await p.locator('#edge-cutStart').fill('2');
  await p.locator('#edge-padEnd').fill('1.5');
  await p.getByRole('button', { name: 'Apply' }).click();
  const edgeCell = await p.locator('#list .row').nth(1).locator('.c-edges').textContent();
  check('the start / end column shows the cut and the silence', edgeCell === '−2 / +1.5 s', edgeCell);
  const r2 = await p.evaluate(() => { const A = window.P2U.app; const it = A.planSet(A.stickSet()).folders[0].files[1]; return [it.recipe.kind, it.recipe.parts[0].cutStart, it.recipe.parts[0].padEnd, it.d.action]; });
  check('a cut track is rendered (encoded) on the stick', JSON.stringify(r2) === JSON.stringify(['render', 2, 1.5, 'encode']), JSON.stringify(r2));

  // 15. Silence between playlists, 10 s.
  await p.locator('#stick select[aria-label="Add a playlist to this stick"]').selectOption('__gap');
  await p.locator('#stick input[aria-label="Seconds of silence"]').fill('10');
  await p.locator('#stick input[aria-label="Seconds of silence"]').press('Enter');
  await p.locator('#stick input[aria-label="Seconds of silence"]').blur();
  const gapPlan = await p.evaluate(() => { const A = window.P2U.app; const f = A.planSet(A.stickSet()).folders.at(-1); return [f.folder, f.files.length, f.files[0].name, f.files[0].secs]; });
  check('silence gets a folder of its own, with one file', JSON.stringify(gapPlan) === JSON.stringify(["03 Silence", 1, "01 - Silence.mp3", 10]), JSON.stringify(gapPlan));

  // 16. Kids' car joined into one track: still a playlist here, one file on the stick.
  await p.locator('[data-pl="p-kids"]').click();
  await p.locator('#join-btn').click();
  await p.locator('#join-on').check();
  await p.keyboard.press('Escape');
  const chips = await p.locator('#list .c-stick .chip').allTextContents();
  check('every track shows where it starts in the joined track', chips.length === 12 && chips[0] === 'Joined, 0:00' && chips.every((c) => c.startsWith('Joined, ')), chips.slice(0, 3).join(' | '));
  check('the sidebar says the playlist goes on as one track', /12 tracks as one/.test(await p.locator('[data-pl="p-kids"] .mt').textContent()));
  const joinPlan = await p.evaluate(() => { const A = window.P2U.app; const f = A.planSet(A.stickSet()).folders[1]; return [f.files.length, f.present.length, f.files[0].name, f.files[0].recipe ? f.files[0].recipe.parts.length : -1]; });
  check('the stick gets one file for it, made of its 12 tracks', JSON.stringify(joinPlan) === JSON.stringify([1, 12, "01 - Kids' car.mp3", 12]), JSON.stringify(joinPlan));
  check("the stick panel counts it as joined", /12 tracks joined into one/.test(await p.locator('#stick .folders').textContent()));

  // 17. The stick's layout in a window of its own, from a double-click in the
  // sidebar: every folder, collapsible, kept up to date.
  const [lay] = await Promise.all([p.waitForEvent('popup'), p.locator('[data-set="set-car"]').dblclick()]);
  lay.on('pageerror', (e) => errs.push(`layout window: ${e.message}`));
  await lay.waitForSelector('.lay-folder', { timeout: 5000 });
  const heads = await lay.locator('.lay-folder > summary').allTextContents();
  check('the layout window lists the folders in stick order', heads.length === 3 && /^01 Road Trip\//.test(heads[0]) && /12 tracks joined into one file/.test(heads[1]) && /^03 Silence\/10 s of silence/.test(heads[2]), heads.join(' | '));
  check('a joined track lists its tracks with their start times', (await lay.locator('.lay-folder.k-joined .lay-parts li').count()) === 12);
  await lay.setViewportSize({ width: 640, height: 780 });
  await lay.screenshot({ path: `${out}/layout-window.png` });
  await lay.locator('.lay-folder > summary').first().click();
  check('folders collapse', !(await lay.locator('.lay-folder').first().evaluate((d) => d.open)));
  await p.locator('[data-pl="p-kids"]').click();
  await p.locator('#join-btn').click();
  await p.locator('#join-on').uncheck();
  await p.keyboard.press('Escape');
  await lay.waitForFunction(() => /12 files/.test(document.querySelectorAll('.lay-folder > summary')[1].textContent), null, { timeout: 5000 }).catch(() => {});
  check('the layout window follows edits', /^02 Kids' car\/12 files/.test(await lay.locator('.lay-folder > summary').nth(1).textContent()));
  await lay.close();
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
    await p.evaluate(() => P2U.api.seek(P2U.app.S.player.dur - 0.3));
    await p.waitForFunction(() => !P2U.app.S.player.trackId, null, { timeout: 3000 }).catch(() => {});
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

    // 22. Tags written into the files, after a preview, with undo.
    const tagsOf = (ids) => p.evaluate((ids) => ids.map((id) => { const t = P2U.app.S.tracks.get(id); return `${t.genre}|${t.labels.join(',')}`; }), ids);
    const selIds = await p.evaluate(() => P2U.app.lib.state.L.ids.filter((x) => P2U.app.lib.state.L.sel.has(x)));
    const tagsBefore = await tagsOf(selIds);
    await p.locator('.lib-edit input[id^="ted-genre"]').fill('Kids');
    await p.locator('.lib-edit input[id^="ted-lbl"]').fill('car');
    await p.keyboard.press('Enter');
    check('the editor shows the new label on all of them', (await p.locator('.lib-edit .tag.lbl.added', { hasText: 'car' }).count()) === 1);
    await p.locator('.lib-edit .ted-foot .btn.primary').click();
    const preview = await p.locator('#modal').textContent();
    check('saving shows what changes first', /Write these tags into 2 files/.test(preview) && /Genre/.test(preview) && /adds/.test(preview), preview.slice(0, 120));
    await p.screenshot({ path: `${out}/tag-preview.png` });
    await p.locator('#modal .mfoot .btn.primary').click();
    const toastSays = (re) => p.waitForFunction((src) => new RegExp(src).test(document.getElementById('toast').textContent), re.source, { timeout: 5000 }).catch(() => {});
    await toastSays(/Wrote tags/);
    check('then writes them', /Wrote tags into 2 files/.test(await p.locator('#toast').textContent()) && (await tagsOf(selIds)).every((x) => /^Kids\|.*car/.test(x)), (await tagsOf(selIds)).join(' / '));
    check('the table shows the new label', (await p.locator('.lr.sel .c-labels').evaluateAll((cs) => cs.filter((c) => c.title.split(', ').includes('car')).length)) === 2);
    await p.locator('#toast .btn').click();
    await toastSays(/Put the old tags back/);
    check('undo writes the old tags back', JSON.stringify(await tagsOf(selIds)) === JSON.stringify(tagsBefore), (await tagsOf(selIds)).join(' / '));

    // 23. Unsaved edits aren't lost by clicking elsewhere.
    await p.locator('.lib-edit input[id^="ted-year"]').fill('1999');
    await p.locator('.lr').nth(4).click();
    check('changing the selection asks first', /Discard your tag changes/.test(await p.locator('#modal-title').textContent()));
    await p.getByRole('button', { name: 'Keep editing' }).click();
    check('keeping them keeps the selection and the edit', (await p.locator('.lr.sel').count()) === 2 && (await p.locator('.lib-edit input[id^="ted-year"]').inputValue()) === '1999');
    await p.locator('.lib-edit .ted-foot .btn', { hasText: 'Revert' }).click();

    // 24. File names cleaned up from the tags, with undo.
    const pathsOf = (ids) => p.evaluate((ids) => ids.map((id) => P2U.app.S.tracks.get(id).path), ids);
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
    check('Space pauses it', !(await p.evaluate(() => P2U.app.S.player.playing)));
    await p.evaluate(() => P2U.app.stopPlayback());
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
    const kid = await p.evaluate(() => P2U.app.pl().entries[2].id);
    await p.keyboard.press('k');
    check('K opens the key dialog', /^Key of /.test(await p.locator('#modal-title').textContent()) && (await p.locator('#key-words').textContent()) === 'The original key');
    for (let i = 0; i < 3; i++) await p.locator('#key-semis').press('ArrowRight');
    check('the slider moves in semitones, named as intervals', (await p.locator('#key-words').textContent()) === '3 semitones up (a minor third)');
    await p.locator('#key-cents').fill('15');
    check('fine tune in cents', /and 15 cents up/.test(await p.locator('#key-words').textContent()) && /\+3\+15C/.test((await p.locator('#key-seg').textContent()).replace(/[! ]/g, '')));
    await p.locator('#key-keep').uncheck();
    await p.screenshot({ path: `${out}/key.png` });
    await p.locator('#modal .btn.primary').click();
    const ent = await p.evaluate((id) => P2U.app.pl().entries.find((e) => e.id === id), kid);
    check('Apply sets the entry\'s key', ent.pitch === 3.15 && ent.keepFormants === false, JSON.stringify(ent));
    check('the Key column shows it', (await p.locator(`#row-${kid} .c-key`).textContent()) === '+3 +15¢');
    const plan = await p.evaluate((id) => {
      const A = P2U.app;
      const f = A.planSet(A.stickSet()).folders.find((x) => x.items.some((it) => it.entry && it.entry.id === id));
      const it = f.items.find((x) => x.entry && x.entry.id === id);
      return { recipe: it.recipe, reason: it.d.reason };
    }, kid);
    check('it goes on the stick rendered, in its key', plan.recipe && plan.recipe.kind === 'render' && plan.recipe.parts[0].pitch === 3.15 && plan.recipe.parts[0].keepFormants === false, JSON.stringify(plan));
    await p.keyboard.press('Control+z');
    check('undo puts it back in its own key', !(await p.evaluate((id) => P2U.app.pl().entries.find((e) => e.id === id).pitch, kid)));
    await p.keyboard.press('Control+y');
    // Cancel leaves it as it was.
    await row(2).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Key…' }).click();
    await p.locator('#key-semis').press('ArrowLeft');
    await p.getByRole('button', { name: 'Cancel' }).click();
    check('Cancel changes nothing', (await p.evaluate((id) => P2U.app.pl().entries.find((e) => e.id === id).pitch, kid)) === 3.15);
    // A joined playlist keeps each track's key.
    await p.evaluate((id) => { const A = P2U.app; A.pl().join = true; }, kid);
    const parts = await p.evaluate(() => { const A = P2U.app; const f = A.planSet(A.stickSet()).folders.find((x) => x.p.id === A.pl().id); return f.files[0].recipe.parts.map((x) => x.pitch || 0); });
    check('a joined playlist keeps each track\'s key', parts.filter((x) => x === 3.15).length === 1 && parts.length > 10, JSON.stringify(parts));
    await p.evaluate(() => { P2U.app.pl().join = false; });
  }

  // 28. Fades: the dialog, its sliders and curves, hearing it, Apply, Cancel and undo.
  {
    await p.locator('[data-pl="p-road"]').click();
    // A track long enough for both fades.
    const fi = await p.evaluate(() => { const A = P2U.app; return A.pl().entries.findIndex((e, i) => i > 2 && A.track(e.trackId).duration >= 40 && !e.pitch); });
    await row(fi).click();
    const fid = await p.evaluate((i) => P2U.app.pl().entries[i].id, fi);
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
    const heard = await p.evaluate(() => ({ id: P2U.app.S.player.entryId, pos: P2U.app.S.player.pos, dur: P2U.app.S.player.dur }));
    check('"Hear the end" plays it just before the fade out', heard.id === fid && Math.abs(heard.pos - (heard.dur - 6.5 - 3)) < 0.6, JSON.stringify(heard));
    await p.locator('#player .pbtn.play').click();
    check('the player bar works with the dialog open', !(await p.evaluate(() => P2U.app.S.player.playing)) && !(await p.locator('#modal-back').isHidden()));
    await p.screenshot({ path: `${out}/fades.png` });
    await p.locator('#modal .btn.primary').click();
    const fe = await p.evaluate((id) => P2U.app.pl().entries.find((e) => e.id === id), fid);
    check('Apply sets the entry\'s fades', fe.fadeIn === 4 && fe.fadeOut === 6.5, JSON.stringify(fe));
    check('the Fades column shows them', (await p.locator(`#row-${fid} .c-fades`).textContent()) === '4 / 6.5 s');
    check('the dialog let go of the player bar', !(await p.evaluate(() => document.body.classList.contains('modal-player'))));
    const fplan = await p.evaluate((id) => {
      const A = P2U.app;
      const f = A.planSet(A.stickSet()).folders.find((x) => x.items.some((it) => it.entry && it.entry.id === id));
      const it = f.items.find((x) => x.entry && x.entry.id === id);
      return { recipe: it.recipe, reason: it.d.reason };
    }, fid);
    check('it goes on the stick rendered, with its fades', fplan.recipe && fplan.recipe.kind === 'render' && fplan.recipe.parts[0].fadeIn === 4 && fplan.recipe.parts[0].fadeOut === 6.5 && !/Cut or padded/.test(fplan.reason), JSON.stringify(fplan));
    await p.keyboard.press('Control+z');
    check('undo takes the fades off', !(await p.evaluate((id) => { const e = P2U.app.pl().entries.find((x) => x.id === id); return e.fadeIn || e.fadeOut; }, fid)));
    await p.keyboard.press('Control+y');
    // Cancel leaves them; "No fades" then Apply takes them off.
    await row(fi).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Fade in and out…' }).click();
    await p.locator('#fade-in').fill('10');
    await p.getByRole('button', { name: 'Cancel' }).click();
    check('Cancel changes nothing', (await p.evaluate((id) => P2U.app.pl().entries.find((e) => e.id === id).fadeIn, fid)) === 4);
    await p.keyboard.press('f');
    await p.getByRole('button', { name: 'No fades' }).click();
    await p.locator('#modal .btn.primary').click();
    check('"No fades" takes them off', !(await p.evaluate((id) => { const e = P2U.app.pl().entries.find((x) => x.id === id); return e.fadeIn || e.fadeOut; }, fid)) && (await p.locator('.c-fades').count()) === 0);
    await p.keyboard.press('Control+z');
    await p.evaluate(() => P2U.app.stopPlayback());
  }

  check('no console errors', errs.length === 0, errs.join(' / '));
  await b.close();
  server.close();
  console.log(fails ? `${fails} FAILED` : 'ALL PASSED');
  process.exit(fails ? 1 : 0);
})();
