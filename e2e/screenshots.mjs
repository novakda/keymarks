// README screenshots from the demo collection (public sites only; see docs/demo/).
//
//   cargo run -p store --example demo -- %TEMP%\keymarks-demo
//   node screenshots.mjs %TEMP%\keymarks-demo        (closes and reopens the running app)

import { chromium } from 'playwright-core';
import { execSync, spawn } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { restoreKeymarks, runningKeymarks } from './running.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = path.join(root, 'target', 'debug', 'keymarks.exe');
const out = path.join(root, 'docs', 'screenshots');
const demo = process.argv[2];
if (!demo) throw new Error('usage: node screenshots.mjs <demo folder>');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const port = await new Promise((resolve) => {
  const srv = net.createServer().listen(0, '127.0.0.1', () => {
    const { port } = srv.address();
    srv.close(() => resolve(port));
  });
});
const stop = () => { try { execSync('taskkill /IM keymarks.exe /F', { stdio: 'ignore' }); } catch {} };
const usersKeymarks = runningKeymarks();
stop();
await sleep(1000);
spawn(exe, [], {
  env: { ...process.env, KEYMARKS_DATA_DIR: demo, KEYMARKS_NO_FOCUS: '1', WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
  detached: true, stdio: 'ignore',
}).unref();

let browser, page;
for (let i = 0; i < 60 && !page; i++) {
  try {
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 5000 });
    page = browser.contexts()[0]?.pages().find((p) => /tauri/.test(p.url()));
  } catch { await sleep(500); }
}
const idle = () => page.waitForFunction(() => document.body.ariaBusy === 'false', null, { timeout: 15000 });
await idle();
const saved = await page.evaluate(() => JSON.stringify({ ...localStorage }));

try {
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  await idle();

  // 1. Search: typing narrows the list and the keyword panel; properties on the right.
  await page.fill('#q', 'rust');
  await idle();
  await page.click('#list li.vrow:nth-child(2)');
  await idle();
  await page.click('#status'); // move focus off the row
  await page.screenshot({ path: path.join(out, 'search.png') });

  // 2. Table view, operators, sorted by rating.
  await page.click('#view-table');
  await page.fill('#q', 'docs | web');
  await idle();
  await page.click('#table-head th:has-text("Rating") button');
  await idle();
  await page.click('#table-body tr.vrow');
  await idle();
  await page.screenshot({ path: path.join(out, 'table.png') });

  // 3. Keyword dictionary right-click menu.
  await page.fill('#q', '');
  await idle();
  await page.click('#kwlist li:has-text("tauri")', { button: 'right' });
  await sleep(200);
  await page.screenshot({ path: path.join(out, 'keywords.png') });
  console.log('screenshots written to', out);
} finally {
  await page.evaluate((s) => { localStorage.clear(); Object.assign(localStorage, JSON.parse(s)); }, saved).catch(() => {});
  await browser?.close().catch(() => {});
  stop();
  await sleep(1000);
  restoreKeymarks(usersKeymarks);
}
