// SYS-22: how fast app/ui draws a 42k-bookmark, 500-folder collection (headless, fake
// commands as in tree.mjs, so this times the page only; the store has its own bench:
// `cargo run --release -p store --example bench`). Prints medians; fails over 100 ms.
//
//   npm run perf            (from e2e/)

import { chromium } from 'playwright-core';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function fakeTauri() {
  let seed = 7;
  const rnd = (n) => ((seed = (seed * 1103515245 + 12345) % 2147483648) % n);
  // 500 folders up to 6 deep, all expanded (the worst case for the tree); folder 2 holds 2,000.
  const folders = [{ id: 1, parent_id: null, name: 'Bookmarks', position: 0, folded: false }];
  const depth = { 1: 0 };
  folders.push({ id: 2, parent_id: 1, name: 'Big', position: 1, folded: false });
  depth[2] = 1;
  while (folders.length < 501) {
    const p = folders[rnd(folders.length)];
    if (depth[p.id] >= 6 || p.id === 2) continue;
    const id = folders.length + 1;
    folders.push({ id, parent_id: p.id, name: `Folder ${id}`, position: id, folded: false });
    depth[id] = depth[p.id] + 1;
  }
  const items = [];
  for (let i = 0; i < 42000; i++) {
    const folder_id = i < 2000 ? 2 : 3 + rnd(499);
    items.push({ id: 1000 + i, folder_id, position: i, title: `Bookmark ${i} about things`, url: `https://site${i}.example/page`, kind: 'bookmark',
      notes: i % 5 ? '' : 'a note', keywords: 'rust web', description: 'description text', rating: 0, added: i, last_visited: null, visits: 0,
      check_enabled: true, check_status: null, changed: false, schedule: null, user_field: '' });
  }
  const byId = new Map(items.map((b) => [b.id, b]));
  const handlers = {
    folders: () => folders.map((f) => ({ ...f })),
    search_ids: ({ query, opts }) => (query.trim() ? items.filter((b) => b.title.includes(query.trim())) : items.filter((b) => b.folder_id === opts.folder)).map((b) => b.id),
    get_bookmarks: ({ ids }) => ids.map((id) => byId.get(id)).filter(Boolean),
    count: () => items.length,
    keywords: () => [],
    data_version: () => 1,
    history: () => ({ undo: null, redo: null }),
    saved_items: () => [],
    schedule_names: () => [],
    get_settings: () => ({ folder_keywords: true, autostart: false, schedules: '', url_rules: { protocol: true, trailing_slash: true, www: true, host_case: true, port: false } }),
  };
  window.__TAURI__ = { core: { invoke: async (cmd, args = {}) => (handlers[cmd] ? handlers[cmd](args) : null) }, event: { listen: async () => () => {} } };
}

const browser = await chromium.launch({ channel: process.platform === 'win32' ? 'msedge' : 'chrome' });
const page = await browser.newPage({ viewport: { width: 1320, height: 820 } });
let failed = 0;
page.on('pageerror', (e) => { failed++; console.log(`FAIL page error: ${e.message}`); });
await page.addInitScript(fakeTauri);
const t0 = Date.now();
await page.goto(pathToFileURL(path.join(root, 'app', 'ui', 'index.html')).href);
const idle = () => page.waitForFunction(() => document.body.ariaBusy === 'false');
await idle();
const load = Date.now() - t0;

// Time `fn` in the page up to the next painted frame, median of 9.
const time = (fn, arg) => page.evaluate(async ([src, a]) => {
  const f = new Function('a', `return (${src})(a)`);
  const ms = [];
  for (let i = 0; i < 9; i++) {
    const t = performance.now();
    await f(a);
    while (document.body.ariaBusy !== 'false') await new Promise((r) => setTimeout(r));
    await new Promise((r) => requestAnimationFrame(() => setTimeout(r)));
    ms.push(performance.now() - t);
  }
  return ms.sort((x, y) => x - y)[4];
}, [fn.toString(), arg]);

const rows = [
  ['page load to idle (500 folders open, 42k items)', load],
  ['tree: rows drawn', await page.$$eval('#tree li', (l) => l.length)],
  ['tree: renderTree()', await time(() => renderTree())],
  ['tree: collapse + expand a top folder', await time(() => { const li = [...document.querySelectorAll('#tree li[aria-level="2"] .twisty')].find((t) => t.textContent); li.click(); document.querySelector(`#tree li[data-id="${li.parentElement.dataset.id}"] .twisty`).click(); })],
  ['select Big (2,000 items): tree + list', await time(() => { setFolder(1); return setFolder(2); })],
  ['list: scroll to the end', await time(() => { const s = scroller(); s.scrollTop = s.scrollTop ? 0 : 1e6; s.dispatchEvent(new Event('scroll')); })],
  ['list: search across 42k (renders hits)', await time(() => { const q = document.getElementById('q'); q.value = q.value ? '' : 'Bookmark 1'; q.dispatchEvent(new Event('input')); })],
];
console.log('| UI (headless Chrome, median of 9, ms) | 42k |\n|---|---:|');
for (const [name, v] of rows) {
  console.log(`| ${name} | ${Number.isInteger(v) ? v : v.toFixed(1)} |`);
  if (/renderTree|collapse|select/.test(name) && v > 100) failed++;
}
await browser.close();
process.exit(failed ? 1 : 0);
