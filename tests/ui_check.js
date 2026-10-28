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

  // 7. Name template: typing updates the preview and keeps focus.
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

  // 11. Profile editor and all-names dialogs.
  await p.getByRole('button', { name: 'Edit…' }).click();
  await p.waitForTimeout(100);
  await p.screenshot({ path: `${out}/profile.png` });
  await p.keyboard.press('Escape');
  await p.getByRole('button', { name: 'Show all' }).click();
  await p.screenshot({ path: `${out}/allnames.png` });
  await p.keyboard.press('Escape');

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

  // 13. Sync flow.
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

  check('no console errors', errs.length === 0, errs.join(' / '));
  await b.close();
  server.close();
  console.log(fails ? `${fails} FAILED` : 'ALL PASSED');
  process.exit(fails ? 1 : 0);
})();
