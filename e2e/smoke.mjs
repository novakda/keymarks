// Drives the real app through its WebView2 debugging port and checks search, modes, errors,
// sorting, scrolling, editing, deleting, undo/redo and backups. It runs on a temporary COPY of
// the user's database (KEYMARKS_DATA_DIR), never the original. Assertions are relations
// between results (e.g. `a b` <= `a`), so they hold for any database. No screenshots; output
// is counts only; UI settings it changes are restored.
//
//   npm run smoke            (from e2e/, on Windows; closes and reopens the running app)

import { chromium } from 'playwright-core';
import { execSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { restoreKeymarks, runningKeymarks } from './running.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = process.env.KEYMARKS_EXE ?? path.join(root, 'target', 'debug', 'keymarks.exe');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (msg) => process.stderr.write(`· ${new Date().toTimeString().slice(0, 8)} ${msg}\n`);

// Other apps (e.g. Electron) squat on common debug ports: ask the OS for a free one.
const PORT = await new Promise((resolve) => {
  const srv = net.createServer().listen(0, '127.0.0.1', () => {
    const { port } = srv.address();
    srv.close(() => resolve(port));
  });
});

// Waits until the app has really exited, so its files can be deleted.
function stopApp() {
  try { execSync('taskkill /IM keymarks.exe /F', { stdio: 'ignore', windowsHide: true }); } catch { /* not running */ }
  for (let i = 0; i < 50; i++) {
    const out = execSync('tasklist /FI "IMAGENAME eq keymarks.exe" /NH', { windowsHide: true }).toString();
    if (!out.includes('keymarks.exe')) return;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100); // sleep without spawning a process
  }
}

// On a desktop of its own (desktop.ps1): nothing the app does can reach the user's screen or focus.
function startApp(env) {
  execSync(`powershell -NoProfile -ExecutionPolicy Bypass -File "${path.join(root, 'e2e', 'desktop.ps1')}" "${exe}"`, {
    env: { ...process.env, ...env }, stdio: 'ignore', windowsHide: true,
  });
}

// Whatever Keymarks the user had open (installed or dev) is put back at the end.
const usersKeymarks = runningKeymarks();

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
// The copy holds the user's private bookmarks: it must not be left behind.
function cleanupWork() {
  try { fs.rmSync(webviewData, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 }); } catch { /* no private data in it */ }
  try {
    fs.rmSync(work, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 });
  } catch (e) {
    log(`could not delete ${work}: ${e.message}`);
  }
  if (fs.existsSync(work)) log(`WARNING: temporary copy still exists: ${work}`);
}
// Whatever happens, close the test app, delete the copy and give the user their app back.
let cleanedUp = false;
function emergencyExit(e) {
  if (cleanedUp) return;
  cleanedUp = true;
  log(`aborted: ${String(e?.message ?? e).replace(/"[^"]*"/g, '"…"')}`);
  stopApp();
  cleanupWork();
  restoreKeymarks(usersKeymarks);
  process.exit(1);
}
process.on('uncaughtException', emergencyExit);
process.on('unhandledRejection', emergencyExit);
setTimeout(() => { log('timed out after 4 minutes'); stopApp(); cleanupWork(); restoreKeymarks(usersKeymarks); process.exit(2); }, 240_000).unref();
log(`starting ${exe} on a copy of the database, debug port ${PORT}`);
// A WebView2 profile of its own: one left over from a previous run (or the user's) would be
// joined instead, and that one wasn't started with the debugging port.
// A saved preference the page must start with (settings.json seeds localStorage).
fs.writeFileSync(path.join(work, 'settings.json'), JSON.stringify({ ui: { 'import-title-words': '1' } }));
const webviewData = fs.mkdtempSync(path.join(os.tmpdir(), 'km-e2e-webview-'));
startApp({ WEBVIEW2_USER_DATA_FOLDER: webviewData, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --disable-features=CalculateNativeWinOcclusion --disable-backgrounding-occluded-windows`, KEYMARKS_DATA_DIR: work, KEYMARKS_NO_FOCUS: '1' });
const { browser, page } = await connect().catch(emergencyExit);
log(`connected to ${page.url()}`);


// WebView2 shows confirm()/prompt() as real Windows dialogs, which take focus from whatever
// the user is doing. So the page's dialogs are replaced by answers the test queues up front;
// no dialog window ever opens. Unexpected dialogs are refused and logged.
const fakeDialogs = () => {
  window.__e2eAnswers = [];
  const next = (kind, fallback) => {
    if (window.__e2eAnswers.length) return window.__e2eAnswers.shift();
    console.warn(`E2E unexpected ${kind} dialog`);
    return fallback;
  };
  window.confirm = () => next('confirm', false) === true;
  window.prompt = () => { const a = next('prompt', null); return a === true ? '' : a; };
  window.alert = () => { next('alert', undefined); };
};
await page.context().addInitScript(fakeDialogs); // survives reloads
await page.evaluate(fakeDialogs);
page.on('console', (m) => { if (m.text().startsWith('E2E unexpected')) log(m.text()); });
const expectDialog = (answer = true) => page.evaluate((a) => window.__e2eAnswers.push(a), answer);
const idle = () => page.waitForFunction(() => document.body.ariaBusy === 'false', null, { timeout: 15000 });
await page.waitForSelector('#status');
await idle();

const saved = await page.evaluate(() => JSON.stringify({ ...localStorage }));
const seeded = JSON.parse(saved)['import-title-words'] === '1';
await page.evaluate(() => localStorage.clear()); // known defaults: all fields, cards, default columns
await page.reload();
await idle();

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok: !!ok, detail });
  log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
};
check('saved preferences seed the page', seeded);

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
  const high = await count('rating>=8'), low = await count('rating<8');
  check('rating>=N filters by rating', high + low === await count('!zzzzqqq') && (await count('sql rating>=8')) <= high, `${high} + ${low}`);
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
  await search('sql');
  await page.focus('#kw-filter');
  await page.keyboard.press('ArrowDown');
  const focused = await page.evaluate(() => document.activeElement.dataset.keyword);
  await page.keyboard.press('Control+Enter');
  await idle();
  check('Ctrl+Enter on a keyword adds it to the search', focused && (await page.inputValue('#q')) === `sql ${focused}`);
  const first = await page.getAttribute('#kwlist li', 'data-keyword');
  await page.focus('#kw-filter');
  await page.keyboard.press('Enter');
  await idle();
  check('Enter in the keyword filter searches for the first keyword', (await page.inputValue('#q')) === first);

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
  await page.fill('#f-kw', 'e2ea+e2eb/e2ec.');
  await page.keyboard.press('Enter');
  const chipsNow = await page.$$eval('#f-tags .chip', (els) => els.map((e) => e.firstChild.textContent.replace(/^🏷 /, '')));
  check('forbidden keyword characters separate keywords', ['e2ea', 'e2eb', 'e2ec'].every((k) => chipsNow.includes(k)) && !chipsNow.some((k) => /[+/]/.test(k)));
  await page.click('#revert');
  await idle();
  await page.focus('#tablewrap');
  await page.keyboard.press('F1');
  check('F1 lists the keyboard shortcuts', await page.isVisible('#keys-dlg') && (await page.textContent('#keys-dlg')).includes('Shift+Enter'));
  await page.click('#keys-dlg [data-close]');
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
  await expectDialog();
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
  await search('');
  await page.click('#table-body tr.vrow:nth-child(1)');
  await idle();
  const user0 = await page.inputValue('#f-user');
  await page.fill('#f-user', 'E2E user field');
  await page.keyboard.press('Control+s');
  await idle();
  await page.click('#table-body tr.vrow:nth-child(2)');
  await idle();
  await page.click('#table-body tr.vrow:nth-child(1)');
  await idle();
  const user1 = await page.inputValue('#f-user');
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  check('the user field saves and undoes', user1 === 'E2E user field' && (await page.inputValue('#f-user')) === user0);

  // 7b. Rating keys and F8 (Check all asks first; the test answers No, so nothing is contacted)
  await search('');
  await page.click('#table-body tr.vrow');
  await idle();
  const r0 = Number(await page.inputValue('#f-rating'));
  await page.focus('#tablewrap');
  await page.keyboard.press(r0 === 10 ? 'Control+-' : 'Control+=');
  await idle();
  const r1 = Number(await page.inputValue('#f-rating'));
  await page.keyboard.press('Control+z');
  await idle();
  check('Ctrl+Plus/Minus changes the rating, Ctrl+Z restores it', Math.abs(r1 - r0) === 1 && Number(await page.inputValue('#f-rating')) === r0, `${r0} → ${r1}`);
  await expectDialog(false);
  await page.keyboard.press('F8');
  await idle();
  check('F8 asks before checking every link', (await page.evaluate(() => window.__e2eAnswers.length)) === 0 && (await page.evaluate(() => document.body.dataset.checking)) === 'false');

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
  check('scheduled checks are off unless turned on', !(await page.isChecked('#s-scheduled')) && /Weekly = 1w/.test(await page.inputValue('#s-schedules')));
  await page.fill('#s-schedules', 'Weekly = soon');
  await page.click('#s-save');
  await idle();
  check('a bad schedule is refused', /needs a number/.test(await page.textContent('#s-error')));
  await page.fill('#s-schedules', 'Daily = 1d\nWeekly = 1w\nMonthly = 1mo');
  await page.check('#s-newwin');
  await page.click('#s-save');
  await idle();
  const savedSettings = () => { try { return JSON.parse(fs.readFileSync(path.join(work, 'settings.json'), 'utf8')); } catch { return {}; } };
  const opening = savedSettings();
  await menu('settings-open');
  await page.waitForSelector('#settings[open]');
  const shown = await page.isChecked('#s-newwin');
  await page.uncheck('#s-newwin');
  await page.click('#s-save');
  await idle();
  check('opening settings save and show again, keeping view preferences', opening.open_new_window === true && shown && savedSettings().open_new_window === false && 'ui' in opening);
  const running = () => execSync('tasklist /FI "IMAGENAME eq keymarks.exe" /NH', { windowsHide: true }).toString().split('keymarks.exe').length - 1;
  startApp({ KEYMARKS_NO_FOCUS: '1' });
  await sleep(3000);
  check('second launch reuses the running app', running() === 1, `${running()} running`);
  check('window is visible after a second launch', await page.evaluate(() => document.visibilityState === 'visible'));

  // 10. Link checking against a local server only (no real site is contacted)
  const server = http.createServer((req, res) => {
    if (req.url === '/moved') { res.writeHead(301, { Location: '/new-home' }); res.end(); return; }
    if (req.url === '/new-home') { res.writeHead(200, { 'Content-Type': 'text/html' }); res.end('<p>home</p>'); return; }
    if (req.url.startsWith('/slow')) { setTimeout(() => { res.writeHead(200, { 'Content-Type': 'text/html' }); res.end('<p>slow</p>'); }, 700); return; }
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
  await fields(['title', 'url', 'keywords', 'description', 'notes']); // while still in exact mode
  // Ctrl+T from the search box checks the selection (here: one of the two local bookmarks).
  await page.click('#table-body tr.vrow');
  await idle();
  await page.focus('#q');
  await page.keyboard.press('Control+t');
  await page.waitForFunction(() => document.body.dataset.checking === 'false', null, { timeout: 30000 });
  await idle();
  check('Ctrl+T in the search box checks the selection', /^Checked 1 link/.test(await statusText()), (await statusText()).replace(/"[^"]*"/g, '"…"'));
  // Pause: the two links under way (two per site) finish, the other two wait for Resume.
  await search('');
  for (const row of [3, 4, 5, 6]) await setUrl(row, `${local}/slow${row}`);
  await search(`${local}/slow`);
  await page.click('#check-shown');
  await page.click('#check-pause');
  await page.waitForFunction(() => /^Paused at 2\//.test(document.getElementById('check-progress').textContent), null, { timeout: 5000 }).catch(() => {});
  await sleep(800); // nothing more may start while paused
  const pausedAt = await page.textContent('#check-progress');
  const marked = (await visibleCells('Title')).filter((t) => t.startsWith('◌')).length;
  check('Pause holds the remaining links, marked waiting', /^Paused at 2\/4/.test(pausedAt) && marked === 2, `${pausedAt}, ${marked} waiting`);
  await page.click('#check-pause');
  await page.waitForFunction(() => document.body.dataset.checking === 'false', null, { timeout: 30000 });
  await idle();
  check('Resume finishes the check', /^Checked 4 links/.test(await statusText()) && (await visibleCells('Title')).every((t) => !t.startsWith('◌')));
  server.close();

  // 12. New bookmark (Insert, as in Powermarks): its title is fetched from the page (local server only)
  const pages = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html' });
    res.end('<html><head><title>E2E Fetched Title</title><meta name="description" content="from the page"></head></html>');
  });
  await new Promise((r) => pages.listen(0, '127.0.0.1', r));
  const pageUrl = `http://127.0.0.1:${pages.address().port}/article`;
  await setMode('all');
  await page.focus('#tablewrap');
  await page.keyboard.press('Insert');
  await page.waitForSelector('#new-dlg[open]');
  await page.fill('#n-url', 'nope nope');
  await page.keyboard.press('Enter');
  await idle();
  check('New bookmark rejects an invalid URL', (await page.textContent('#n-error')).includes('valid URL'));
  await page.fill('#n-url', pageUrl);
  await page.keyboard.press('Enter');
  await idle();
  check('Insert adds and selects the new bookmark', (await page.inputValue('#f-url')) === pageUrl);
  await page.waitForFunction(() => document.getElementById('f-title')?.value === 'E2E Fetched Title', null, { timeout: 20000 }).catch(() => {});
  check('its title and description are fetched', (await page.inputValue('#f-title')) === 'E2E Fetched Title' && (await page.inputValue('#f-description')) === 'from the page');
  await page.fill('#f-title', 'E2E old title');
  await page.keyboard.press('Control+s');
  await idle();
  await page.focus('#tablewrap');
  await page.keyboard.press('F7');
  await idle();
  check('F7 fetches the title again', (await page.inputValue('#f-title')) === 'E2E Fetched Title' && /^Name and description updated for 1/.test(await statusText()));
  await page.keyboard.press('Control+z');
  await idle();
  check('Ctrl+Z undoes the fetch', (await page.inputValue('#f-title')) === 'E2E old title');
  pages.close();

  // 13. Saved searches and links (Powermarks' macros). Links are never clicked: that would open a browser.
  const savedNames = () => page.$$eval('#savedlist li:not(.hint) span:nth-child(2)', (els) => els.map((e) => e.textContent));
  await setMode('keywords');
  await search('sql');
  const sqlKeywordsOnly = Number((await statusText()).match(/^(\d+)/)[1]);
  await page.click('#save-search');
  await page.waitForSelector('#saved-dlg[open]');
  check('☆ proposes the search as name and shows its mode', (await page.inputValue('#sv-name')) === 'sql' && (await page.textContent('#sv-mode')).includes('Keywords only'));
  await page.fill('#sv-name', 'E2E keyword sql');
  await page.keyboard.press('Enter');
  await idle();
  check('saved search appears in the panel', (await savedNames()).includes('E2E keyword sql'));
  await setMode('all');
  await search('');
  await page.click('#savedlist li:has-text("E2E keyword sql")');
  await idle();
  check('running it restores query and mode', (await page.inputValue('#q')) === 'sql' && (await page.inputValue('#mode')) === 'keywords'
    && Number((await statusText()).match(/(\d+) matching/)?.[1]) === sqlKeywordsOnly, `${sqlKeywordsOnly} expected`);
  await search('');
  const position = (await savedNames()).indexOf('E2E keyword sql') + 1;
  if (position >= 1 && position <= 9) {
    await page.focus('#tablewrap');
    await page.keyboard.press(`Alt+${position}`);
    await idle();
    check(`Alt+${position} runs it`, (await page.inputValue('#q')) === 'sql');
  }
  await setMode('all');
  await page.click('#table-body tr.vrow');
  await idle();
  await page.click('#pin-link');
  await page.waitForSelector('#saved-dlg[open]');
  await page.fill('#sv-name', 'E2E pinned link');
  await page.keyboard.press('Enter');
  await idle();
  const afterPin = await savedNames();
  check('Pin to Saved adds a link after the search', afterPin.indexOf('E2E pinned link') === afterPin.indexOf('E2E keyword sql') + 1);
  await page.click('#savedlist li:has-text("E2E pinned link")', { button: 'right' });
  await page.click('#sctx [data-act=up]');
  await idle();
  const moved = await savedNames();
  check('Move up reorders', moved.indexOf('E2E pinned link') === moved.indexOf('E2E keyword sql') - 1);
  await page.click('#savedlist li:has-text("E2E keyword sql")', { button: 'right' });
  await page.click('#sctx [data-act=edit]');
  await page.fill('#sv-name', 'E2E renamed');
  await page.keyboard.press('Enter');
  await idle();
  check('Edit renames', (await savedNames()).includes('E2E renamed'));
  for (const name of ['E2E renamed', 'E2E pinned link']) {
    await page.click(`#savedlist li:has-text("${name}")`, { button: 'right' });
    await expectDialog();
    await page.click('#sctx [data-act=delete]');
    await idle();
  }
  check('Delete removes both', (await savedNames()).every((n) => !n.startsWith('E2E')));

  // 10b. Bookmark context menu (Copy URL is left out: it would overwrite the user's clipboard)
  await search('');
  await page.click('#table-body tr.vrow:nth-child(1)');
  await page.click('#table-body tr.vrow:nth-child(2)', { modifiers: ['Control'] });
  await idle();
  const rightClick = async () => { await page.click('#table-body tr.vrow:nth-child(2)', { button: 'right' }); await idle(); };
  await rightClick();
  check('right-click keeps a multi-selection', (await page.textContent('#bctx-title')) === '2 bookmarks' && await page.isHidden('#bctx [data-act=pin]'));
  const toggleLabel = () => page.textContent('#bctx [data-act=checking]');
  const label = await toggleLabel();
  await page.click('#bctx [data-act=checking]');
  await idle();
  await rightClick();
  check('link checking toggles from the menu', (await toggleLabel()) !== label);
  await page.click('#bctx [data-act=checking]'); // put it back
  await idle();
  await rightClick();
  await page.click('#bctx [data-act=addkw]');
  await page.fill('#ak-words', 'e2ecommonkw');
  await page.keyboard.press('Enter');
  await idle();
  const tagged = await count('e2ecommonkw');
  // Remove keywords (any case) from the same two, one undo step.
  await page.click('#table-body tr.vrow:nth-child(1)');
  await page.click('#table-body tr.vrow:nth-child(2)', { modifiers: ['Control'] });
  await idle();
  await rightClick();
  await page.click('#bctx [data-act=rmkw]');
  await page.fill('#ak-words', 'E2ECOMMONKW');
  await page.keyboard.press('Enter');
  await idle();
  const removedLeft = await count('e2ecommonkw');
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  check('Remove keywords takes them off the selection, Ctrl+Z puts them back', removedLeft === 0 && (await count('e2ecommonkw')) === 2, `${removedLeft}`);
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  await search('');
  await page.click('#table-body tr.vrow:nth-child(1)');
  await idle();
  const sched0 = await page.inputValue('#f-schedule');
  await page.selectOption('#f-schedule', 'Daily');
  await idle();
  const sched1 = await page.inputValue('#f-schedule');
  await page.click('#table-body tr.vrow:nth-child(2)', { modifiers: ['Control'] });
  await idle();
  await rightClick();
  await page.click('#bctx [data-act=schedule]');
  await page.selectOption('#sd-schedule', '');
  await page.click('#sched-form button[type=submit]');
  await idle();
  const schedMsg = await statusText();
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  await page.keyboard.press('Control+z');
  await idle();
  await page.click('#table-body tr.vrow:nth-child(1)');
  await idle();
  check('check schedule set from the panel and for a selection, undoable', sched1 === 'Daily' && /^2 bookmarks checked only when you ask/.test(schedMsg) && (await page.inputValue('#f-schedule')) === sched0);
  check('Add keywords puts them on every selected bookmark, Ctrl+Z removes them', tagged === 2 && (await count('e2ecommonkw')) === 0, `${tagged}`);
  const count0 = await everything();
  await search('');
  await page.click('#table-body tr.vrow:nth-child(1)');
  await idle();
  await rightClick(); // outside the selection: selects that row alone
  await expectDialog();
  await page.click('#bctx [data-act=delete]');
  await idle();
  check('menu Delete removes only the clicked bookmark', (await everything()) === count0 - 1);
  await search('');
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();

  // 10c. Pane splitter: drag, remembered across a reload, double-click resets
  const navWidth = () => page.evaluate(() => Math.round(document.querySelector('nav').getBoundingClientRect().width));
  const w0 = await navWidth();
  const bar = await page.locator('#split-left').boundingBox();
  await page.mouse.move(bar.x + 3, bar.y + 100);
  await page.mouse.down();
  await page.mouse.move(bar.x + 63, bar.y + 100, { steps: 5 });
  await page.mouse.up();
  const w1 = await navWidth();
  await page.reload();
  await idle();
  check('splitter resizes the side pane and remembers it', Math.abs(w1 - w0 - 60) <= 2 && (await navWidth()) === w1, `${w0} → ${w1}`);
  await page.dblclick('#split-left');
  check('double-click resets the pane', (await navWidth()) === w0);

  // 10d. Table columns: drag a header edge, remembered across a reload, double-click resets
  await page.click('#view-table');
  const thWidth = () => page.evaluate(() => Math.round(document.querySelector('#table-head th').getBoundingClientRect().width));
  const c0 = await thWidth();
  const grip = await page.locator('#table-head th:first-child .grip').boundingBox();
  await page.mouse.move(grip.x + 3, grip.y + 5);
  await page.mouse.down();
  await page.mouse.move(grip.x + 53, grip.y + 5, { steps: 5 });
  await page.mouse.up();
  const c1 = await thWidth();
  await page.reload();
  await idle();
  check('column edge drag resizes and is remembered', Math.abs(c1 - c0 - 50) <= 2 && (await thWidth()) === c1, `${c0} → ${c1}`);
  await page.dblclick('#table-head th:first-child .grip');
  check('double-click resets the columns', Math.abs((await thWidth()) - c0) <= 1);
  const heads = () => page.$$eval('#table-head th', (ths) => ths.map((th) => th.dataset.col));
  const sortsBefore = await page.$$eval('#table-head th', (ths) => ths.map((th) => th.ariaSort).join());
  const order0 = await heads();
  const dragHeader = async (from, to) => {
    const a = await page.locator(`#table-head th:nth-child(${from}) button`).boundingBox();
    const b = await page.locator(`#table-head th:nth-child(${to}) button`).boundingBox();
    await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
    await page.mouse.down();
    await page.mouse.move(b.x + 10, b.y + b.height / 2, { steps: 8 });
    await page.mouse.up();
    await idle();
  };
  await dragHeader(2, 1);
  const order1 = await heads();
  await page.reload();
  await idle();
  check('dragging a header moves the column, remembered', order1[0] === order0[1] && order1[1] === order0[0] && (await heads()).join() === order1.join(), order1.join(' '));
  check('a header drag does not sort', (await page.$$eval('#table-head th', (ths) => ths.map((th) => th.ariaSort).join())) === sortsBefore);
  await dragHeader(1, 2);
  // View preferences are copied to settings.json beside the database (OPT-02).
  const uiPrefs = () => { try { return JSON.parse(fs.readFileSync(path.join(work, 'settings.json'), 'utf8')).ui || {}; } catch { return {}; } };
  for (let i = 0; i < 20 && !uiPrefs().columns?.startsWith('["title"'); i++) await sleep(100);
  check('view and columns saved in settings.json', uiPrefs().view === 'table' && uiPrefs().columns?.startsWith('["title"'), Object.keys(uiPrefs()).join(' '));

  // Display menu: colour by rating, relative dates.
  await search('');
  await page.click('#table-body tr.vrow:nth-child(1)');
  await idle();
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+=');
  await idle();
  await page.click('#display summary');
  await page.check('#d-rating');
  await idle();
  const rated = await page.$$eval('#table-body tr.vrow:nth-child(1).rated', (r) => r.length);
  const addedCol = (await heads()).indexOf('added') + 1;
  await page.selectOption('#d-dates', 'relative');
  await idle();
  const rel = await page.$$eval(`#table-body tr.vrow td:nth-child(${addedCol})`, (tds) => tds.filter((td) => /ago|yesterday|today|now|last|this/i.test(td.textContent)).length);
  await page.selectOption('#d-dates', 'date');
  await page.uncheck('#d-rating');
  await page.click('#display summary');
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  for (let i = 0; i < 20 && uiPrefs()['date-format'] !== 'date'; i++) await sleep(100);
  check('Display: colour by rating and relative dates', rated === 1 && rel > 0 && uiPrefs()['date-format'] === 'date', `${rated} ${rel}`);

  // Mark visited counts a visit; Alt+Right goes to the next bookmark, saving edits first.
  const visits = async () => Number((await page.textContent('#info')).match(/Visited (\d+)/)[1]);
  const v0 = await visits();
  await page.click('#table-body tr.vrow:nth-child(1)', { button: 'right' });
  await idle();
  await page.click('#bctx [data-act=visited]');
  await idle();
  check('Mark visited counts a visit', (await visits()) === v0 + 1, `${v0}`);
  const second = await page.getAttribute(`#table-body tr.vrow:nth-child(2) td:nth-child(${(await heads()).indexOf('title') + 1})`, 'title');
  await page.fill('#f-notes', 'e2e-alt-nav-note');
  await page.keyboard.press('Alt+ArrowRight');
  await idle();
  const movedTo = await page.inputValue('#f-title');
  const cur = await page.$$eval('#table-body tr.vrow', (rows) => rows.findIndex((r) => r.classList.contains('cursor')));
  const noted = await count('e2e-alt-nav-note');
  check('Alt+Right saves the edits and shows the next bookmark', cur === 1 && movedTo && second.endsWith(movedTo) && noted === 1, `cursor row ${cur} noted ${noted}`); // the cell has status marks before the title
  await page.focus('#tablewrap');
  await page.keyboard.press('Control+z');
  await idle();
  await search('');

  // 11. Import and Export dialogs (counts only; nothing is imported or exported here)
  await menu('import');
  await page.waitForSelector('#import-dlg[open]');
  await page.waitForFunction(() => !document.getElementById('import-sources').textContent.includes('Looking for browsers'), null, { timeout: 20000 });
  const sourceRows = await page.locator('#import-sources tr').count();
  check('Import offers keywords from titles', await page.isVisible('#import-title-words'));
  check('Import lists browser profiles', sourceRows > 0 || (await page.textContent('#import-sources')).includes('No Chrome'), `${sourceRows} profiles`);
  await page.click('#import-dlg button[value=close]');
  await menu('export-open');
  await page.waitForSelector('#export-dlg[open]');
  check('Export offers all, matching and selected', /All [\d,]+ bookmarks/.test(await page.textContent('#ex-all')) && /matching/.test(await page.textContent('#ex-matching')));
  await page.click('#export-dlg button[data-close]');
} catch (e) {
  check('run completed', false, e.message);
} finally {
  // Put the user's settings back and leave a normal (non-debug) app running.
  await page.evaluate((s) => { localStorage.clear(); Object.assign(localStorage, JSON.parse(s)); }, saved).catch(() => {});
  await browser.close().catch(() => {});
  cleanedUp = true;
  stopApp();
  cleanupWork();
  restoreKeymarks(usersKeymarks);
}

const width = Math.max(...results.map((r) => r.name.length));
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name.padEnd(width)}  ${r.detail}`);
const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
