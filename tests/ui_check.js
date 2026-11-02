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

  // 0. The processing panel: closed when the app starts, beside any view once
  // open. An open setlist opens the transfer and a track the tags; nothing at the
  // top of the window does. Left on the transfer for the checks after this.
  {
    const panel = p.locator('#stick');
    const btn = p.locator('#proc-fat');
    const car = p.locator('[data-set="set-car"]');
    const road = p.locator('[data-pl="p-road"]');
    const title = () => p.locator('#proc-title').textContent();
    const right = (sel) => p.evaluate((s) => document.querySelector(s).getBoundingClientRect().right, sel);
    const pressed = async () => (await btn.getAttribute('aria-pressed')) === 'true';
    const settled = () => p.waitForTimeout(260);
    check('the processing panel is closed when the app starts', await panel.isHidden());
    check('nothing at the top of the window opens it', (await p.locator('.top-tools button').allTextContents()).join() === 'Library,Menu');
    await p.locator('#app-menu').click();
    check('and nothing in the menu', (await p.locator('#menu').getByText('FAT USB Stick transfer').count()) === 0 && (await p.locator('#menu').getByText('Tags', { exact: true }).count()) === 0);
    await p.keyboard.press('Escape');
    check('the playlist has the whole width', Math.abs(await right('#main') - await right('#player')) < 2);
    await p.screenshot({ path: `${out}/panel-closed.png` });
    const colsClosed = await p.locator('#list-cols > *').count();
    // An open setlist has a section for getting it out of the app.
    check('a playlist has no transfer button', (await btn.count()) === 0);
    await car.click();
    check('an open setlist has a section for transferring it', await p.locator('.sv-transfer').isVisible() && /Transfer this setlist/.test(await p.locator('.sv-transfer').textContent()));
    check('with the FAT USB Stick transfer in it', (await p.locator('.sv-transfer #proc-fat').textContent()) === 'FAT USB Stick transfer' && !(await pressed()));
    await btn.click();
    await settled();
    const box = await panel.boundingBox();
    check('the button opens the panel at the right of the window', await panel.isVisible() && await pressed() && Math.round(box.width) === 344 && Math.abs(box.x + box.width - await right('#player')) < 2, JSON.stringify(box));
    check('its title names it', (await title()) === 'FAT USB Stick transfer');
    check('the setlist, device, profile and sync are all in it', await p.locator('#set-select').isVisible() && await p.locator('#profile-select').isVisible() && await p.locator('#sync-btn').isVisible());
    await p.screenshot({ path: `${out}/panel-open.png` });
    await p.locator('#proc-close').click();
    await panel.waitFor({ state: 'hidden' });
    check('its close button closes it, and the focus goes back to the button', !(await pressed()) && await p.evaluate(() => document.activeElement.id === 'proc-fat'));
    check('the setlist has the whole width again', Math.abs(await right('#stickview') - await right('#player')) < 2);
    await btn.click();
    await btn.click();
    await panel.waitFor({ state: 'hidden' });
    check('the button closes it too', !(await pressed()));
    await btn.click();
    await btn.click();
    await btn.click();
    await settled();
    check('quick clicks end up where the last one says', await panel.isVisible() && await pressed());
    await p.reload();
    await p.waitForTimeout(600);
    check('it is closed again after a restart, whatever it was', await panel.isHidden());
    // It stays open from view to view until it's closed.
    await car.click();
    await btn.click();
    await settled();
    await road.click();
    await settled();
    check('it stays open going to a playlist, which makes room', await panel.isVisible() && await right('#main') < (await panel.boundingBox()).x + 1 && colsClosed >= (await p.locator('#list-cols > *').count()));
    await p.locator('#lib-btn').click();
    await settled();
    check('and beside the library', await panel.isVisible() && await right('#libview') <= (await panel.boundingBox()).x + 1);
    await p.locator('#lib-btn').click();
    await settled();
    check('back from the library, it is as it was', await panel.isVisible() && (await title()) === 'FAT USB Stick transfer');
    await p.locator('#proc-close').click();
    await panel.waitFor({ state: 'hidden' });
    await p.locator('#lib-btn').click();
    check('closed, the library has the whole window', Math.abs(await right('#libview') - await right('#player')) < 2);
    await p.locator('#lib-btn').click();
    // Tags, in the same panel: they're about a track, so a track opens them.
    await row(0).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Edit tags' }).click();
    check('"Edit tags" opens the panel on the tags', (await title()) === 'Tags' && await p.locator('.st-tags').isVisible() && await p.locator('#sync-btn').isHidden());
    await settled();
    check('it fits its column, nothing cut off', Math.round((await panel.boundingBox()).width) === 304 && await panel.evaluate((x) => x.scrollWidth <= x.clientWidth));
    await p.locator('#list').focus();
    await p.keyboard.press('Escape');
    check('with nothing selected it says what to do', /Select a track in a playlist or the library/.test(await p.locator('.st-tags').textContent()));
    await car.click();
    await btn.click();
    check('the setlist\'s button switches it over, without closing it', (await title()) === 'FAT USB Stick transfer' && await p.locator('#sync-btn').isVisible() && await p.locator('.st-tags').isHidden() && await pressed());
    await road.click();
    // A stick that's plugged in while it's closed says so, with the way in.
    await p.locator('#proc-close').click();
    await panel.waitFor({ state: 'hidden' });
    await p.evaluate(async () => { const A = Fatstick.app; A.S.sticks = []; await A.refreshSticks(true); });
    check('a stick plugged in shows in the sidebar with the panel closed', /plugged in/.test(await p.locator('[data-set="set-car"] .flag').textContent()));
    await p.locator('#toast').getByRole('button', { name: 'Open the transfer' }).click();
    await settled();
    check('and its toast opens the panel', await panel.isVisible() && (await title()) === 'FAT USB Stick transfer');
    // A narrow window: under the list.
    await p.setViewportSize({ width: 800, height: 700 });
    await settled();
    const nb = await panel.boundingBox();
    check('in a narrow window it goes under the list, full width', nb.width > 700 && nb.y > (await p.locator('#main').boundingBox()).y);
    await p.locator('#proc-close').click();
    await panel.waitFor({ state: 'hidden' });
    await p.setViewportSize({ width: 1280, height: 800 });
    await p.evaluate(() => Fatstick.app.setProcessing('fat'));
    await settled();
    check('and back at the side in a wide one', await panel.isVisible() && Math.round((await panel.boundingBox()).width) === 344);
  }

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
  // and keeps focus; the panel shows an example.
  check('the panel shows an example name', /Files named like 01 /.test(await p.locator('.prof-names').textContent()));
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
  check('the panel follows', /Files named like 01 - /.test(await p.locator('.prof-names').textContent()));

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
  // "List all" shows the setlist in the middle of the window.
  await p.getByRole('button', { name: 'List all' }).click();
  check('"List all" shows the setlist in the middle of the window', await p.locator('#stickview').isVisible() && (await p.locator('#stickview .lay-folder').count()) === 2);
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

  // 13. Sticks that remember what they hold: the Car mix's own stick is
  // marked in the sidebar and in the panel's list; another computer's stick is
  // named; syncing the Gym clip onto the Car mix's stick needs a tick first.
  check('the plugged-in stick is marked in the sidebar', /plugged in/.test(await p.locator('[data-set="set-car"] .flag').textContent()));
  const holds = await p.locator('#stick .dev .dholds').allTextContents();
  check('the stick list says what each stick holds',
    holds.some(t => /^Holds this setlist, synced 3 days ago/.test(t)) && holds.some(t => /^Holds Dad's car \(Road songs\), synced .* ago, set up on another computer\./.test(t)), holds.join(' / '));
  await p.locator('[data-set="set-gym"]').click();
  await p.locator('#stick .dev input[value="dev-sandisk"]').check();
  check('picking another setlist\'s stick warns', /This stick holds Car mix \(Road Trip, Kids' car\).*Syncing Gym clip to it replaces that/.test(await p.locator('#holds-other').textContent()));
  await p.locator('#sync-btn').click();
  check('the confirmation asks before replacing it', /Car mix will need a stick of its own/.test(await p.locator('#replace-warning').textContent()) && await p.locator('#modal .btn.primary').isDisabled());
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
  // The cut reaches a minute and a half: the slider goes to 1:30, and a cut
  // past the close-up half minute widens the picture (which can then close in again).
  {
    const dur = await p.evaluate(() => { const A = Fatstick.app; return A.track(A.pl().entries[1].trackId).duration; });
    const axisEnd = () => p.locator('.edge-panel.start .fade-axis > span').last().textContent();
    check('the cut slider reaches a minute and a half', Number(await p.locator('#edge-cutStart-range').getAttribute('max')) === Math.min(90, Math.round((dur - 1) * 10) / 10), String(dur));
    check('the picture starts close up, on the first half minute', (await axisEnd()) === '0:30' && (await p.locator('.edge-panel.start .edge-zoom-btn[aria-pressed="true"]').textContent()) === '0:30');
    await p.locator('#edge-cutStart-range').evaluate((el) => { el.value = '75'; el.dispatchEvent(new Event('input')); });
    check('a cut of 75 s from the slider widens the picture to 1:30', (await p.locator('#edge-cutStart').inputValue()) === '75' && (await axisEnd()) === '1:30' && await p.locator('.edge-panel.start .edge-zoom-btn').first().isDisabled());
    const cutW = await p.locator('.edge-panel.start .edge-cut').evaluate((el) => Number(el.getAttribute('width')));
    check('the cut is drawn most of the way across', cutW > 240 && cutW < 260, String(cutW));
    await p.locator('#edge-cutStart').fill('3');
    await p.locator('.edge-panel.start .edge-zoom-btn').first().click();
    check('once the cut is back inside, the picture can close in again', (await axisEnd()) === '0:30' && (await p.locator('.edge-panel.start .edge-zoom-btn').first().getAttribute('aria-pressed')) === 'true');
  }
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
  await p.locator('#stick select[aria-label="Add a playlist to this setlist"]').selectOption('__gap');
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

  // 17. The setlist's view, from the sidebar: every folder in order,
  // collapsible, for organizing the setlist's playlists.
  const carIds = () => p.evaluate(() => Fatstick.app.stickSet('set-car').playlistIds.slice());
  const folders = () => p.locator('#stickview .lay-folder');
  await p.locator('[data-set="set-car"]').click();
  check('clicking a setlist shows it in the middle of the window', await p.locator('#stickview').isVisible() && await p.locator('#main').isHidden() && (await p.locator('[data-set="set-car"]').getAttribute('aria-current')) === 'page');
  const heads = await p.locator('#stickview .lay-folder > summary, #stickview .lay-sumline').allTextContents();
  check('it lists the folders in order', heads.length === 3 && /^01 Road Trip\//.test(heads[0]) && /12 tracks joined into one file/.test(heads[1]) && /^03 Silence\/s of silence/.test(heads[2]), heads.join(' | '));
  check('a joined track lists its tracks with their start times', (await p.locator('#stickview .lay-folder.k-joined .lay-parts li').count()) === 12);
  check('nothing in a playlist is edited here', (await p.locator('#stickview input[type="checkbox"]').count()) === 0);
  check('the panel leaves the list to the view', await p.locator('#stick [aria-label="Playlists in this setlist"]').isHidden());
  check('the display shows the setlist', /CAR MIX/.test((await p.locator('#disp-text').textContent()).toUpperCase()) && /DIR/.test(await p.locator('#disp-unit').textContent()));
  await p.screenshot({ path: `${out}/setlist-view.png` });
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
  check('dragging a playlist from the left puts it where it\'s dropped', (await carIds())[0] === 'p-gym' && /Add Gym here/.test(badge), `${badge}: ${JSON.stringify(await carIds())}`);
  check('the drop doesn\'t open the playlist', await p.locator('#stickview').isVisible());
  // ... and the handles move them by dragging too.
  await p.getByRole('button', { name: 'Collapse all' }).click();
  await dragPl(folders().first().locator('.sv-grip'), folders().last(), 0.9);
  check('dragging a handle moves it', (await carIds()).at(-1) === 'p-gym', JSON.stringify(await carIds()));
  await p.keyboard.press('Control+z');
  await p.keyboard.press('Control+z');
  check('undo puts the setlist back as it was', JSON.stringify(await carIds()) === JSON.stringify(ids0), JSON.stringify(await carIds()));
  // Onto a setlist in the sidebar: it goes on the end of that setlist.
  await dragPl(p.locator('[data-pl="p-sleep"]'), p.locator('[data-set="set-gym"]'), 0.5);
  check('a playlist dropped on a setlist on the left goes in it', await p.evaluate(() => Fatstick.app.stickSet('set-gym').playlistIds.includes('p-sleep')));
  await p.keyboard.press('Control+z');
  // Taken out, with undo.
  await folders().nth(1).hover();
  await folders().nth(1).getByRole('button', { name: /Take .* out of this setlist/ }).click();
  check('a playlist can be taken out', (await carIds()).length === 2 && !(await carIds()).includes('p-kids'));
  await p.keyboard.press('Control+z');
  check('and put back with undo', (await carIds()).includes('p-kids'));
  // A change to a playlist shows here too.
  await p.locator('[data-pl="p-kids"]').click();
  await p.locator('#join-btn').click();
  await p.locator('#join-on').uncheck();
  await p.keyboard.press('Escape');
  await p.locator('[data-set="set-car"]').click();
  check('the setlist view follows edits made in the playlist', /12 files/.test(await folders().nth(1).locator('summary').textContent()));
  await p.locator('[data-pl="p-road"]').click();

  // 18. Sync flow. First one that fails with the panel closed: the toast says
  // so and shows why.
  {
    const realSync = await p.evaluateHandle(() => { const was = Fatstick.api.sync; Fatstick.api.sync = () => new Promise((r) => setTimeout(() => r({ ok: false, error: 'The stick went away.' }), 500)); return was; });
    await p.locator('#sync-btn').click();
    await p.locator('#modal .btn.primary').click();
    await p.locator('#proc-close').click();
    await p.locator('#stick').waitFor({ state: 'hidden' });
    await p.locator('#toast').getByRole('button', { name: 'See why' }).click();
    await p.locator('#stick').waitFor({ state: 'visible' });
    check('a sync that fails with the panel closed shows why when asked', /The stick went away/.test(await p.locator('.sync-done.bad').textContent()));
    await p.getByRole('button', { name: 'Done' }).click();
    await p.evaluate((was) => { Fatstick.api.sync = was; }, realSync);
  }
  await p.locator('#sync-btn').click();
  check('confirmation dialog', /Erase .* and sync/.test(await p.locator('#modal-title').textContent()));
  await p.screenshot({ path: `${out}/confirm.png` });
  await p.locator('#modal .btn.primary').click();
  await p.waitForTimeout(2500);
  await p.screenshot({ path: `${out}/syncing.png` });
  // It goes on with the panel closed: the display and the open setlist say so.
  await p.locator('#proc-close').click();
  await p.locator('#stick').waitFor({ state: 'hidden' });
  check('a sync goes on with the panel closed, and the display says so', /%/.test(await p.locator('#disp-unit').textContent()));
  await p.locator('[data-set="set-car"]').click();
  check('and so does the setlist\'s button', (await p.locator('#proc-fat.busy').count()) === 1);
  await p.waitForSelector('#proc-fat:not(.busy)', { timeout: 30000 });
  check('its button stops saying so when it ends', /^Synced /.test(await p.locator('#toast').textContent()) && await p.locator('#stick').isHidden());
  await p.locator('#proc-fat').click();
  await p.waitForSelector('.sync-done', { timeout: 3000 });
  const done = await p.locator('.sync-done').textContent();
  check('sync finishes', /^Synced\./.test(done), done);
  await p.getByRole('button', { name: 'Done' }).click();
  check('stick is up to date afterwards', /Up to date/.test(await p.locator('.st-status').textContent()));
  await p.screenshot({ path: `${out}/synced.png` });
  await p.locator('[data-pl="p-road"]').click();

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
    await p.locator('.srch-close').click();

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
    const settled = (re) => p.waitForFunction((src) => new RegExp(src).test(document.querySelector('.st-tags .ted-status').textContent), re.source, { timeout: 5000 }).catch(() => {});
    await p.locator('#lib-tags').click();
    check('the Tags panel shows what\'s selected in the library', /2 tracks/.test(await p.locator('.st-tags .ted-head h3').textContent()));
    check('the editor says edits save as they are made', /saved into the files as you make them/.test(await p.locator('.st-tags .ted-status').textContent()));
    await p.locator('.st-tags input[id^="ted-genre"]').fill('Kids');
    await p.keyboard.press('Enter');
    await settled(/Saved: Genre/);
    check('Enter in a field writes it into the files, without asking', (await tagsOf(selIds)).every((x) => /^Kids\|/.test(x)) && await p.locator('#modal-back').isHidden(), (await tagsOf(selIds)).join(' / '));
    check('and says what was saved', /Saved: Genre “Kids”, in 2 files\./.test(await p.locator('.st-tags .ted-status').textContent()));
    await p.locator('.st-tags input[id^="ted-lbl"]').fill('car');
    await p.keyboard.press('Enter');
    await settled(/label “car” added/);
    check('a label is written as soon as it is added', (await tagsOf(selIds)).every((x) => /^Kids\|.*car/.test(x)), (await tagsOf(selIds)).join(' / '));
    check('the table shows the new label', (await p.locator('.lr.sel .c-labels').evaluateAll((cs) => cs.filter((c) => [...c.querySelectorAll('.tag')].some((t) => t.textContent === 'car' || /\bcar\b/.test(t.getAttribute('aria-label') || ''))).length)) === 2);
    await p.screenshot({ path: `${out}/tags-saved.png` });
    await p.locator('.st-tags .ted-status .btn', { hasText: 'Undo' }).click();
    await p.waitForFunction(() => Fatstick.app.lib.state.L.ids.filter((x) => Fatstick.app.lib.state.L.sel.has(x)).every((id) => !Fatstick.app.S.tracks.get(id).labels.includes('car')), null, { timeout: 5000 }).catch(() => {});
    check('Undo beside it takes the label off again', (await tagsOf(selIds)).every((x) => /^Kids\|/.test(x) && !/car/.test(x)), (await tagsOf(selIds)).join(' / '));
    await p.locator('.lib-scroll').focus();
    await p.keyboard.press('Control+z');
    await p.waitForFunction((want) => JSON.stringify(Fatstick.app.lib.state.L.ids.filter((x) => Fatstick.app.lib.state.L.sel.has(x)).map((id) => { const t = Fatstick.app.S.tracks.get(id); return `${t.genre}|${t.labels.join(',')}|${t.year || ''}`; })) === want, JSON.stringify(tagsBefore), { timeout: 5000 }).catch(() => {});
    check('Ctrl+Z puts the genre back', JSON.stringify(await tagsOf(selIds)) === JSON.stringify(tagsBefore), (await tagsOf(selIds)).join(' / '));

    // 23. Clicking away saves what was typed: no warning.
    await p.locator('.st-tags input[id^="ted-year"]').fill('1999');
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

    // 26. Tags in a playlist's view: the same panel, following its selection.
    const tTitle = p.locator('.st-tags input[id^="ted-title"]');
    const tGenre = p.locator('.st-tags input[id^="ted-genre"]');
    check('the Tags panel stays open going back to a playlist', await p.locator('.st-tags').isVisible() && (await p.locator('#proc-title').textContent()) === 'Tags');
    await p.locator('#proc-close').click();
    await p.locator('#stick').waitFor({ state: 'hidden' });
    check('closing it puts the focus back on the list', await p.evaluate(() => document.activeElement.id === 'list'));
    const first = (await titles())[0];
    await row(0).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Edit tags' }).click();
    check('"Edit tags" opens the Tags panel on that track, in no dialog', await p.locator('#modal-back').isHidden() && (await p.locator('#proc-title').textContent()) === 'Tags' && (await tTitle.inputValue()) === first, first);
    check('with its first field ready to type in', await tTitle.evaluate((x) => document.activeElement === x));
    await row(1).click();
    check('clicking another track shows its tags there', (await tTitle.inputValue()) === (await titles())[1]);
    check('and no row opens its tags as well', (await p.locator('#list .row-tags').count()) === 0);
    await p.keyboard.press('t');
    check('T leaves the rows alone while the panel has the tags', (await p.locator('#list .row-tags').count()) === 0);
    await row(1).click({ button: 'right' });
    check('the menu doesn\'t offer to show them under the row either', (await p.getByRole('menuitem', { name: 'Show the tags' }).count()) === 0);
    await p.keyboard.press('Escape');
    await row(2).click({ modifiers: ['Shift'] });
    check('selecting several edits them together', /2 tracks/.test(await p.locator('.st-tags .ted-head h3').textContent()) && (await tTitle.getAttribute('placeholder')) === 'Various');
    await p.waitForTimeout(250);
    check('its fields fit the panel', await p.evaluate(() => { const box = document.getElementById('stick'); const r = box.getBoundingClientRect(); return box.scrollLeft === 0 && box.scrollWidth <= box.clientWidth && [...box.querySelectorAll('.ted-grid input:not(.num-in)')].every((i) => { const b = i.getBoundingClientRect(); return b.left >= r.left && b.right <= r.right; }); }));
    await p.screenshot({ path: `${out}/tags-playlist.png` });
    // A change from here goes into the files like one in the library.
    const plTags = await p.evaluate(() => { const A = Fatstick.app; return A.pl().entries.slice(1, 3).map((e) => e.trackId); });
    const lenBefore = (await titles()).length;
    // Typing here isn't a shortcut for the list behind it.
    const tComposer = p.locator('.st-tags input[id^="ted-composer"]');
    await tComposer.type('kfet');
    await tComposer.press('Delete');
    check('typing in a field leaves the playlist alone', (await titles()).length === lenBefore && await p.locator('#modal-back').isHidden() && (await tComposer.inputValue()) === 'kfet');
    await tComposer.press('Escape');
    check('and Esc puts the field back, unsaved', (await tComposer.inputValue()) === '');
    await tGenre.fill('Playlist Rock');
    await tGenre.press('Enter');
    await settled(/Saved: Genre/);
    check('a tag edited in a playlist\'s view is written into the files', (await p.evaluate((ids) => ids.map((id) => Fatstick.app.track(id).genre), plTags)).every((g) => g === 'Playlist Rock'));
    await p.locator('.st-tags .ted-status .btn', { hasText: 'Undo' }).click();
    await settled(/saved into the files|^$/);
    await p.waitForFunction((ids) => ids.every((id) => Fatstick.app.track(id).genre !== 'Playlist Rock'), plTags, { timeout: 5000 }).catch(() => {});
    check('and its Undo puts it back', (await p.evaluate((ids) => ids.map((id) => Fatstick.app.track(id).genre), plTags)).every((g) => g !== 'Playlist Rock'));
    // Search results beside the playlist: the last selection made is the one shown.
    await p.keyboard.press('Escape');
    await p.locator('#srch-btn').click();
    await p.locator('#srch-in').fill('Everywhere');
    await p.locator('.srch-res .sr').first().waitFor();
    await p.locator('.srch-res .sr').first().click();
    check('a search result shows its tags there', (await tTitle.inputValue()) === 'Everywhere');
    await row(0).click();
    check('and a click in the playlist takes it back', (await tTitle.inputValue()) === first);
    await p.locator('.srch-res .sr').first().click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Edit tags' }).click();
    check('"Edit tags" on a search result shows that one', (await tTitle.inputValue()) === 'Everywhere');
    await p.locator('.srch-close').click();
    check('with the search closed, it\'s the playlist\'s selection again', (await tTitle.inputValue()) === first);
    await p.locator('#srch-btn').click();
    check('open again, it\'s the result again', (await tTitle.inputValue()) === 'Everywhere');
    await p.locator('#srch-in').fill('');
    await p.waitForFunction(() => document.querySelectorAll('.srch-res .sr').length === 0);
    check('a selected result that goes from the list goes from the panel', (await tTitle.inputValue()) === first);
    await p.locator('.srch-close').click();
    // A setlist's view has no tracks to select.
    await p.locator('[data-set="set-car"]').click();
    check('a setlist\'s view has nothing selected', /Select a track in a playlist or the library/.test(await p.locator('.st-tags').textContent()));
    await p.locator('[data-pl="p-road"]').click();
    check('back in the playlist, its selection shows again', (await tTitle.inputValue()) === first);
    // In the library: its selection, and "Edit tags" from its foot and its menu.
    await p.locator('#proc-close').click();
    await p.locator('#stick').waitFor({ state: 'hidden' });
    await p.locator('#lib-btn').click();
    await p.locator('.lr').first().click();
    await p.locator('#lib-tags').click();
    check('its button goes while the panel has the tags', (await p.locator('#lib-tags').count()) === 0);
    check('"Edit tags" in the library opens the panel on its selection', (await p.locator('#proc-title').textContent()) === 'Tags' && await p.locator('#libview').isVisible() && (await tTitle.inputValue()) === (await p.locator('.lr.sel').first().locator('.lc-title, span').first().textContent()).trim());
    await p.locator('#proc-close').click();
    await p.locator('#stick').waitFor({ state: 'hidden' });
    await p.locator('.lr').nth(1).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Edit tags' }).click();
    check('and so does its right-click menu', await p.locator('.st-tags').isVisible() && await p.locator('#libview').isVisible());
    await p.locator('#lib-btn').click();
    // A file outside the music folder: shown, not editable.
    const outsider = await p.evaluate(() => { const A = Fatstick.app; const e = A.pl().entries.find((x) => !A.lib.inMusic(A.track(x.trackId))); return e ? A.pl().entries.indexOf(e) : -1; });
    if (outsider >= 0) {
      await row(outsider).click();
      check('a file outside the music folder shows its tags, and says why they can\'t change', await tTitle.isDisabled() && /Only files in your music folder/.test(await p.locator('.st-tags .ted-note').textContent()));
    }
    // The transfer again, for what follows.
    await p.locator('[data-set="set-car"]').click();
    await p.locator('#proc-fat').click();
    await p.locator('[data-pl="p-road"]').click();
    check('the transfer stays open going back to the playlist', await p.locator('#sync-btn').isVisible());
    await row(0).click();
    check('and clicking a row shows its tags under it again', (await p.locator('#list .row-tags').count()) <= 1);

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
    // Shapes: smooth to start with; a pick redraws the curve and says how it sounds.
    const outCurve = () => p.locator('.fade-panel.out .fade-line').getAttribute('d');
    check('each end offers six shapes, smooth picked', (await p.locator('.fade-panel.out [role="radio"]').count()) === 6 && (await p.locator('.fade-panel.out [role="radio"][aria-checked="true"]').getAttribute('data-shape')) === 'smooth');
    const smoothOut = await outCurve();
    await p.locator('.fade-panel.out [role="radio"][data-shape="late"]').click();
    check('picking "Late" redraws the fade out and says how it sounds', (await outCurve()) !== smoothOut && /Holds the volume/.test(await p.locator('.fade-panel.out .fade-shape-hint').textContent()));
    // The arrow keys move along the shapes.
    await p.locator('.fade-panel.in [role="radio"][aria-checked="true"]').focus();
    await p.keyboard.press('ArrowRight');
    check('an arrow key picks the next shape', (await p.locator('.fade-panel.in [role="radio"][aria-checked="true"]').getAttribute('data-shape')) === 'linear' && (await p.evaluate(() => document.activeElement.dataset.shape)) === 'linear');
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
    check('and their shapes', fe.fadeInShape === 'linear' && fe.fadeOutShape === 'late', JSON.stringify(fe));
    check('the Fades column shows them', (await p.locator(`#row-${fid} .c-fades`).textContent()) === '4 / 6.5 s' && /in over 4 s \(linear\) and out over 6.5 s \(late\)/.test(await p.locator(`#row-${fid} .c-fades button`).getAttribute('title')));
    check('the dialog let go of the player bar', !(await p.evaluate(() => document.body.classList.contains('modal-player'))));
    const fplan = await p.evaluate((id) => {
      const A = Fatstick.app;
      const f = A.planSet(A.stickSet()).folders.find((x) => x.items.some((it) => it.entry && it.entry.id === id));
      const it = f.items.find((x) => x.entry && x.entry.id === id);
      return { recipe: it.recipe, reason: it.d.reason };
    }, fid);
    check('it goes on the stick rendered, with its fades', fplan.recipe && fplan.recipe.kind === 'render' && fplan.recipe.parts[0].fadeIn === 4 && fplan.recipe.parts[0].fadeOut === 6.5 && !/Cut or padded/.test(fplan.reason), JSON.stringify(fplan));
    check('in their shapes, which make it another render', fplan.recipe.parts[0].fadeInShape === 'linear' && fplan.recipe.parts[0].fadeOutShape === 'late'
      && await p.evaluate((r) => { const R = Fatstick.rules; const plain = { ...r, parts: [{ ...r.parts[0], fadeInShape: undefined, fadeOutShape: undefined }] }; return R.describe(r) !== R.describe(plain); }, fplan.recipe), JSON.stringify(fplan.recipe));
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
    check('"No fades" takes them off, shapes and all', !(await p.evaluate((id) => { const e = Fatstick.app.pl().entries.find((x) => x.id === id); return e.fadeIn || e.fadeOut || e.fadeInShape || e.fadeOutShape; }, fid)) && (await p.locator('.c-fades').count()) === 0);
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
    check('Ctrl+P opens the chosen setlist with a preview of its pages', (await p.locator('#modal-title').textContent()) === 'Print a setlist' && (await p.locator('.sl-thumb .sl-h1').first().textContent()) === car.name, car.name);
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
    await p.locator('#modal').getByRole('button', { name: 'Close' }).click();
    // From the setlist's view too.
    await p.locator('[data-set="set-car"]').click();
    await p.locator('#stickview').getByRole('button', { name: 'Print…' }).click();
    check('the setlist\'s view prints it', (await p.locator('#modal-title').textContent()) === 'Print a setlist' && await p.locator('.sl-thumb').first().isVisible());
    await p.locator('#modal').getByRole('button', { name: 'Close' }).click();
    await p.locator('[data-pl="p-road"]').click();
  }

  // 32. Devices left out: "Exclude…" in the transfer panel; ticked ones aren't shown
  // or offered, each change is saved as it's made, and the stick panel says
  // nothing about them.
  {
    await p.locator('[data-pl="p-road"]').click();
    const devs = () => p.locator('#stick .dev').count();
    const n0 = await devs();
    const box = () => p.locator('.ex-dev', { hasText: 'Generic Flash Disk' }).locator('input');
    await p.locator('#ex-btn').click();
    await p.locator('.ex-dev').first().waitFor();
    check('"Exclude…" lists what is plugged in', /^Exclude devices$/.test(await p.locator('#modal-title').textContent()) && (await p.locator('.ex-dev').count()) === n0, `${await p.locator('.ex-dev').count()} of ${n0}`);
    await box().check();
    await p.locator('.ex-status', { hasText: 'Saved' }).waitFor();
    check('ticking one saves it and takes it out of the stick list at once', (await devs()) === n0 - 1 && !(await p.locator('#stick .dev', { hasText: 'Generic Flash Disk' }).count())
      && /Saved: Generic Flash Disk is excluded/.test(await p.locator('.ex-status').textContent()));
    await p.getByRole('button', { name: 'Done' }).click();
    check('the stick panel says nothing about excluded devices', !/exclud/i.test(await p.locator('#stick').evaluate((el) => { const c = el.cloneNode(true); c.querySelector('#ex-btn')?.remove(); return c.textContent; })));
    await p.evaluate(() => Fatstick.app.refreshSticks(true));
    check('looking again leaves it out', (await devs()) === n0 - 1);
    await p.locator('#ex-btn').click();
    await p.locator('.ex-dev').first().waitFor();
    check('and it stays ticked', await box().isChecked());
    // Space plays and pauses over the dialog, even on a box; Enter ticks it.
    const playing0 = await p.evaluate(() => Fatstick.app.S.player.playing);
    await box().focus();
    await p.keyboard.press('Space');
    await p.waitForTimeout(80);
    check('Space on a box plays (or pauses), and leaves the box', (await p.evaluate(() => Fatstick.app.S.player.playing)) !== playing0 && await box().isChecked());
    await p.keyboard.press('Space');
    await box().focus();
    await p.keyboard.press('Enter');
    await p.locator('.ex-status', { hasText: 'shown again' }).waitFor();
    check('Enter unticks it, and it\'s back', !(await box().isChecked()) && (await devs()) === n0);
    await p.getByRole('button', { name: 'Done' }).click();
  }

  // 32b. The player bar works under every dialog, undimmed, and Space plays
  // and pauses with any dialog open (on a button too), but types in a text box.
  {
    await p.locator('#ex-btn').click();
    const bar = await p.evaluate(() => {
      const b = document.querySelector('#player .pbtn.play');
      const r = b.getBoundingClientRect();
      const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      const m = document.getElementById('modal').getBoundingClientRect();
      return { onTop: !!(top && top.closest('#player')), above: m.bottom <= document.getElementById('player').getBoundingClientRect().top, opacity: getComputedStyle(document.getElementById('player')).opacity };
    });
    check('a dialog leaves the player bar on top, undimmed, with the dialog above it', bar.onTop && bar.above && bar.opacity === '1', JSON.stringify(bar));
    const was = await p.evaluate(() => Fatstick.app.S.player.playing);
    await p.locator('#player .pbtn.play').click();
    check('its play button works with the dialog open', (await p.evaluate(() => Fatstick.app.S.player.playing)) !== was && !(await p.locator('#modal-back').isHidden()));
    await p.getByRole('button', { name: 'Done' }).focus();
    await p.keyboard.press('Space');
    await p.waitForTimeout(80);
    check('Space on a dialog\'s button plays or pauses, without pressing the button', (await p.evaluate(() => Fatstick.app.S.player.playing)) === was && !(await p.locator('#modal-back').isHidden()));
    await p.keyboard.press('Escape');
    // A text box: Space types.
    const before = await p.evaluate(() => Fatstick.app.S.player.playing);
    await p.locator('#srch-btn').click();
    await p.locator('#srch-in').click();
    await p.keyboard.type('road trip');
    check('Space in a text box types a space and leaves the player be', (await p.locator('#srch-in').inputValue()) === 'road trip' && (await p.evaluate(() => Fatstick.app.S.player.playing)) === before);
    await p.keyboard.press('Escape');
    await p.keyboard.press('Escape');
    await p.evaluate(() => Fatstick.app.stopPlayback());
  }

  // 32b. A setlist plays from beginning to end: every file in the stick's order,
  // silence and joined tracks included, each with its changes.
  {
    await p.evaluate(() => { const A = Fatstick.app; A.stopPlayback(); A.setProcessing(null); });
    const A = () => Fatstick.app;
    // A setlist of its own: a playlist with a cut, padded and faded track, silence, and a joined one.
    const made = await p.evaluate(() => {
      const A = Fatstick.app;
      const S = A.S;
      const [a, b] = [S.playlists[0], S.playlists[1]];
      a.entries.slice(0, 2).forEach((e) => { delete e.cutStart; delete e.cutEnd; delete e.padStart; delete e.padEnd; delete e.pitch; delete e.fadeIn; delete e.fadeOut; });
      Object.assign(a.entries[1], { cutStart: 2, padEnd: 1.5, pitch: 3, keepFormants: true, fadeIn: 4 });
      b.join = true;
      const set = S.sets.find((x) => x.id === 'set-car');
      set.gaps = [{ id: 'gap-test', seconds: 7 }];
      set.playlistIds = [a.id, 'gap-test', b.id];
      return { a: a.id, b: b.id, aFiles: A.planSet(set).folders[0].files.length, parts: b.entries.length };
    });
    await p.locator('[data-set="set-car"]').click();
    await p.getByRole('button', { name: 'Expand all' }).click();
    const files = () => p.evaluate(() => Fatstick.app.setFilesOf(Fatstick.app.stickSet('set-car')).map((x) => x.it.entry.id));
    const order = await files();
    check('a setlist plays what the stick gets: playlist, silence, joined', order.length === made.aFiles + 2 && order[made.aFiles] === 'gap-test:silence' && order[made.aFiles + 1] === `${made.b}:joined`, order.slice(-3).join(' | '));
    // What's sent for a file is what a sync renders it from.
    const sent = await p.evaluate(() => {
      const api = Fatstick.api;
      window.__played = [];
      const real = api.playParts;
      api.playParts = (a) => { window.__played.push(a); return real.call(api, a); };
      return true;
    });
    const btn = p.locator('#sv-play');
    check('its view has a Play button', (await btn.textContent()).trim() === 'Play' && sent);
    await btn.click();
    const pl = () => p.evaluate(() => { const P = Fatstick.app.S.player; return { set: P.set && P.set.id, file: P.set && P.set.file, index: P.set && P.set.index, playing: P.playing, entry: P.entryId, pos: P.pos, dur: P.dur }; });
    let now = await pl();
    check('Play starts it at its first file', now.set === 'set-car' && now.index === 0 && now.file === order[0] && now.playing);
    check('the button can pause it', (await btn.textContent()).trim() === 'Pause');
    check('the row playing is marked', (await p.locator(`#stickview .lay-file.sv-now[data-file="${order[0]}"]`).count()) === 1 && (await p.locator('#stickview .lay-file.sv-now').count()) === 1);
    check('the player bar says where it is', new RegExp(`Car mix, 1 of ${order.length}\\.`).test(await p.locator('#player .np-s').textContent()));
    check('so does the display', /^PLAY 01-/.test((await p.locator('#disp-text').textContent()).trim()));
    await p.screenshot({ path: `${out}/setlist-playing.png` });
    await p.locator('#sv-play').click();
    check('pause', !(await pl()).playing && (await btn.textContent()).trim() === 'Go on');
    await p.locator('#sv-play').click();
    check('and go on, in the same file', (await pl()).playing && (await pl()).file === order[0]);
    // Next: the second file has its changes.
    await p.locator('#player button[aria-label="Next"]').click();
    now = await pl();
    check('next goes to the next file', now.file === order[1] && now.index === 1);
    const nowSent = () => p.evaluate(() => { const P = Fatstick.app.S.player; return P.sent.get(P.token).args; });
    const second = await nowSent();
    const part2 = second.parts[0];
    check('with its cut, silence, key, fade and volume', second.parts.length === 1 && part2.cutStart === 2 && part2.padEnd === 1.5 && part2.pitch === 3 && part2.fadeIn === 4 && typeof part2.gainDb === 'number', JSON.stringify(part2));
    const expected = await p.evaluate(() => { const A = Fatstick.app; const it = A.setFilesOf(A.stickSet('set-car'))[1].it; return { secs: it.secs, recipe: it.recipe }; });
    check('its length is as the stick will have it', Math.abs(now.dur - expected.secs) < 0.01, `${now.dur} ${expected.secs}`);
    check('and the same parts a sync renders it from', JSON.stringify(expected.recipe.parts[0]) === JSON.stringify(part2), JSON.stringify(expected.recipe.parts[0]));
    await p.locator('#player button[aria-label="Previous"]').click();
    check('previous goes back a file', (await pl()).file === order[0]);
    await p.locator('#player button[aria-label="Next"]').click();
    // Ends by itself, in order: seek near the end of each file up to the silence.
    const runOut = async () => { await p.evaluate(() => { const A = Fatstick.app; A.seekTo(Math.max(0, A.S.player.dur - 0.3)); }); await p.waitForTimeout(700); };
    await runOut();
    check('at the end of a file it goes on to the next by itself', (await pl()).index === 2);
    // Jump to the last file of the first playlist by double-click, then run on.
    const lastOfFirst = order[made.aFiles - 1];
    await p.locator(`#stickview [data-file="${lastOfFirst}"]`).first().scrollIntoViewIfNeeded();
    await p.locator(`#stickview [data-file="${lastOfFirst}"] .lay-name`).first().dblclick();
    await p.waitForTimeout(100);
    check('double-clicking a file starts there', (await pl()).file === lastOfFirst);
    await runOut();
    now = await pl();
    check('the silence plays as a file of its own', now.file === 'gap-test:silence' && Math.abs(now.dur - 7) < 0.01 && now.entry === null && now.playing);
    const gapSent = await nowSent();
    check('with nothing but silence in it', gapSent.parts.length === 1 && gapSent.parts[0].trackId === null && gapSent.parts[0].padStart === 7, JSON.stringify(gapSent.parts));
    check('and says so', /^SILENCE /.test((await p.locator('#disp-text').textContent()).trim()) && (await p.locator('#player .np-t').textContent()) === 'Silence' && (await p.locator('#stickview .lay-line.sv-now').count()) === 1);
    await p.waitForTimeout(600);
    const moved = (await pl()).pos;
    check('its position moves', moved >= 0.25, String(moved));
    await runOut();
    now = await pl();
    check('then the joined playlist, as one file', now.file === `${made.b}:joined` && now.index === order.length - 1);
    const joinSent = await nowSent();
    check('made of all its tracks, in order', joinSent.parts.length === made.parts && joinSent.parts.every((x) => x.trackId), String(joinSent.parts.length));
    const join = await p.evaluate(() => { const A = Fatstick.app; const it = A.S.player.set.item; return { secs: it.secs, at: it.parts.map((x) => x.at), ids: it.parts.map((x) => x.entry.id) }; });
    check('its length is all of theirs', Math.abs((await pl()).dur - join.secs) < 0.01);
    check('the player names the track in it, and its playlist', /as one track/.test(await p.locator('#player .np-s').textContent()) && (await pl()).entry === join.ids[0]);
    await p.evaluate((sec) => Fatstick.app.seekTo(sec), join.at[2] + 1);
    await p.waitForTimeout(100);
    check('the position says which track it is', (await pl()).entry === join.ids[2] && (await p.locator(`#stickview .lay-parts li.sv-now[data-part="${join.ids[2]}"]`).count()) === 1);
    // Double-clicking a track in the joined file starts the file at that track.
    await p.locator(`#stickview [data-part="${join.ids[1]}"] .nm`).dblclick();
    await p.waitForTimeout(100);
    now = await pl();
    check('double-clicking a track in it starts there', now.file === `${made.b}:joined` && now.entry === join.ids[1] && Math.abs(now.pos - join.at[1]) < 0.6, JSON.stringify(now));
    // The last file ends the setlist, unless it repeats.
    await runOut();
    await p.waitForTimeout(300);
    check('after the last file it stops', (await pl()).set === null && !(await pl()).playing && (await btn.textContent()).trim() === 'Play');
    await p.keyboard.press('Space');
    await p.waitForTimeout(100);
    check('Space in its view starts it', (await pl()).set === 'set-car' && (await pl()).index === 0);
    await p.evaluate((i) => Fatstick.app.playSetlist('set-car', i), order.length - 1);
    await p.evaluate(() => { Fatstick.app.S.player.repeat = 'all'; });
    await runOut();
    await p.waitForTimeout(300);
    check('repeating the setlist goes back to its first file', (await pl()).index === 0 && (await pl()).set === 'set-car');
    await p.evaluate(() => { Fatstick.app.S.player.repeat = 'one'; });
    await p.locator('#player button[aria-label="Next"]').click();
    const oneFile = (await pl()).file;
    await runOut();
    check('repeating one plays the same file again', (await pl()).file === oneFile);
    await p.evaluate(() => { Fatstick.app.S.player.repeat = 'off'; });
    // An edit counts from the next file; one taken out doesn't lose the place.
    await p.evaluate((i) => Fatstick.app.playSetlist('set-car', i), made.aFiles);
    await p.evaluate(() => { const s = Fatstick.app.stickSet('set-car'); s.playlistIds = s.playlistIds.filter((x) => x !== 'gap-test'); });
    await runOut();
    await p.waitForTimeout(200);
    check('a file taken out while it plays: on to what followed it', (await pl()).file === `${made.b}:joined`);
    await p.evaluate(() => { const s = Fatstick.app.stickSet('set-car'); s.playlistIds = [s.playlistIds[0], 'gap-test', s.playlistIds[1]]; });
    // Elsewhere: the row in a playlist is marked, and playing something else takes over.
    await p.evaluate(() => Fatstick.app.playSetlist('set-car', 0));
    await p.locator(`[data-pl="${made.a}"]`).click();
    check('its track is marked in its playlist too', (await p.locator('#list .row.playing').count()) === 1);
    check('the bar goes on saying it\'s the setlist', /Car mix, 1 of/.test(await p.locator('#player .np-s').textContent()));
    await row(3).dblclick();
    await p.waitForTimeout(100);
    check('playing a track in a playlist takes over', (await pl()).set === null && (await pl()).playing);
    await p.evaluate(() => Fatstick.app.stopPlayback());
    // The sidebar's menu plays one too.
    await p.locator('[data-set="set-car"]').click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Play', exact: true }).click();
    check('a setlist\'s right-click menu plays it', (await pl()).set === 'set-car');
    await p.evaluate(() => Fatstick.app.stopPlayback());
    // A profile with one playlist per stick plays just that.
    const single = await p.evaluate(() => {
      const A = Fatstick.app;
      const s = A.stickSet('set-car');
      const pr = A.S.profiles.find((x) => x.id === s.profileId);
      const was = pr.layout;
      pr.layout = 'single';
      const f = A.setFilesOf(s).map((x) => x.it.entry.id);
      pr.layout = was;
      return f;
    });
    check('a profile with one playlist per stick plays that one, no silence', single.length === made.aFiles && !single.includes('gap-test:silence'));
    // A dialog's preview takes over from the setlist, and live changes never reach its stream.
    await p.evaluate(() => Fatstick.app.playSetlist('set-car', 0));
    const shapeSent = await p.evaluate(() => { window.__shape = 0; const api = Fatstick.api; const real = api.playerShape; api.playerShape = (...a) => { window.__shape++; return real.apply(api, a); }; return true; });
    await p.locator(`[data-pl="${made.a}"]`).click();
    await row(0).click();
    await p.keyboard.press('f');
    await p.locator('#modal input[type="range"]').first().fill('5');
    await p.waitForTimeout(100);
    check('a dialog\'s slider leaves a setlist playing alone', (await p.evaluate(() => window.__shape)) === 0 && shapeSent && (await pl()).set === 'set-car');
    await p.keyboard.press('Escape');
    await p.evaluate(() => { const A = Fatstick.app; A.stopPlayback(); delete Fatstick.api.playParts; });
    await p.reload();
    await p.waitForTimeout(600);
    await p.evaluate(() => Fatstick.app.setProcessing('fat'));
    await p.waitForTimeout(260);
  }

  // 32c. Gapless: what follows is queued as each one starts, and moving on to it
  // doesn't start or stop anything.
  {
    await p.evaluate(() => {
      const api = Fatstick.api;
      window.__calls = [];
      for (const k of ['play', 'playParts', 'queue', 'stop']) {
        const real = api[k];
        api[k] = (a) => { window.__calls.push({ k, token: a && a.token, args: a }); return real.call(api, a); };
      }
    });
    const calls = () => p.evaluate(() => window.__calls.map((c) => `${c.k}${c.token ? '' : ':none'}`));
    const clear = () => p.evaluate(() => { window.__calls.length = 0; });
    const cur = () => p.evaluate(() => { const P = Fatstick.app.S.player; return { entry: P.entryId, token: P.token, playing: P.playing, pos: P.pos, dur: P.dur, from: P.from, live: P.live, set: P.set && P.set.index }; });
    const ids = await p.evaluate(() => Fatstick.app.pl().entries.filter((e) => Fatstick.app.track(e.trackId).exists).map((e) => e.id));
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[0]);
    await p.waitForTimeout(50);
    check('a track starts in the stream, with the next one queued', JSON.stringify(await calls()) === JSON.stringify(['playParts', 'queue']));
    const q = await p.evaluate(() => window.__calls[1].args.parts[0].trackId);
    check('what\'s queued is the next in the playlist', q === await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).trackId, ids[1]));
    await clear();
    await p.evaluate(() => { const A = Fatstick.app; A.seekTo(A.S.player.dur - 0.3); });
    await p.waitForTimeout(600);
    let now = await cur();
    check('at its end the next is playing', now.entry === ids[1] && now.playing && now.pos < 1);
    check('with no play or stop sent, only the one after it queued', JSON.stringify(await calls()) === JSON.stringify(['queue']), JSON.stringify(await calls()));
    check('the row and the bar follow', (await p.locator(`#row-${ids[1]}.playing`).count()) === 1 && (await p.locator('#row-' + ids[0] + '.playing').count()) === 0);
    // The repeat button changes what's queued.
    await clear();
    await p.evaluate(() => { document.getElementById('repeat').click(); });
    const queuedNow = () => p.evaluate(() => { const a = window.__calls.filter((c) => c.k === 'queue').at(-1); return a && a.args ? a.args.parts[0].trackId : null; });
    await p.waitForTimeout(50);
    const thisTrack = await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).trackId, ids[1]);
    check('repeat one queues the same track, for no gap as it comes round', (await queuedNow()) === thisTrack, await p.evaluate(() => Fatstick.app.S.player.repeat));
    await p.evaluate(() => { document.getElementById('repeat').click(); });
    await p.evaluate(() => { document.getElementById('repeat').click(); });
    await p.waitForTimeout(50);
    check('and play & stop goes back to the next one', (await p.evaluate(() => Fatstick.app.S.player.repeat)) === 'off');
    // The last track has nothing after it, unless the playlist repeats.
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[ids.length - 1]);
    await p.waitForTimeout(50);
    check('the last track queues nothing', (await queuedNow()) === null);
    await p.evaluate(() => { document.getElementById('repeat').click(); document.getElementById('repeat').click(); });
    await p.waitForTimeout(50);
    check('repeating the playlist queues its first', (await queuedNow()) === await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).trackId, ids[0]));
    await p.evaluate(() => { document.getElementById('repeat').click(); });
    // An edit changes what follows.
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[0]);
    await p.waitForTimeout(50);
    await clear();
    await row(1).click();
    await p.keyboard.press('Delete');
    await p.waitForTimeout(250);
    const third = await p.evaluate((id) => Fatstick.app.pl().entries.find((e) => e.id === id).trackId, ids[2]);
    check('taking the next track out queues the one after', (await queuedNow()) === third);
    await p.keyboard.press('Control+z');
    // The position is the track as the stick has it, cuts and silences included.
    await p.evaluate(() => { const e = Fatstick.app.pl().entries[0]; e.cutStart = 2; e.padEnd = 1.5; });
    await p.evaluate(() => Fatstick.app.playEntry(Fatstick.app.pl().entries[0].id));
    const cutNow = await p.evaluate(() => { const A = Fatstick.app; const e = A.pl().entries[0]; return { dur: A.S.player.dur, want: A.track(e.trackId).duration - 2 + 1.5, pos: A.S.player.pos, part: A.S.player.sent.get(A.S.player.token).args.parts[0] }; });
    check('a track\'s length is as on the stick, from 0', Math.abs(cutNow.dur - cutNow.want) < 0.01 && cutNow.pos < 0.5, JSON.stringify(cutNow));
    check('with its cut and silence in what it plays', cutNow.part.cutStart === 2 && cutNow.part.padEnd === 1.5);
    await p.evaluate(() => { const e = Fatstick.app.pl().entries[0]; delete e.cutStart; delete e.padEnd; });
    // A dialog's Hear plays the source, live, and queues nothing; afterwards it goes back.
    await clear();
    await row(0).click();
    await p.keyboard.press('f');
    await p.waitForTimeout(50);
    await p.getByRole('button', { name: /^Hear the start/ }).click();
    await p.waitForTimeout(50);
    now = await cur();
    check('a dialog hears the track live, on its own', now.live && (await calls()).at(-1) === 'play', JSON.stringify(await calls()));
    await p.keyboard.press('Escape');
    await p.evaluate(() => Fatstick.app.stopPlayback());
    // "Hear the prepared files", in the transfer panel, asks for the prepared file.
    await p.locator('#hear-prepared').check();
    await clear();
    await p.evaluate((id) => Fatstick.app.playEntry(id), ids[0]);
    await p.waitForTimeout(50);
    const heardPrepared = await p.evaluate(() => window.__calls.find((c) => c.k === 'playParts').args);
    check('"Hear the prepared files" asks for the prepared file', !!heardPrepared.recipe && typeof heardPrepared.parts[0].gainDb === 'number', JSON.stringify(heardPrepared.recipe));
    await p.locator('#hear-prepared').uncheck();
    await p.evaluate(() => Fatstick.app.stopPlayback());
    await p.reload();
    await p.waitForTimeout(600);
    await p.evaluate(() => Fatstick.app.setProcessing('fat'));
    await p.waitForTimeout(260);
  }

  // 32d. "Enable Play/Setlist Processing": on, what's been set is heard; off, the files.
  {
    const cb = p.locator('#proc-on');
    const sentNow = () => p.evaluate(() => { const P = Fatstick.app.S.player; return { args: P.sent.get(P.token).args, dur: P.dur, pos: P.pos, entry: P.entryId, set: P.set && P.set.file, count: P.set && P.set.count, processing: P.processing }; });
    check('the player bar has the switch, on', /Enable Play\/Setlist Processing/.test(await p.locator('#player .aos').textContent()) && await cb.isChecked());
    check('and "As on the stick" is gone', !/As on the stick/.test(await p.locator('#player').textContent()));
    // A playlist in no setlist, with even volume on, a cut, a key and a fade.
    const made = await p.evaluate(() => {
      const A = Fatstick.app;
      const S = A.S;
      const q = S.playlists[S.playlists.length - 1];
      for (const s of S.sets) s.playlistIds = s.playlistIds.filter((x) => x !== q.id);
      q.norm.on = true;
      const e = q.entries.find((x) => A.track(x.trackId).exists && A.track(x.trackId).lufs != null) || q.entries[0];
      Object.assign(e, { cutStart: 3, padEnd: 2, pitch: 2, keepFormants: true, fadeOut: 5 });
      return { id: q.id, entry: e.id, dur: A.track(e.trackId).duration, lufs: A.track(e.trackId).lufs };
    });
    await p.locator(`[data-pl="${made.id}"]`).click();
    await p.evaluate(({ entry, id }) => Fatstick.app.playEntry(entry, id), made);
    let now = await sentNow();
    const part = now.args.parts[0];
    check('on: a track plays with its cut, silence, key and fade', part.cutStart === 3 && part.padEnd === 2 && part.pitch === 2 && part.fadeOut === 5 && Math.abs(now.dur - (made.dur - 3 + 2)) < 0.01, JSON.stringify(part));
    if (made.lufs != null) check('with its even volume, in a playlist that\'s in no setlist', part.gainDb !== 0 || made.lufs === -18, JSON.stringify(part));
    check('and asks for no prepared file', !now.args.recipe);
    // Off, while it plays: the file as it is, from the same place in the track.
    await p.evaluate(() => Fatstick.api.seek(10));
    await p.waitForTimeout(40);
    await p.evaluate(() => { Fatstick.app.S.player.pos = 10; });
    await cb.uncheck();
    await p.waitForTimeout(40);
    now = await sentNow();
    check('off: the file as it is, nothing else', JSON.stringify(Object.keys(now.args.parts[0])) === JSON.stringify(['trackId']) && Math.abs(now.dur - made.dur) < 0.01, JSON.stringify(now.args.parts[0]));
    check('going on from the same place in the track', Math.abs(now.pos - 13) < 0.5, String(now.pos));
    check('the bar says so', /The files as they are/.test(await p.locator('#player .aos small').textContent()));
    check('the transfer\'s "Hear the prepared files" waits for it', await p.evaluate(() => { const b = document.getElementById('hear-prepared'); return !b || b.disabled; }));
    await p.evaluate(() => Fatstick.app.stopPlayback());
    // A setlist, off: its tracks in order, no silence, a joined playlist as plain tracks.
    const setup = await p.evaluate(() => {
      const A = Fatstick.app;
      const S = A.S;
      const [a, b] = [S.playlists[0], S.playlists[1]];
      b.join = true;
      const set = S.sets.find((x) => x.id === 'set-car');
      set.gaps = [{ id: 'gap-off', seconds: 9 }];
      set.playlistIds = [a.id, 'gap-off', b.id];
      return { b: b.id, n: b.entries.filter((e) => A.track(e.trackId).exists).length };
    });
    await p.locator('[data-set="set-car"]').click();
    await p.getByRole('button', { name: 'Expand all' }).click();
    const offFiles = await p.evaluate(() => Fatstick.app.setFilesOf(Fatstick.app.stickSet('set-car')).map((x) => x.it.entry.id));
    check('off: a setlist has no silence to play', !offFiles.includes('gap-off:silence') && offFiles.at(-1) === `${setup.b}:joined`);
    await p.evaluate((i) => Fatstick.app.playSetlist('set-car', i), offFiles.length - 1);
    now = await sentNow();
    check('off: a joined playlist is its tracks as they are', now.args.parts.length === setup.n && now.args.parts.every((x) => JSON.stringify(Object.keys(x)) === '["trackId"]'), String(now.args.parts.length));
    await p.locator('#stickview [data-file="gap-off:silence"] .lay-fname').dblclick();
    await p.waitForTimeout(40);
    check('double-clicking a silence plays from what follows it', (await sentNow()).set === `${setup.b}:joined`);
    await cb.check();
    await p.waitForTimeout(40);
    now = await sentNow();
    check('on again: its tracks with their changes, and the silence back', now.processing && (await p.evaluate(() => Fatstick.app.setFilesOf(Fatstick.app.stickSet('set-car')).some((x) => x.it.gap))));
    await p.evaluate(() => Fatstick.app.stopPlayback());
    await p.reload();
    await p.waitForTimeout(600);
  }

  // 32e. What's only about a stick shows while the transfer is open.
  {
    await p.waitForTimeout(100);
    const car = p.locator('[data-set="set-car"]');
    const road = p.locator('[data-pl="p-road"]');
    const open = async () => { await car.click(); await p.locator('#proc-fat').click(); await p.waitForTimeout(260); };
    const close = async () => { await p.locator('#proc-close').click(); await p.locator('#stick').waitFor({ state: 'hidden' }); };
    check('transfer closed: a playlist has no "On the stick" column', !/On the stick/.test(await p.locator('#list-cols').textContent()) && (await p.locator('#list .c-stick').count()) === 0);
    await p.getByRole('button', { name: 'Columns' }).click();
    check('and doesn\'t offer it', !/on the stick/i.test(await p.locator('#menu').textContent()));
    await p.keyboard.press('Escape');
    check('a missing file can still be located', (await p.locator('#list .c-title .locate').count()) >= 1);
    check('on the line that says it is missing, inside its row', await p.locator('#list .row.missing').first().evaluate((r) => {
      const l = r.querySelector('.locate').getBoundingClientRect(), s = r.querySelector('.sub').getBoundingClientRect(), o = r.getBoundingClientRect();
      // Two lines are a little taller than a row, whatever else is in it.
      return Math.abs(l.top + l.height / 2 - (s.top + s.height / 2)) < 2 && l.bottom <= s.bottom + 1;
    }));
    check('the display says nothing of syncing', (await p.locator('#disp-sub').textContent()) === '');
    check('a setlist on the left says its playlists and length, no profile', /^2 playlists, \d/.test(await p.locator('[data-set="set-car"] .mt').textContent()), await p.locator('[data-set="set-car"] .mt').textContent());
    check('and nothing about not being synced', (await p.locator('[data-set="set-gym"] .flag').count()) === 0);
    check('a stick plugged in is still marked', /plugged in/.test(await p.locator('[data-set="set-car"] .flag').textContent()));
    await p.locator('#app-menu').click();
    check('the menu has nothing about devices', !/Exclude/.test(await p.locator('#menu').textContent()));
    await p.keyboard.press('Escape');
    await car.click();
    await p.waitForTimeout(50);
    check('a setlist\'s view lists playlists and tracks', (await p.locator('#stickview .lay-folder .lay-fname').first().textContent()) === 'Road Trip');
    await p.getByRole('button', { name: 'Expand all' }).click();
    const firstRow = await p.locator('#stickview .lay-file .nm').first().textContent();
    check('by title, not file name', !/\.mp3$/.test(firstRow), firstRow);
    check('with no file sizes or how they\'re made', (await p.locator('#stickview .lay-how, #stickview .lay-size').count()) === 0);
    check('no sync status, profile or stick', (await p.locator('#stickview .pill, #stickview .lay-sub, #stickview .lay-status').count()) === 0);
    check('its summary counts playlists and tracks', /^2 playlists, \d+ tracks, /.test(await p.locator('#stickview .lay-sum').textContent()), await p.locator('#stickview .lay-sum').textContent());
    check('the display counts tracks', /TRK/.test(await p.locator('#disp-unit').textContent()));
    await p.locator('#proc-fat').click();
    await p.waitForTimeout(260);
    check('transfer open: its folders', /^01 Road Trip\//.test(await p.locator('#stickview .lay-folder .lay-fname').first().textContent()));
    check('files, what they are and how big', /\.mp3$/.test(await p.locator('#stickview .lay-file .nm').first().textContent()) && (await p.locator('#stickview .lay-how').count()) > 0 && (await p.locator('#stickview .lay-size').count()) > 0);
    check('how it stands, with what profile and stick', (await p.locator('#stickview .pill').count()) === 1 && /Generic profile/.test(await p.locator('#stickview .lay-sub').textContent()));
    check('the display counts folders and says how it stands', /DIR/.test(await p.locator('#disp-unit').textContent()) && /out of date|up to date|synced/.test(await p.locator('#disp-sub').textContent()));
    await road.click();
    // Its columns are fitted once the list has a width, a frame after it's shown.
    await p.locator('#list-cols .h-stick').waitFor({ timeout: 3000 }).catch(() => {});
    check('a playlist has its "On the stick" column', /On the stick/.test(await p.locator('#list-cols').textContent()));
    check('a setlist never synced says so', /not synced/.test(await p.locator('[data-set="set-gym"] .flag').textContent()));
    await p.getByRole('button', { name: 'Columns' }).click();
    check('and it\'s in the Columns menu', /On the stick/.test(await p.locator('#menu').textContent()));
    await p.keyboard.press('Escape');
    await p.locator('#ex-btn').click();
    check('devices are excluded from here', /^Exclude devices$/.test(await p.locator('#modal-title').textContent()));
    await p.keyboard.press('Escape');
    // Drawn again as the transfer closes, a scrolled setlist stays where it was.
    await car.click();
    const kept = await p.evaluate(() => { const x = document.getElementById('sv-scroll'); x.scrollTop = 200; return x.scrollTop; });
    await p.locator('#proc-fat').focus();
    await p.keyboard.press('Enter');
    await p.waitForTimeout(260);
    check('it stays scrolled where it was, the focus on the button', Math.abs((await p.evaluate(() => document.getElementById('sv-scroll').scrollTop)) - kept) < 2 && kept > 0 && await p.evaluate(() => document.activeElement.id === 'proc-fat'));
    // Nothing is encoded for a setlist that isn't going on a stick.
    const jobs = await p.evaluate(async () => {
      const A = Fatstick.app;
      const api = Fatstick.api;
      window.__prep = [];
      const real = api.prepareQueue;
      api.prepareQueue = (j) => { window.__prep.push(j.length); return real.call(api, j); };
      for (const s of A.S.sets) s.stickId = null;
      A.S.syncs = {};
      A.S.cache.clear();
      A.scheduleWork();
      await new Promise((r) => setTimeout(r, 400));
      return window.__prep.slice();
    });
    check('no encoding in the background for a setlist with no stick', jobs.length === 0 || jobs.at(-1) === 0, JSON.stringify(jobs));
    await open();
    await p.waitForTimeout(400);
    const after = await p.evaluate(() => window.__prep.slice());
    check('with the transfer open on it, its files are prepared', after.at(-1) > 0, JSON.stringify(after));
    await p.reload();
    await p.waitForTimeout(600);
    await p.evaluate(() => Fatstick.app.setProcessing('fat'));
    await p.waitForTimeout(260);
  }

  // 33. "Show in the library" from a playlist's track: the library, with it selected.
  // (The panel closed: the library's table has the room for all its columns.)
  await p.evaluate(() => Fatstick.app.setProcessing(null));
  {
    await p.locator('[data-pl="p-road"]').click();
    const i = await p.evaluate(() => { const A = Fatstick.app; return A.pl().entries.findIndex((e) => A.lib.inMusic(A.track(e.trackId)) && A.track(e.trackId).exists); });
    const want = await p.evaluate((i) => { const A = Fatstick.app; return A.track(A.pl().entries[i].trackId).id; }, i);
    await row(i).click({ button: 'right' });
    await p.getByRole('menuitem', { name: 'Show in the library' }).click();
    await p.waitForTimeout(150);
    const sel = await p.evaluate(() => [...Fatstick.app.lib.state.L.sel]);
    check('"Show in the library" opens the library with that track selected', JSON.stringify(sel) === JSON.stringify([want]) && (await p.locator('.lr.sel').count()) === 1, JSON.stringify(sel));
  }

  // 34. Tracks whose files have gone can be taken out of the library.
  {
    const gone = await p.evaluate(() => { const A = Fatstick.app; return [...A.S.tracks.values()].filter((t) => A.lib.inMusic(t) && !t.exists && !t.forgotten).map((t) => t.id); });
    const before = await p.evaluate(() => Fatstick.app.lib.state.L.ids.length);
    await p.locator('.fct', { hasText: 'File not found' }).click();
    await p.waitForTimeout(100);
    check('the gone files are listed by "File not found"', (await p.locator('.lr').count()) === gone.length && gone.length > 0, `${await p.locator('.lr').count()} of ${gone.length}`);
    await p.locator('.lr').first().click({ button: 'right' });
    check('the menu offers to take a gone file out', (await p.getByRole('menuitem', { name: 'Remove from the library' }).count()) === 1);
    await p.keyboard.press('Escape');
    check('so does a button for all of them', /Remove all \d+ from the library/.test(await p.locator('#lib-forget-all').textContent()));
    await p.locator('#lib-forget-all').click();
    await p.waitForTimeout(200);
    const left = await p.evaluate(() => { const A = Fatstick.app; return [...A.S.tracks.values()].filter((t) => A.lib.inMusic(t) && !t.exists && !t.forgotten).length; });
    check('then they\'re gone from the library', left === 0 && (await p.locator('.lr').count()) === 0 && /out of the library/.test(await p.locator('#toast').textContent()), await p.locator('#toast').textContent());
    await p.locator('.lib-chips .tag', { hasText: 'File not found' }).locator('.x').click().catch(() => {});
  }

  // 35. A track's labels: "+2" in the library list, hovered, shows them all; search shows them too.
  {
    await p.evaluate(() => Fatstick.app.setView('library'));
    await p.waitForTimeout(100);
    const more = p.locator('.lr .tag.more').first();
    check('a track with more labels than fit shows "+N"', (await p.locator('.lr .tag.more').count()) > 0);
    const tid = await more.getAttribute('data-tid');
    await more.hover();
    await p.waitForTimeout(100);
    const tipN = await p.locator('#tip .tag').count();
    const has = await p.evaluate((id) => Fatstick.app.track(id).labels.length, tid);
    check('hovering it shows all of the track\'s labels', await p.locator('#tip').isVisible() && tipN === has, `${tipN} of ${has}`);
    await p.mouse.move(5, 5);
    await p.waitForTimeout(50);
    check('and the tip goes when the pointer does', await p.locator('#tip').isHidden());
    await p.evaluate(() => Fatstick.app.lib.openSearch());
    const name = await p.evaluate((id) => Fatstick.app.track(id).title, tid);
    await p.locator('#srch-in').fill(name);
    await p.waitForTimeout(200);
    const first = p.locator('.sr').first();
    const shown = await first.locator('.tag.lbl:not(.more)').count();
    const plus = await first.locator('.tag.more').count();
    check('search results show the track\'s labels, "+N" for the rest', shown >= 1 && shown + (plus ? Number((await first.locator('.tag.more').textContent()).slice(1)) : 0) === has, `${shown} shown, ${plus} more of ${has}`);
    check('tracks have no file path tooltips (only labels have a tip)', (await p.locator('.sr[title]').count()) === 0);
    await p.evaluate(() => Fatstick.app.lib.closeSearch());
    await p.locator('[data-pl="p-road"]').click();
    check('nor in a playlist or the library', (await p.locator('.row[title]').count()) === 0 && await p.evaluate(() => { Fatstick.app.setView('library'); const n = document.querySelectorAll('.lr[title]').length; Fatstick.app.setView('playlist'); return n === 0; }));
  }

  // 36. YouTube: the other side of the search panel. All made up here; tests/app_e2e.py runs the real side.
  {
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.locator('[data-pl="p-road"]').click();
    const entries = () => p.evaluate(() => Fatstick.app.pl().entries.length);
    const inLib = () => p.evaluate(() => [...Fatstick.app.S.tracks.values()].filter((t) => t.path && t.path.includes('/YouTube/')).length);
    await p.keyboard.press('Control+Shift+F');
    check('Ctrl+Shift+F opens the search on YouTube', (await p.locator('#srch-tab-youtube').getAttribute('aria-selected')) === 'true' && await p.locator('#yt-in').isVisible());
    await p.locator('.yt-setup').waitFor();
    check('with nothing set up, it says what it needs and how big it is', /yt-dlp/.test(await p.locator('.yt-setup').textContent()) && /about 85 MB/.test(await p.locator('#yt-get').textContent()));
    check('and searching waits for that', await p.locator('#yt-in').isDisabled());
    await p.locator('#yt-get').click();
    await p.locator('.yt-prog').waitFor();
    await p.waitForFunction(() => /Getting yt-dlp: \d+ MB of \d+ MB/.test(document.querySelector('.yt-prog').textContent));
    check('getting them shows how far along it is', true);
    await p.locator('.yt-prog .btn').click();
    await p.locator('#yt-get').waitFor();
    check('it can be cancelled, and asked for again', await p.locator('#yt-in').isDisabled() && (await p.locator('.yt-err').count()) === 0);
    await p.locator('#yt-get').click();
    await p.locator('#yt-in:not([disabled])').waitFor({ timeout: 8000 });
    check('then searching is open', /Only the audio is ever fetched/.test(await p.locator('.srch-scope').textContent()) && /yt-dlp 2026\.10\.26, Deno 2\.9\.6/.test(await p.locator('.yt-foot').textContent()));

    // Searching: on Enter, not on every letter.
    const yin = p.locator('#yt-in');
    await yin.fill('Harbour Lights');
    await p.waitForTimeout(500);
    check('typing alone doesn\'t search', (await p.locator('.yr').count()) === 0);
    await yin.press('Enter');
    check('Enter does, and says so', /Searching YouTube/.test(await p.locator('.srch-scope').textContent()));
    await p.locator('.yr').first().waitFor();
    check('the results come with channel, views and length', (await p.locator('.yr').count()) === 25 && /The Night Shift, 48M views/.test(await p.locator('.yr').nth(1).textContent()) && /3:51/.test(await p.locator('.yr').nth(1).textContent()));
    await p.locator('.yt-more').click();
    await p.waitForFunction(() => document.querySelectorAll('.yr').length === 40);
    check('"Show more" asks for more', /40 results/.test(await p.locator('.srch-scope').textContent()));
    await yin.fill('nothing at all');
    await yin.press('Enter');
    await p.waitForFunction(() => /Nothing on YouTube/.test(document.querySelector('.srch-scope').textContent));
    check('nothing found says so', (await p.locator('.yr').count()) === 0);
    await yin.fill('fail please');
    await yin.press('Enter');
    await p.locator('.yt-err').waitFor();
    check('a failed search says why', /Couldn't reach YouTube/.test(await p.locator('.yt-err').textContent()));
    await yin.fill('Harbour Lights');
    await yin.press('Enter');
    await p.waitForFunction(() => document.querySelectorAll('.yr').length === 25);

    // Hearing one: fetched, then played. It isn't in the library for that.
    const row = (i) => p.locator('.yr').nth(i);
    const libBefore = await inLib();
    await row(1).dblclick();
    await row(1).locator('.yr-pct').waitFor();
    check('a song on its way shows its progress', /%|…/.test(await row(1).locator('.yr-pct').textContent()));
    await p.waitForFunction(() => Fatstick.app.S.player.trackId && Fatstick.app.S.player.trackId.startsWith('yt:'), null, { timeout: 5000 });
    check('double-clicking fetches it and plays it', await row(1).locator('.eq').isVisible() && /Harbour Lights \(Official Video\)/.test(await p.locator('.np-t').textContent()) && /From YouTube/.test(await p.locator('.np-s').textContent()));
    check('the display says where it\'s from', /From YouTube/.test(await p.locator('#disp-sub').textContent()));
    check('and hearing it adds nothing to the library', (await inLib()) === libBefore && (await row(1).locator('.sr-in').count()) === 0);
    // A song started while another is on its way isn't cut in on.
    await row(2).dblclick();
    await p.waitForTimeout(40);
    await p.evaluate(() => { const A = Fatstick.app; A.playTrack(A.pl().entries[0].trackId); });
    const other = await p.evaluate(() => Fatstick.app.S.player.trackId);
    await p.waitForTimeout(1300);
    check('what was started in the meantime goes on playing', (await p.evaluate(() => Fatstick.app.S.player.trackId)) === other);
    await p.evaluate(() => Fatstick.app.stopPlayback());

    // Stopping one on its way.
    await row(4).locator('.yr-add').click();
    await row(4).locator('.yr-pct').waitFor();
    await row(4).locator('.yr-act button').click();
    await row(4).locator('.yr-add').waitFor({ timeout: 3000 });
    check('a song on its way can be stopped', (await row(4).locator('.sr-in').count()) === 0 && (await inLib()) === libBefore);

    // The + button: into the library.
    await row(3).locator('.yr-add').click();
    await row(3).locator('.sr-in').waitFor({ timeout: 5000 });
    check('the + button adds it to the library', (await inLib()) === libBefore + 1 && /In your library/.test(await p.locator('#toast').textContent()));
    const kept = await p.evaluate(() => [...Fatstick.app.S.tracks.values()].find((t) => t.path.includes('/YouTube/')));
    check('with a clean title and artist', kept.title === 'Harbour Lights' && kept.artist === 'Lyric Corner', `${kept.artist} / ${kept.title}`);
    check('and it\'s found by the music search', await p.evaluate((id) => Fatstick.app.lib.state.SE !== undefined && Fatstick.app.track(id).exists, kept.id));
    await p.locator('#toast .btn').click();
    await p.locator('.lr.sel').first().waitFor();
    check('"Show" opens the library on it', /Harbour Lights/.test(await p.locator('.lr.sel').first().textContent()));
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.keyboard.press('Control+Shift+F');
    await row(0).waitFor();
    await p.waitForTimeout(300); // the sidebar widens, and titles wrap differently

    // A drag into the list: at that place, once it has arrived.
    const n0 = await entries();
    const from = await row(5).boundingBox();
    const place = await p.locator('#list .row').nth(2).boundingBox();
    await p.mouse.move(from.x + 40, from.y + 12);
    await p.mouse.down();
    await p.mouse.move(from.x + 90, from.y + 20, { steps: 3 });
    await p.mouse.move(place.x + 200, place.y + 2, { steps: 6 });
    check('a song dragged out of the results says where it\'s going', /Add Harbour Lights - Piano Cover here/.test(await p.locator('#drag-badge').textContent()), await p.locator('#drag-badge').textContent());
    await p.mouse.up();
    await p.waitForFunction((n) => Fatstick.app.pl().entries.length === n + 1, n0, { timeout: 5000 });
    const placed = await p.evaluate(() => { const A = Fatstick.app; return A.pl().entries.findIndex((e) => A.track(e.trackId).path.includes('/YouTube/')); });
    check('it goes into the playlist there when it has arrived', placed === 2, `at ${placed}`);
    check('and the list\'s Undo takes it out', /Added 1 track/.test(await p.locator('#toast').textContent()));
    await p.locator('#toast .btn').click();
    check('(Undo)', (await entries()) === n0);

    // Keys, several at once, the menu.
    await row(6).click();
    await row(7).click({ modifiers: ['Shift'] });
    check('rows select like the music search\'s', (await p.locator('.yr.sel').count()) === 2);
    await p.locator('.yt-res').press('Shift+Enter');
    await p.waitForFunction((n) => Fatstick.app.pl().entries.length === n + 2, n0, { timeout: 6000 });
    check('Shift+Enter adds what\'s selected to the open playlist', true);
    await row(0).click({ button: 'right' });
    const items = await p.getByRole('menuitem').allTextContents();
    check('the right-click menu has the rest', ['Play', 'Add to Road Trip', 'New playlist from these', 'Add to the library', 'Copy the link'].every((x) => items.some((i) => i.includes(x))), items.join(' | '));
    await p.keyboard.press('Escape');
    // Added once: the same track from then on.
    const sameN = await inLib();
    await yin.fill('Harbour Lights');
    await yin.press('Enter');
    await p.waitForTimeout(600);
    check('a new search still marks what\'s in the library', (await row(3).locator('.sr-in').count()) === 1);
    await row(3).click();
    await p.locator('.yt-res').press('Shift+Enter');
    await p.waitForTimeout(250);
    check('adding it again doesn\'t make another file', (await inLib()) === sameN);

    // No music folder yet: it asks for one.
    await p.evaluate(() => { Fatstick.app.S.music = { ...Fatstick.app.S.music, root: null }; });
    await row(0).locator('.yr-add').click();
    await row(0).locator('.sr-in').waitFor({ timeout: 5000 });
    check('with no music folder, it asks for one first', await p.evaluate(() => Fatstick.app.S.music.root === '~/Music'));

    // The tabs, and Escape.
    await p.locator('#srch-tab-music').click();
    check('the Your music tab goes back to the music search', await p.locator('#srch-in').isVisible() && await p.locator('#yt-in').isHidden());
    await p.locator('#srch-in').fill('Harbour');
    await p.waitForTimeout(200);
    check('where a song added from YouTube is found', (await p.locator('.sr:not(.yr)').count()) > 0);
    await p.locator('#srch-in').fill('');
    await p.locator('#srch-tab-youtube').click();
    check('and YouTube\'s results are still there', (await p.locator('.yr').count()) === 25 && (await yin.inputValue()) === 'Harbour Lights');
    await p.keyboard.press('Control+f');
    check('Ctrl+F is always your music', await p.locator('#srch-in').isVisible());
    await p.locator('#srch-tab-youtube').click();
    await yin.press('Escape');
    check('Escape clears the box', (await yin.inputValue()) === '');
    await yin.press('Escape');
    check('and then closes the search', await p.evaluate(() => !Fatstick.app.S.searching));
    await p.locator('#srch-btn').click();
    check('the search button comes back to where you were', await yin.isVisible());
    if (process.argv[2]) {
      await yin.fill('Harbour Lights');
      await yin.press('Enter');
      await p.waitForFunction(() => /25 results/.test(document.querySelector('.srch-scope').textContent));
      await row(1).dblclick();
      await p.waitForFunction(() => Fatstick.app.S.player.trackId && Fatstick.app.S.player.playing, null, { timeout: 5000 });
      await row(2).locator('.yr-add').click();
      await p.waitForTimeout(350);
      await p.mouse.move(700, 450);
      await p.screenshot({ path: path.join(out, 'youtube.png') });
    }
    await p.evaluate(() => { Fatstick.app.stopPlayback(); Fatstick.app.lib.closeSearch(); });
    check('tracks from YouTube have no tooltips on their rows either', (await p.locator('.sr[title]').count()) === 0);
  }

  // 37. The sidebar: the search box on top, lists that fold away, lists that can be empty.
  {
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.keyboard.press('Control+f');
    await p.locator('.srch-close').click();
    await p.waitForTimeout(250);
    const rect = (sel) => p.locator(sel).first().evaluate((x) => { const r = x.getBoundingClientRect(); const c = getComputedStyle(x); return { top: r.top, left: r.left, h: r.height, font: c.fontSize, border: c.borderTopColor }; });
    const side0 = await rect('#side');
    const closedBox = await rect('#srch-btn');
    check('the search box is at the top of the sidebar', closedBox.top - side0.top <= 14 && closedBox.top < (await rect('#fold-playlists')).top);
    check('nothing is left at its foot', (await p.locator('.side-foot').count()) === 0);
    await p.locator('#srch-btn').click();
    await p.waitForTimeout(250);
    const openBox = await rect('#srch-in');
    check('and it is the same box as the search panel\'s', openBox.h === closedBox.h && openBox.font === closedBox.font && openBox.left === closedBox.left);
    check('the open panel has one way to close it, by its box', (await p.locator('.srch-close').count()) === 1 && !/Close search/.test(await p.locator('#side').textContent()));
    await p.locator('.srch-close').click();
    await p.waitForTimeout(250);
    check('closing it puts the focus back on the box', await p.evaluate(() => document.activeElement.id === 'srch-btn'));
    // With the window short and the sidebar scrolled, it's still there.
    await p.setViewportSize({ width: 1280, height: 420 });
    await p.locator('#side').evaluate((x) => { x.scrollTop = 200; });
    await p.waitForTimeout(100);
    check('it stays in sight as the sidebar scrolls', await p.evaluate(() => {
      const s = document.getElementById('side');
      const b = document.getElementById('srch-btn').getBoundingClientRect();
      const under = document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2);
      return s.scrollTop > 0 && b.top >= s.getBoundingClientRect().top && under && !!under.closest('#srch-btn');
    }));
    await p.locator('#side').evaluate((x) => { x.scrollTop = 0; });
    await p.setViewportSize({ width: 1280, height: 800 });

    // Folding away.
    const open = (key) => p.locator(`#fold-${key}`).getAttribute('aria-expanded');
    const plsN = await p.locator('[data-pl]').count();
    const setsN = await p.locator('[data-set]').count();
    check('both lists start open', (await open('playlists')) === 'true' && (await open('setlists')) === 'true' && plsN > 0 && setsN > 0);
    await p.locator('#fold-playlists').click();
    check('a list folds away, with its import link', (await open('playlists')) === 'false' && (await p.locator('[data-pl]').count()) === 0 && (await p.locator('.side-more').count()) === 0);
    check('leaving the other as it was', (await p.locator('[data-set]').count()) === setsN);
    check('its heading says how many are in it', new RegExp(`Playlists\\s*${plsN}`).test(await p.locator('#fold-playlists').textContent()));
    check('the open playlist stays open', await p.locator('#list .row').count() > 0);
    check('the focus stays on the heading', await p.evaluate(() => document.activeElement.id === 'fold-playlists'));
    await p.keyboard.press('Enter');
    check('Enter opens it again', (await open('playlists')) === 'true' && (await p.locator('[data-pl]').count()) === plsN);
    check('and the focus stays', await p.evaluate(() => document.activeElement.id === 'fold-playlists'));
    await p.locator('#fold-setlists').click();
    check('the other folds the same way', (await open('setlists')) === 'false' && (await p.locator('[data-set]').count()) === 0);
    const heading = await p.locator('#sec-setlists .side-h').evaluate((x) => x.getBoundingClientRect().height);
    check('a folded list is only its heading', (await p.locator('#sec-setlists').evaluate((x) => x.getBoundingClientRect().height)) <= heading + 1);
    // Making one shows it, to name it.
    await p.locator('#sec-setlists [aria-label="New setlist"]').click();
    check('a new setlist opens its folded list, to be named', (await open('setlists')) === 'true' && await p.locator('#side .input').isVisible());
    await p.keyboard.press('Escape');
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(60);
    await p.locator('#fold-playlists').click();
    await p.locator('#sec-playlists [aria-label="New playlist"]').click();
    check('so does a new playlist', (await open('playlists')) === 'true' && await p.locator('#side .input').isVisible());
    await p.keyboard.press('Escape');
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(60);
    check('and undoing those leaves things as they were', (await p.locator('[data-pl]').count()) === plsN && (await p.locator('[data-set]').count()) === setsN);
    // A track from the library dropped on a folded list's heading makes a playlist, and shows it.
    await p.locator('#fold-playlists').click();
    await p.locator('#lib-btn').click();
    await p.waitForTimeout(200);
    {
      await p.locator('.lr').first().scrollIntoViewIfNeeded();
      const from = await p.locator('.lr').first().boundingBox();
      const to = await p.locator('#fold-playlists').boundingBox();
      await p.mouse.move(from.x + 40, from.y + 10);
      await p.mouse.down();
      await p.mouse.move(from.x + 60, from.y + 30, { steps: 3 });
      await p.mouse.move(to.x + 30, to.y + to.height / 2, { steps: 4 });
      await p.waitForTimeout(60);
      check('a folded list\'s heading takes a drag', await p.locator('#side').evaluate((x) => x.classList.contains('drop-new')));
      await p.mouse.up();
    }
    await p.waitForTimeout(100);
    check('a track dropped on it makes a playlist, and opens the list', (await open('playlists')) === 'true' && (await p.locator('[data-pl]').count()) === plsN + 1);
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.waitForTimeout(60);
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(80);
    check('and undoing that takes it away', (await p.locator('[data-pl]').count()) === plsN);
    // The Setlists heading doesn't: that was never about playlists.
    await p.locator('#lib-btn').click();
    await p.waitForTimeout(200);
    {
      await p.locator('.lr').first().scrollIntoViewIfNeeded();
      const from = await p.locator('.lr').first().boundingBox();
      const to = await p.locator('#fold-setlists').boundingBox();
      await p.mouse.move(from.x + 40, from.y + 10);
      await p.mouse.down();
      await p.mouse.move(from.x + 60, from.y + 30, { steps: 3 });
      await p.mouse.move(to.x + 30, to.y + to.height / 2, { steps: 4 });
      await p.waitForTimeout(60);
      await p.mouse.up();
    }
    await p.waitForTimeout(100);
    check('a track dropped on the Setlists heading makes nothing', (await p.locator('[data-pl]').count()) === plsN);
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.waitForTimeout(60);

    // Removing the last playlist.
    const removeAll = async (row, label) => {
      for (;;) {
        const r = p.locator(row).first();
        if (!(await r.count())) return;
        await r.click({ button: 'right' });
        const item = p.getByRole('menuitem', { name: label });
        if (await item.isDisabled()) throw new Error(`${label} is disabled`);
        await item.click();
        await p.waitForTimeout(30);
      }
    };
    await p.locator('[data-pl]').first().click();
    await removeAll('[data-pl]', 'Remove playlist');
    await p.waitForTimeout(80);
    check('the last playlist can be removed', await p.evaluate(() => Fatstick.app.S.playlists.length === 0 && Fatstick.app.pl() === undefined));
    check('the sidebar says none', /No playlists/.test(await p.locator('#side-pls').textContent()) && await p.locator('.side-more').isVisible());
    check('the window says none and offers ways to make one', (await p.locator('#pl-name').textContent()) === 'No playlists' && await p.locator('#empty-new').isVisible() && /Import a playlist file/.test(await p.locator('#list-empty').textContent()));
    check('with no tools, columns or hint for a list that isn\'t there', (await p.locator('#pl-head .tools').count()) === 0 && await p.locator('#list-cols').isHidden() && await p.locator('#list-foot').isHidden());
    check('the display says so', /NO PLAYLISTS/.test(await p.locator('#disp-text').textContent()), await p.locator('#disp-text').textContent());
    check('setlists lost it too, and are still there', await p.evaluate(() => Fatstick.app.S.sets.length > 0 && Fatstick.app.S.sets.every((s) => s.playlistIds.every((x) => !x.startsWith('p-')))));
    await p.locator('#list').focus();
    for (const k of ['Control+a', 'Space', 'ArrowDown', 'ArrowUp', 'Delete', 'Control+c', 'Control+x', 'Control+v', 'Alt+ArrowUp', 'e', 'k', 'f', 't', 'Enter', 'Home', 'End']) await p.keyboard.press(k);
    await p.setViewportSize({ width: 1100, height: 700 });
    await p.waitForTimeout(80);
    await p.setViewportSize({ width: 1280, height: 800 });
    check('its keys do nothing, and a resize is fine', await p.evaluate(() => Fatstick.app.S.playlists.length === 0 && !Fatstick.app.S.player.playing) && (await p.locator('#modal-back').isHidden()));
    await p.getByRole('button', { name: 'Menu', exact: true }).click();
    check('the Menu can\'t export a playlist', await p.getByRole('menuitem', { name: /Export this playlist/ }).isDisabled());
    await p.keyboard.press('Escape');
    // The library and search add to a new playlist.
    await p.locator('#lib-btn').click();
    await p.waitForTimeout(150);
    await p.locator('.lr').first().click();
    check('the library offers a new playlist, and nothing to add to', await p.locator('#libview').getByRole('button', { name: 'New playlist', exact: true }).isVisible() && (await p.locator('#libview').getByRole('button', { name: /^Add to / }).count()) === 0);
    await p.locator('.lr').first().click();
    await p.locator('.lr').first().press('Shift+Enter');
    await p.waitForTimeout(80);
    check('adding from it makes one', await p.evaluate(() => Fatstick.app.S.playlists.length === 1 && Fatstick.app.pl().entries.length === 1));
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(80);
    check('undone, there\'s none again', await p.evaluate(() => Fatstick.app.S.playlists.length === 0 && Fatstick.app.pl() === undefined));
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.keyboard.press('Control+f');
    await p.locator('#srch-in').fill('Everywhere');
    const res = p.locator('.srch-res .sr').first();
    await res.waitFor();
    // Dropped on the empty window, it makes a playlist too.
    {
      const from = await res.boundingBox();
      const to = await p.locator('#list-wrap').boundingBox();
      await p.mouse.move(from.x + 30, from.y + 10);
      await p.mouse.down();
      await p.mouse.move(from.x + 60, from.y + 30, { steps: 3 });
      await p.mouse.move(to.x + to.width / 2, to.y + 200, { steps: 4 });
      await p.waitForTimeout(60);
      await p.mouse.up();
    }
    await p.waitForTimeout(100);
    check('a song dropped on the empty window gets a playlist', await p.evaluate(() => Fatstick.app.S.playlists.length === 1 && Fatstick.app.pl().entries.length === 1));
    await p.locator('.srch-close').click();
    await p.waitForTimeout(250);
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(80);
    check('undone, there\'s none again either', await p.evaluate(() => Fatstick.app.S.playlists.length === 0));
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(80);
    check('undo brings back the last playlist removed', await p.evaluate(() => Fatstick.app.S.playlists.length >= 1 && !!Fatstick.app.pl()) && (await p.locator('#pl-name').textContent()) !== 'No playlists' && await p.locator('#list-cols').isVisible());

    // Removing the last setlist, with its view and the transfer open.
    await p.locator('[data-set]').first().click();
    await p.waitForTimeout(80);
    await p.locator('#proc-fat').click();
    await p.waitForTimeout(250);
    await p.locator('#sv-play').click().catch(() => {});
    await p.waitForTimeout(80);
    await removeAll('[data-set]', 'Remove setlist');
    await p.waitForTimeout(300);
    check('the last setlist can be removed, and its view goes with it', await p.evaluate(() => Fatstick.app.S.sets.length === 0 && Fatstick.app.stickSet() === undefined && Fatstick.app.S.view === 'playlist'));
    check('and the transfer closes', await p.evaluate(() => Fatstick.app.S.processing === null) && await p.locator('#stick').isHidden());
    check('and whatever of it was playing stops', await p.evaluate(() => !Fatstick.app.S.player.set));
    check('the sidebar says none', /No setlists/.test(await p.locator('#side-sets').textContent()));
    await p.getByRole('button', { name: 'Menu', exact: true }).click();
    check('the Menu can\'t print a setlist', await p.getByRole('menuitem', { name: /Print a setlist/ }).isDisabled());
    await p.keyboard.press('Escape');
    await p.keyboard.press('Control+p');
    await p.waitForTimeout(60);
    check('and neither can Ctrl+P, which says why', await p.locator('#modal-back').isHidden());
    await p.keyboard.press('Control+z');
    await p.waitForTimeout(100);
    check('undo brings one back, with the view left where it is', await p.evaluate(() => Fatstick.app.S.sets.length >= 1 && !!Fatstick.app.stickSet() && Fatstick.app.S.view === 'playlist'));
    // ...and with its removal undone by a redo, nothing's left open either.
    await p.locator('[data-set]').first().click();
    await p.waitForTimeout(60);
    await p.keyboard.press('Control+Shift+z');
    await p.waitForTimeout(100);
    check('redoing it from its own view leaves the view', await p.evaluate(() => Fatstick.app.S.sets.length === 0 && Fatstick.app.S.view === 'playlist'));

    // Making them from nothing.
    await removeAll('[data-pl]', 'Remove playlist');
    await p.locator('#sec-setlists [aria-label="New setlist"]').click();
    await p.locator('#side .input').fill('Empty one');
    await p.keyboard.press('Enter');
    await p.waitForTimeout(60);
    check('a setlist can be made with no playlist to put in it', await p.evaluate(() => Fatstick.app.S.sets.length === 1 && Fatstick.app.S.sets[0].playlistIds.length === 0 && !!Fatstick.app.stickSet()));
    await p.locator('[data-set]').first().click();
    await p.waitForTimeout(60);
    check('and opened', await p.locator('#stickview').isVisible() && /Empty one/.test(await p.locator('#stickview').textContent()));
    await p.locator('#proc-fat').click();
    await p.waitForTimeout(250);
    check('and the transfer opens on it', await p.locator('#set-select').isVisible());
    await p.locator('#proc-close').click();
    await p.waitForTimeout(250);
    await p.evaluate(() => Fatstick.app.setView('playlist'));
    await p.locator('#empty-new').click();
    await p.locator('#side .input').fill('Fresh');
    await p.keyboard.press('Enter');
    await p.waitForTimeout(60);
    check('the window\'s button makes a playlist, and opens it', await p.evaluate(() => Fatstick.app.S.playlists.length === 1 && Fatstick.app.pl().name === 'Fresh') && (await p.locator('#pl-name').textContent()) === 'Fresh' && /This playlist is empty/.test(await p.locator('#list-empty').textContent()));
    if (process.argv[2]) await p.screenshot({ path: path.join(out, 'empty.png') });
  }

  check('no console errors', errs.length === 0, errs.join(' / '));
  await b.close();
  server.close();
  console.log(fails ? `${fails} FAILED` : 'ALL PASSED');
  process.exit(fails ? 1 : 0);
})();
