// Drives the real app through its WebView2 debugging port and checks search, modes, errors,
// sorting, scrolling, editing, deleting, undo/redo and backups. It runs on a temporary COPY of
// the user's database (KEYMARKS_DATA_DIR), never the original. Assertions are relations
// between results (e.g. `a b` <= `a`), so they hold for any database. No screenshots; output
// is counts only; UI settings it changes are restored.
//
//   npm run smoke            (from e2e/, on Windows; closes and reopens the running app)

import { chromium } from 'playwright-core';
import { execSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = process.env.KEYMARKS_EXE ?? path.join(root, 'target', 'debug', 'keymarks.exe');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (msg) => process.stderr.write(`· ${msg}\n`);

// Other apps (e.g. Electron) squat on common debug ports: ask the OS for a free one.
const PORT = await new Promise((resolve) => {
  const srv = net.createServer().listen(0, '127.0.0.1', () => {
    const { port } = srv.address();
    srv.close(() => resolve(port));
  });
});

function stopApp() {
  try { execSync('taskkill /IM keymarks.exe /F', { stdio: 'ignore' }); } catch { /* not running */ }
}

function startApp(env) {
  if (env) {
    spawn(exe, [], { env: { ...process.env, ...env }, detached: true, stdio: 'ignore' }).unref();
  } else {
    // Via Explorer: a normal launch that inherits none of our handles, so callers waiting
    // on this script's output aren't kept waiting by the app.
    spawn('explorer.exe', [exe], { detached: true, stdio: 'ignore' }).unref();
  }
}

async function connect() {
  for (let i = 0; i < 60; i++) {
    try {
      const browser = await chromium.connectOverCDP(`http://127.0.0.1:${PORT}`, { timeout: 5000 });
      const page = browser.contexts()[0]?.pages().find((p) => /tauri/.test(p.url()));
      if (page) return { browser, page };
      await browser.close();
    } catch { /* not up yet */ }
    await sleep(500);
  }
  throw new Error(`app did not expose port ${PORT}`);
}

// ---- run ------------------------------------------------------------------------------

// The app must be closed first: one instance only, and the copy must be consistent.
stopApp();
const realData = path.join(process.env.APPDATA, 'com.pattern158.keymarks');
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'pm-e2e-'));
for (const f of ['bookmarks.db', 'bookmarks.db-wal', 'bookmarks.db-shm']) {
  if (fs.existsSync(path.join(realData, f))) fs.copyFileSync(path.join(realData, f), path.join(work, f));
}
// The killed app can hold the files for a moment; retry rather than leave a copy behind.
const cleanupWork = () => fs.rmSync(work, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 });
setTimeout(() => { log('timed out after 4 minutes'); stopApp(); cleanupWork(); startApp(); process.exit(2); }, 240_000).unref();
log(`starting ${exe} on a copy of the database, debug port ${PORT}`);
startApp({ WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}`, KEYMARKS_DATA_DIR: work });
const { browser, page } = await connect();
log(`connected to ${page.url()}`);
const idle = () => page.waitForFunction(() => document.body.ariaBusy === 'false', null, { timeout: 15000 });
await page.waitForSelector('#status');
await idle();

const saved = await page.evaluate(() => JSON.stringify({ ...localStorage }));
await page.evaluate(() => localStorage.clear()); // known defaults: all fields, cards, default columns
await page.reload();
await idle();

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok: !!ok, detail });
  log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
};

const statusText = () => page.textContent('#status');
async function search(text) {
  await page.fill('#q', text);
  await idle();
}
async function count(text) {
  await search(text);
  const m = (await statusText()).match(/^(\d+) matching/);
  // Status messages can quote bookmark titles: mask quoted text before printing.
  if (!m) throw new Error(`no result count after ${JSON.stringify(text)}: ${(await statusText()).replace(/"[^"]*"/g, '"…"')}`);
  return Number(m[1]);
}
// Import, Export, Backups and Settings live in the ⋯ menu.
async function menu(id) {
  await page.click('#more summary');
  await page.click(`#${id}`);
}
async function setMode(mode) {
  await page.selectOption('#mode', mode);
  await idle();
}
async function setOptions(fn) {
  await page.click('#opts summary');
  await fn();
  await idle();
  await page.click('#status'); // outside click closes the menu
}
const fields = (wanted) => setOptions(async () => {
  for (const f of ['title', 'url', 'keywords', 'description', 'notes']) {
    await page.setChecked(`input[name=field][value=${f}]`, wanted.includes(f));
    await idle();
  }
});
async function visibleCells(column) {
  return page.evaluate((label) => {
    const heads = [...document.querySelectorAll('#table-head th')].map((th) => th.textContent.replace(/[▲▼2]/g, '').trim());
    const i = heads.indexOf(label);
    return [...document.querySelectorAll('#table-body tr.vrow')].map((tr) => tr.cells[i]?.textContent ?? '');
  }, column);
}

try {
  const total = Number((await statusText()).match(/· (\d+) bookmarks/)[1]);

  // 1. Operators (All fields)
  const sql = await count('sql');
  const mvc = await count('mvc');
  const sqlMvc = await count('sql mvc');
  check('word search finds results', sql > 0, `sql=${sql}`);
  check('implicit AND narrows', sqlMvc <= Math.min(sql, mvc), `sql mvc=${sqlMvc}`);
  const py = await count('python');
  const js = await count('javascript');
  const either = await count('python | javascript');
  check('OR is a union', either >= Math.max(py, js) && either <= py + js, `${py} | ${js} = ${either}`);
  const prog = await count('programming');
  const progPy = await count('programming python');
  const progNotPy = await count('programming ! python');
  check('a ! b = a minus (a b)', progNotPy === prog - progPy, `${prog} - ${progPy} = ${progNotPy}`);
  const google = await count('google');
  const notGoogle = await count('!google');
  check('leading NOT complements', google + notGoogle === total, `${google} + ${notGoogle} = ${total}`);
  const lw = await count('linux | windows');
  const grouped = await count('(linux | windows) tutorial');
  check('parentheses group', grouped <= lw, `(linux|windows) tutorial=${grouped} of ${lw}`);
  check('dotted operators', (await count('sql .and .not mvc')) === (await count('sql ! mvc')));
  const net = await count('net');
  const netWhole = await count('net.');
  check('word. is whole-word only', netWhole <= net, `net=${net} net.=${netWhole}`);

  // 2. Keywords only vs All fields
  const redditAll = await count('reddit');
  await setMode('keywords');
  const redditKw = await count('reddit');
  check('Keywords-only is narrower', redditKw <= redditAll, `keywords=${redditKw} all=${redditAll}`);
  await setMode('all');

  // 3. Exact text in URLs (table view so the URL column can be read)
  await page.click('#view-table');
  await setMode('exact');
  await fields(['url']);
  const gh = await count('github.com/');
  const urls = await visibleCells('URL');
  check('exact URL search matches URLs', gh > 0 && urls.length > 0 && urls.every((u) => u.toLowerCase().includes('github.com/')), `${gh} hits, ${urls.length} rows checked`);
  await fields(['title']);
  const anyCase = await count('SQL');
  await setOptions(() => page.setChecked('#opt-case', true));
  const exactCase = await count('SQL');
  check('match case narrows', exactCase <= anyCase, `SQL: ${exactCase} cased vs ${anyCase}`);
  await setOptions(() => page.setChecked('#opt-case', false));
  await fields(['title', 'url', 'keywords', 'description', 'notes']);
  await setMode('all');

  // 4. Errors, Esc, keyword panel
  const good = await count('sql');
  for (const bad of ['(sql | mvc', 'sql &', '| sql']) {
    await search(bad);
    const invalid = await page.getAttribute('#q', 'aria-invalid');
    const msg = await statusText();
    const rows = await page.locator('#table-body tr.vrow').count();
    check(`syntax error reported: ${bad}`, invalid === 'true' && msg.startsWith('Search:') && rows > 0, msg);
  }
  check('valid search clears the error', (await count('sql')) === good && (await page.getAttribute('#q', 'aria-invalid')) === null);
  await page.press('#q', 'Escape');
  await idle();
  check('Esc clears the search', (await page.inputValue('#q')) === '' && (await statusText()).startsWith(`${total} matching`));
  await search('sql');
  const before = await count('sql');
  const kw = await page.textContent('#kwlist li span:nth-child(2)');
  await page.click('#kwlist li');
  await idle();
  const after = Number((await statusText()).match(/^(\d+)/)[1]);
  check('keyword click adds to the search', (await page.inputValue('#q')) === `sql ${kw}` && after <= before, `${before} → ${after}`);

  // 5. Sorting and scrolling (table)
  await search('');
  const dates = async () => (await visibleCells('Added')).filter(Boolean).map((d) => Date.parse(d));
  const sorted = (xs, desc) => xs.every((x, i) => i === 0 || (desc ? xs[i - 1] >= x : xs[i - 1] <= x));
  await page.click('#table-head th:has-text("Added") button');
  await idle();
  check('Added sorts newest first', sorted(await dates(), true));
  await page.click('#table-head th:has-text("Added") button');
  await idle();
  check('second click reverses', sorted(await dates(), false));
  await page.click('#table-head th:has-text("Rating") button');
  await idle();
  check('previous sort becomes tiebreak', (await page.textContent('#table-head th:has-text("Added")')).includes('2'));
  await page.$eval('#tablewrap', (el) => { el.scrollTop = el.scrollHeight; });
  await sleep(600); // virtual scroller fetches the newly visible page
  const last = await page.evaluate(() => {
    const body = document.getElementById('table-body');
    return { rows: body.querySelectorAll('tr.vrow').length, trailingSpacer: body.lastElementChild?.classList.contains('spacer'), loaded: body.querySelector('tr.vrow td')?.textContent !== '…' };
  });
  // Details print counts only: never bookmark titles or keywords (the data is private).
  check('scrolls to the very last bookmark', last.rows > 0 && !last.trailingSpacer && last.loaded, `${last.rows} rows rendered`);

  // 6. Edit, undo, redo (on the copy)
  await page.$eval('#tablewrap', (el) => { el.scrollTop = 0; });
  await idle();
  await page.click('#table-body tr.vrow');
  await idle();
  const original = await page.inputValue('#f-title');
  await page.fill('#f-title', `${original} [e2e]`);
  await page.keyboard.press('Control+s');
  await idle();
  const savedTitle = await page.inputValue('#f-title');
  check('edit saves', savedTitle.endsWith('[e2e]') && (await statusText()) === 'Saved.');
  check('undo button names the edit', (await page.getAttribute('#undo', 'title')).startsWith('Undo: Edit'));
  await page.click('#undo');
  await idle();
  check('undo restores the edit', (await page.inputValue('#f-title')) === original);
  await page.click('#redo');
  await idle();
  check('redo reapplies it', (await page.inputValue('#f-title')).endsWith('[e2e]'));
  await page.click('#undo');
  await idle();

  // 7. Delete and Ctrl+Z ("!zzzzqqq" matches everything and puts the count in the status bar)
  const everything = () => count('!zzzzqqq');
  const all = await everything();
  await search('');
  await page.click('#table-body tr.vrow');
  await page.focus('#tablewrap');
  page.once('dialog', (d) => d.accept());
  await page.keyboard.press('Delete');
  await idle();
  const afterDelete = await everything();
  check('Delete removes the bookmark', afterDelete === all - 1, `${all} → ${afterDelete}`);
  await search('');
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  const afterUndo = await everything();
  check('Ctrl+Z brings it back', afterUndo === all, `→ ${afterUndo}`);

  // 8. Backups (one is taken at startup, in the background)
  await menu('backups-open');
  await page.waitForSelector('#backup-list tbody tr', { timeout: 10000 }).catch(() => {});
  const backupRows = await page.locator('#backup-list tbody tr').count();
  check('Backups lists the startup backup', backupRows >= 1, `${backupRows} backups`);
  await page.click('#backups button[value=close]');

  // 9. Desktop: settings dialog, single instance (a second launch hands over and exits)
  await menu('settings-open');
  await page.waitForSelector('#settings[open]');
  check('Settings shows the hotkey', (await page.inputValue('#s-hotkey')).length > 0);
  await page.click('#settings button[value=cancel]');
  const running = () => execSync('tasklist /FI "IMAGENAME eq keymarks.exe" /NH').toString().split('keymarks.exe').length - 1;
  spawn(exe, [], { detached: true, stdio: 'ignore' }).unref();
  await sleep(3000);
  check('second launch reuses the running app', running() === 1, `${running()} running`);
  check('window is visible after a second launch', await page.evaluate(() => document.visibilityState === 'visible'));

  // 10. Link checking against a local server only (no real site is contacted)
  const server = http.createServer((req, res) => {
    if (req.url === '/moved') { res.writeHead(301, { Location: '/new-home' }); res.end(); return; }
    if (req.url === '/new-home') { res.writeHead(200, { 'Content-Type': 'text/html' }); res.end('<p>home</p>'); return; }
    res.writeHead(404); res.end('gone');
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  // The port makes these URLs unique: the user's own data may contain other 127.0.0.1 links.
  const local = `http://127.0.0.1:${server.address().port}`;
  const setUrl = async (row, url) => {
    await page.click(`#table-body tr.vrow:nth-child(${row})`);
    await idle();
    await page.fill('#f-url', url);
    await page.keyboard.press('Control+s');
    await idle();
  };
  await search('');
  await setUrl(1, `${local}/moved`);
  await setUrl(2, `${local}/missing`);
  await setMode('exact');
  await fields(['url']);
  const localCount = await count(local);
  check('two bookmarks point at the local server', localCount === 2, `${localCount}`);
  await page.click('#check-shown');
  await page.waitForFunction(() => document.body.dataset.checking === 'false', null, { timeout: 30000 });
  await idle();
  const summary = (await statusText()).replace(/"[^"]*"/g, '"…"');
  const urlsNow = await visibleCells('URL');
  check('link check finds the missing page', /not found/i.test(summary), summary);
  check('permanent redirect is repaired', urlsNow.length === 2 && urlsNow.some((u) => u.endsWith('/new-home')) && /Updated 1 moved URL/.test(summary), `${urlsNow.length} rows`);
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  check('undo restores the original URL', (await visibleCells('URL')).some((u) => u.endsWith('/moved')));
  // Ctrl+T from the search box checks the selection (here: one of the two local bookmarks).
  await page.click('#table-body tr.vrow');
  await idle();
  await page.focus('#q');
  await page.keyboard.press('Control+t');
  await page.waitForFunction(() => document.body.dataset.checking === 'false', null, { timeout: 30000 });
  await idle();
  check('Ctrl+T in the search box checks the selection', /^Checked 1 link/.test(await statusText()), (await statusText()).replace(/"[^"]*"/g, '"…"'));
  server.close();

  // 11. Import and Export dialogs (counts only; nothing is imported or exported here)
  await menu('import');
  await page.waitForSelector('#import-dlg[open]');
  await page.waitForFunction(() => !document.getElementById('import-sources').textContent.includes('Looking for browsers'), null, { timeout: 20000 });
  const sourceRows = await page.locator('#import-sources tr').count();
  check('Import lists browser profiles', sourceRows > 0 || (await page.textContent('#import-sources')).includes('No Chrome'), `${sourceRows} profiles`);
  await page.click('#import-dlg button[value=close]');
  await menu('export-open');
  await page.waitForSelector('#export-dlg[open]');
  check('Export offers all, matching and selected', /All [\d,]+ bookmarks/.test(await page.textContent('#ex-all')) && /matching/.test(await page.textContent('#ex-matching')));
  await page.click('#export-dlg button[value=cancel]');
  await fields(['title', 'url', 'keywords', 'description', 'notes']);
  await setMode('all');
} catch (e) {
  check('run completed', false, e.message);
} finally {
  // Put the user's settings back and leave a normal (non-debug) app running.
  await page.evaluate((s) => { localStorage.clear(); Object.assign(localStorage, JSON.parse(s)); }, saved).catch(() => {});
  await browser.close().catch(() => {});
  stopApp();
  cleanupWork();
  startApp();
}

const width = Math.max(...results.map((r) => r.name.length));
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name.padEnd(width)}  ${r.detail}`);
const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
