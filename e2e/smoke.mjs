// Drives the real app through its WebView2 debugging port and checks search, modes, errors,
// sorting and scrolling. Assertions are relations between results (e.g. `a b` <= `a`), so
// they hold for any database. Read-only: never opens, edits or deletes bookmarks, takes no
// screenshots, and restores the UI settings it changes.
//
//   npm run smoke            (from e2e/, on Windows; closes and reopens the running app)

import { chromium } from 'playwright-core';
import { execSync, spawn } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = process.env.POWERMARKS_EXE ?? path.join(root, 'target', 'debug', 'powermarks.exe');
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
  try { execSync('taskkill /IM powermarks.exe /F', { stdio: 'ignore' }); } catch { /* not running */ }
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

setTimeout(() => { log('timed out after 4 minutes'); stopApp(); startApp(); process.exit(2); }, 240_000).unref();
stopApp(); // a second instance can't start with different WebView2 arguments
log(`starting ${exe} with debug port ${PORT}`);
startApp({ WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}` });
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
  if (!m) throw new Error(`no result count after ${JSON.stringify(text)}: ${await statusText()}`);
  return Number(m[1]);
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
} catch (e) {
  check('run completed', false, e.message);
} finally {
  // Put the user's settings back and leave a normal (non-debug) app running.
  await page.evaluate((s) => { localStorage.clear(); Object.assign(localStorage, JSON.parse(s)); }, saved).catch(() => {});
  await browser.close().catch(() => {});
  stopApp();
  startApp();
}

const width = Math.max(...results.map((r) => r.name.length));
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name.padEnd(width)}  ${r.detail}`);
const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
