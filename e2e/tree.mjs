// Headless check of the folder tree and folder-scoped list in app/ui, with the Tauri
// commands replaced by a small in-memory fake (no app, no database, any OS with a Chromium
// playwright-core can drive: Edge on Windows, Chrome elsewhere). It tests the page's logic: what the tree shows, what it
// asks the store for, and how notes look. The store's own behaviour is covered by
// `cargo test -p store`; the real app by smoke.mjs (Windows).
//
//   npm run tree            (from e2e/)

import { chromium } from 'playwright-core';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
let failed = 0;
const check = (what, ok, detail = '') => {
  if (!ok) failed++;
  process.stdout.write(`${ok ? 'ok  ' : 'FAIL'} ${what}${ok || !detail ? '' : ` (${detail})`}\n`);
};

// Runs in the page before app.js: a fake of the commands the page calls.
function fakeTauri() {
  const folders = [
    { id: 1, parent_id: null, name: 'Bookmarks', position: 0, folded: true },
    { id: 2, parent_id: 1, name: 'Work', position: 1, folded: true },
    { id: 3, parent_id: 2, name: 'Clients', position: 1, folded: true },
    { id: 4, parent_id: 1, name: 'Home', position: 2, folded: true },
  ];
  const bm = (id, folder_id, position, title, url, kind = 'bookmark', notes = '') => ({
    id, folder_id, position, title, url, kind, notes, keywords: '', description: '', rating: 0, added: id, last_visited: null,
    visits: 0, check_enabled: kind === 'bookmark', check_status: null, changed: false, schedule: null, user_field: '',
  });
  const items = [
    bm(10, 1, 3, 'Root page', 'https://root.example/'),
    bm(11, 2, 2, 'Wiki', 'https://wiki.example/'),
    bm(12, 2, 3, 'Standup notes', '', 'note', 'Ask about the release\nsecond line'),
    bm(13, 2, 4, 'Tracker', 'https://tracker.example/'),
    bm(14, 3, 1, 'Client wiki', 'https://client.example/'),
  ];
  const calls = (window.__calls = []);
  let nextId = 100;
  const byPos = (a, b) => a.position - b.position;
  const handlers = {
    folders: () => [...folders].sort((a, b) => (a.parent_id ?? 0) - (b.parent_id ?? 0) || byPos(a, b)).map((f) => ({ ...f })),
    search_ids: ({ query, opts }) => (query.trim()
      ? items.filter((b) => b.title.toLowerCase().includes(query.trim().toLowerCase())).map((b) => b.id)
      : items.filter((b) => b.folder_id === opts.folder).sort(byPos).map((b) => b.id)),
    get_bookmarks: ({ ids }) => ids.map((id) => items.find((b) => b.id === id)).filter(Boolean),
    count: () => items.length,
    keywords: () => [],
    data_version: () => 1,
    history: () => ({ undo: null, redo: null }),
    saved_items: () => [],
    schedule_names: () => [],
    set_folder_folded: ({ id, folded }) => { folders.find((f) => f.id === id).folded = folded; },
    create_folder: ({ parent, name }) => {
      const id = nextId++;
      folders.push({ id, parent_id: parent, name: name.trim(), position: 99, folded: true });
      return id;
    },
    rename_folder: ({ id, name }) => { folders.find((f) => f.id === id).name = name.trim(); },
    delete_folder: ({ id }) => {
      const f = folders.find((x) => x.id === id);
      for (const c of folders) if (c.parent_id === id) c.parent_id = f.parent_id;
      for (const b of items) if (b.folder_id === id) b.folder_id = f.parent_id;
      folders.splice(folders.indexOf(f), 1);
    },
    add_note: ({ folder, title }) => {
      const id = nextId++;
      items.push(bm(id, folder, 99, title.trim(), '', 'note'));
      return id;
    },
  };
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        calls.push([cmd, JSON.parse(JSON.stringify(args))]);
        return handlers[cmd] ? handlers[cmd](args) : null;
      },
    },
    event: { listen: async () => () => {} },
  };
}

// An installed browser, so nothing is downloaded: Edge on Windows, Chrome elsewhere.
const browser = await chromium.launch({ channel: process.platform === 'win32' ? 'msedge' : 'chrome' });
const page = await browser.newPage({ viewport: { width: 1320, height: 820 } });
page.on('pageerror', (e) => { failed++; process.stdout.write(`FAIL page error: ${e.message}\n`); });
await page.addInitScript(fakeTauri);
await page.goto(pathToFileURL(path.join(root, 'app', 'ui', 'index.html')).href);

const idle = () => page.waitForFunction(() => document.body.ariaBusy === 'false');
const tree = () => page.$$eval('#tree li', (lis) => lis.map((li) => li.querySelector('.name').textContent));
const titles = () => page.$$eval('#list li.vrow .title', (els) => els.map((e) => e.textContent));
const status = () => page.textContent('#status');
const calls = (cmd) => page.evaluate((c) => window.__calls.filter(([n]) => n === c).map(([, a]) => a), cmd);
const item = (name) => page.locator('#tree li', { has: page.locator('.name', { hasText: new RegExp(`^${name}$`) }) });
const answer = (text) => page.once('dialog', (d) => d.accept(text));

await idle();
check('the tree shows the root and its folders; collapsed ones hide their children', JSON.stringify(await tree()) === '["Bookmarks","Work","Home"]', JSON.stringify(await tree()));
check('the root is selected and the list shows only its items', JSON.stringify(await titles()) === '["Root page"]', JSON.stringify(await titles()));
check('the status bar names the folder', (await status()).startsWith('1 matching in “Bookmarks”'), await status());
check('the empty search asks for the folder', (await calls('search_ids')).at(-1).opts.folder === 1);

await item('Work').click();
await idle();
const work = await titles();
check('selecting a folder lists its bookmarks and notes in folder order', JSON.stringify(work) === '["Wiki","✎ Standup notes","Tracker"]', JSON.stringify(work));
const note = page.locator('#list li.vrow').nth(1);
check('a note is marked and shows its text, not a URL', (await note.getAttribute('class')).includes('note') && (await note.locator('.meta').textContent()) === 'Ask about the release');
check('subfolders stay out of the list (Explorer style)', !work.some((t) => t.includes('Clients')));
check('keywords follow the folder too', (await calls('keywords')).at(-1).opts.folder === 2);

await item('Work').locator('.twisty').click();
check('expanding shows the child folder', JSON.stringify(await tree()) === '["Bookmarks","Work","Clients","Home"]', JSON.stringify(await tree()));
check('the expand state is saved', JSON.stringify((await calls('set_folder_folded')).at(-1)) === '{"id":2,"folded":false}');
await page.focus('#tree');
await page.keyboard.press('ArrowLeft');
check('Left collapses', JSON.stringify(await tree()) === '["Bookmarks","Work","Home"]');
await page.keyboard.press('ArrowRight');
await page.keyboard.press('ArrowRight');
await idle();
check('Right expands, then moves into the child', (await page.textContent('#tree li[aria-selected=true] .name')) === 'Clients' && JSON.stringify(await titles()) === '["Client wiki"]');

await page.fill('#q', 'wiki');
await idle();
const found = await titles();
check('a search covers the whole collection', found.length === 2 && found.includes('Wiki') && found.includes('Client wiki'), JSON.stringify(found));
check('the status bar drops the folder while searching', /^2 matching ·/.test(await status()), await status());
await item('Home').click();
await idle();
check('choosing a folder clears the search and shows the folder', (await page.inputValue('#q')) === '' && (await titles()).length === 0);

await item('Work').click();
await idle();
await page.locator('#list li.vrow').nth(1).click();
await idle();
check('a note in the properties panel has no URL or link-check fields', !(await page.isVisible('#f-url')) && !(await page.isVisible('#f-check')) && (await page.isVisible('#f-notes')));

answer('Projects');
await item('Work').click({ button: 'right' });
await page.click('#fctx [data-act=new]');
await idle();
check('New folder creates it inside the folder and selects it', JSON.stringify((await calls('create_folder')).at(-1)) === '{"parent":2,"name":"Projects"}' && (await page.textContent('#tree li[aria-selected=true] .name')) === 'Projects');
check('its parent opens to show it', (await tree()).includes('Projects'));

answer('Side projects');
await page.focus('#tree');
await page.keyboard.press('F2');
await idle();
check('F2 renames', (await tree()).includes('Side projects'), JSON.stringify(await tree()));

await item('Work').click();
await idle();
answer();
await page.keyboard.press('Delete');
await idle();
check('Del deletes the folder after asking; its contents move up', JSON.stringify((await calls('delete_folder')).at(-1)) === '{"id":2}'
  && (await page.textContent('#tree li[aria-selected=true] .name')) === 'Bookmarks'
  && (await titles()).includes('Wiki') && (await titles()).includes('Root page'), JSON.stringify(await titles()));
check('the moved-up folders appear under the parent', (await tree()).includes('Clients'));

await item('Home').click();
await idle();
answer('Shopping');
await item('Home').click({ button: 'right' });
await page.click('#fctx [data-act=note]');
await idle();
check('New note adds a note to the folder and selects it', JSON.stringify(await titles()) === '["✎ Shopping"]' && (await page.inputValue('#f-title')) === 'Shopping');

await page.reload();
await idle();
check('the selected folder is remembered', (await page.textContent('#tree li[aria-selected=true] .name')) === 'Home');

await page.click('#kw-head');
check('the Keywords pane collapses', !(await page.isVisible('#kwlist')) && (await page.getAttribute('#kw-head', 'aria-expanded')) === 'false');

await browser.close();
process.stdout.write(failed ? `${failed} failed\n` : 'all passed\n');
process.exit(failed ? 1 : 0);
