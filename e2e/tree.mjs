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
  window.__setStatus = (title, st) => { const b = items.find((x) => x.title === title); b.check_status = st; };
  window.__addItem = (folderId, title, url) => items.push(bm(nextId++, folderId, 99, title, url));
  let nextId = 100;
  const byPos = (a, b) => a.position - b.position;
  // A one-stack undo like the store's: each filing command saves the whole fake state.
  const steps = [];
  const snap = (name) => steps.push([name, JSON.stringify({ folders, items })]);
  const handlers = {
    folders: () => [...folders].sort((a, b) => (a.parent_id ?? 0) - (b.parent_id ?? 0) || byPos(a, b)).map((f) => ({ ...f })),
    search_ids: ({ query, opts }) => {
      if (!query.trim()) return items.filter((b) => b.folder_id === opts.folder).sort(byPos).map((b) => b.id);
      // Scoped (FIND-08): the folder and everything below it.
      const below = (id) => id === opts.folder || (id !== 1 && below(folders.find((f) => f.id === id).parent_id));
      return items.filter((b) => b.title.toLowerCase().includes(query.trim().toLowerCase()) && (!opts.in_folder || below(b.folder_id))).map((b) => b.id);
    },
    get_bookmarks: ({ ids }) => ids.map((id) => items.find((b) => b.id === id)).filter(Boolean),
    count: () => items.length,
    keywords: () => [],
    data_version: () => 1,
    history: () => ({ undo: steps.at(-1)?.[0] ?? null, redo: null }),
    undo: () => {
      const step = steps.pop();
      if (!step) return null;
      const old = JSON.parse(step[1]);
      folders.splice(0, folders.length, ...old.folders);
      items.splice(0, items.length, ...old.items);
      return step[0];
    },
    move_items: ({ folders: fs, bookmarks: bs, to, beside }) => {
      snap('Move');
      const moved = [...fs.map((id) => folders.find((x) => x.id === id)), ...bs.map((id) => items.find((x) => x.id === id))];
      const kids = [...folders.filter((f) => f.parent_id === to), ...items.filter((b) => b.folder_id === to)].sort(byPos).filter((k) => !moved.includes(k));
      const anchor = beside ? kids.findIndex((k) => k.id === beside.id && ('folded' in k) === beside.folder) : -1;
      const at = anchor >= 0 ? anchor + (beside.after ? 1 : 0) : kids.length;
      kids.splice(at, 0, ...moved);
      kids.forEach((k, i) => { k.position = i + 1; if ('folded' in k) k.parent_id = to; else k.folder_id = to; });
      return moved.length;
    },
    saved_items: () => [],
    schedule_names: () => [],
    set_folder_folded: ({ id, folded }) => { folders.find((f) => f.id === id).folded = folded; },
    create_folder: ({ parent, name }) => {
      snap(`New folder “${name.trim()}”`);
      const id = nextId++;
      folders.push({ id, parent_id: parent, name: name.trim(), position: 99, folded: true });
      return id;
    },
    rename_folder: ({ id, name }) => { snap('Rename'); folders.find((f) => f.id === id).name = name.trim(); },
    delete_folder: ({ id }) => {
      snap('Delete');
      const f = folders.find((x) => x.id === id);
      for (const c of folders) if (c.parent_id === id) c.parent_id = f.parent_id;
      for (const b of items) if (b.folder_id === id) b.folder_id = f.parent_id;
      folders.splice(folders.indexOf(f), 1);
    },
    get_settings: () => ({ folder_keywords: true, autostart: false }),
    browser_sources: () => [{ browser: 'Chrome', profile: 'Default', count: 2 }],
    import_browser: ({ newFolder }) => {
      snap('Import from Chrome (Default)');
      const id = nextId++;
      if (newFolder) folders.push({ id, parent_id: 1, name: 'Chrome (Default)', position: 99, folded: true });
      items.push(bm(nextId++, newFolder ? id : 1, 1, 'Imported', 'https://imported.example/'));
      const added = nextId - 1;
      return { added: 1, updated: 1, skipped: 1, ids: [added], folders: newFolder ? 1 : 0, folder: newFolder ? id : null, items: [
        { outcome: 'added', id: added, url: 'https://imported.example/', title: 'Imported' },
        { outcome: 'updated', id: 11, url: 'http://wiki.example', title: 'Wiki' },
        { outcome: 'skipped', id: 13, url: 'https://www.tracker.example/', title: 'Tracker' }] };
    },
    export_file: ({ format }) => [3, `keymarks-bookmarks.${format}`],
    // The New Item Folder (ITEM-04): one at most; inbox adds make a root "New" if it's gone.
    set_new_item_folder: ({ id }) => { snap('Set New Item Folder'); for (const f of folders) f.new_items = f.id === id; return true; },
    add_bookmark: ({ url, title, folder }) => {
      const id = nextId++;
      items.push(bm(id, folder, 99, title || url, url));
      return { added: 1, updated: 0, skipped: 0, ids: [id], folders: 0, folder: null, items: [{ outcome: 'added', id, url, title }] };
    },
    add_from_clipboard: () => {
      let inbox = folders.find((f) => f.new_items);
      if (!inbox) folders.push(inbox = { id: nextId++, parent_id: 1, name: 'New', position: 99, folded: true, new_items: true });
      const id = nextId++;
      items.push(bm(id, inbox.id, 99, 'Clipped', 'https://clipped.example/'));
      return { added: 1, updated: 0, skipped: 0, ids: [id], folders: 0, folder: null, items: [{ outcome: 'added', id, url: 'https://clipped.example/', title: 'Clipped' }] };
    },
    // The Duplicates view (SAVD-10): a rough url_key; the store's own is unit-tested.
    duplicate_groups: () => {
      const key = (u) => u.trim().replace(/^[a-z]+:\/\//i, '').replace(/^www\./i, '').replace(/\/$/, '').toLowerCase();
      const groups = new Map();
      for (const b of items) if (b.kind === 'bookmark' && b.url) groups.set(key(b.url), [...(groups.get(key(b.url)) ?? []), b.id]);
      return [...groups.values()].filter((g) => g.length > 1);
    },
    merge_duplicates: ({ keep }) => {
      snap('Merge duplicates');
      const group = handlers.duplicate_groups().find((g) => g.includes(keep)) ?? [];
      const gone = group.filter((id) => id !== keep);
      for (const id of gone) items.splice(items.findIndex((b) => b.id === id), 1);
      return gone.length;
    },
    dead_links: () => items.filter((b) => ['not_found', 'forbidden'].includes(b.check_status) && b.check_enabled).map((b) => b.id),
    sort_folder: ({ id, by, descending, foldersAt, recursive }) => {
      snap('Sort folder');
      const kids = [...folders.filter((f) => f.parent_id === id), ...items.filter((b) => b.folder_id === id)].sort(byPos);
      const name = (k) => (k.name ?? k.title).toLowerCase();
      kids.sort((a, b) => (foldersAt === 'first' ? ('folded' in b) - ('folded' in a) : 0) || (descending ? -1 : 1) * name(a).localeCompare(name(b)));
      kids.forEach((k, i) => { k.position = i + 1; });
      window.__sorted = { id, by, descending, foldersAt, recursive };
      return 1;
    },
    set_folder_note: ({ id, note }) => { snap('Note on folder'); folders.find((f) => f.id === id).note = note.trim(); },
    // Smart selection (FLD-06): items in the folders and below, once each (the store's own is unit-tested).
    selection_bookmarks: ({ folders: fs }) => {
      const under = (id, of) => id === of || (id != null && id !== 1 && under(folders.find((f) => f.id === id)?.parent_id, of));
      return items.filter((b) => fs.some((f) => under(b.folder_id, f))).map((b) => b.id);
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

await item('Work').click({ button: 'right' });
await page.click('#fctx [data-act=new]');
check('New folder asks for the name in place, in the tree', await page.isVisible('#tree li.naming input:focus'));
await page.keyboard.type('Projects');
await page.keyboard.press('Enter');
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

// ---- filing: Move to folder, drag and drop, undo (the reload reset the fake) ----
const box = async (loc) => loc.boundingBox();
const drag = async (from, to, dy = 0.5) => {
  const a = await box(from);
  const b = await box(to);
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 2, b.y + b.height * dy, { steps: 6 });
  await page.mouse.up();
  await idle();
};
const row = (n) => page.locator('#list li.vrow').nth(n);
const lastMove = async () => JSON.stringify((await calls('move_items')).at(-1));
const moves = async () => (await calls('move_items')).length;

await item('Work').click();
await idle();
await drag(row(2), row(0), 0.25);
check('dragging a row above another reorders the folder', await lastMove() === '{"folders":[],"bookmarks":[13],"to":2,"beside":{"folder":false,"id":11,"after":false}}'
  && JSON.stringify(await titles()) === '["Tracker","Wiki","✎ Standup notes"]', `${await lastMove()} ${JSON.stringify(await titles())}`);
await page.focus('#list');
await page.keyboard.press('Control+z');
await idle();
check('Ctrl+Z undoes the move and the list shows the old order', JSON.stringify(await titles()) === '["Wiki","✎ Standup notes","Tracker"]' && (await status()).startsWith('Undid: Move'), `${JSON.stringify(await titles())} ${await status()}`);

await drag(row(0), item('Home'));
check('dragging a row onto a tree folder moves it there', await lastMove() === '{"folders":[],"bookmarks":[11],"to":4,"beside":null}'
  && JSON.stringify(await titles()) === '["✎ Standup notes","Tracker"]', `${await lastMove()} ${JSON.stringify(await titles())}`);

await row(0).click();
await row(1).click({ modifiers: ['Control'] });
await idle();
await page.focus('#list');
await page.keyboard.press('Control+m');
check('Ctrl+M opens the folder picker for the selection', await page.isVisible('#move-dlg') && (await page.textContent('#move-title')) === 'Move 2 bookmarks to', await page.textContent('#move-title'));
await page.keyboard.type('cli');
const offered = await page.$$eval('#mv-list li', (lis) => lis.map((li) => li.textContent));
check('the picker filters folders as you type', JSON.stringify(offered) === '["Bookmarks › Work › Clients"]', JSON.stringify(offered));
await page.keyboard.press('Enter');
await idle();
check('Enter moves both in one call', await lastMove() === '{"folders":[],"bookmarks":[12,13],"to":3,"beside":null}' && (await titles()).length === 0, await lastMove());

await page.reload();
await idle();
const before = await moves();
await drag(item('Home'), item('Work'));
check('dragging a folder onto another moves it inside, and shows it there', await lastMove() === '{"folders":[4],"bookmarks":[],"to":2,"beside":null}'
  && JSON.stringify(await tree()) === '["Bookmarks","Work","Clients","Home"]', `${await lastMove()} ${JSON.stringify(await tree())}`);
await drag(item('Work'), item('Clients'));
check('a folder can’t be dropped into its own subtree', (await moves()) === before + 1);
await drag(item('Home'), item('Work'), 0.1);
check('dropping near a folder’s top edge puts the folder before it', await lastMove() === '{"folders":[4],"bookmarks":[],"to":1,"beside":{"folder":true,"id":2,"after":false}}'
  && JSON.stringify(await tree()) === '["Bookmarks","Home","Work","Clients"]', `${await lastMove()} ${JSON.stringify(await tree())}`);

await item('Home').click();
await page.keyboard.press('Control+m');
const forFolder = await page.$$eval('#mv-list li', (lis) => lis.map((li) => li.textContent));
check('the picker leaves out the folder itself for a folder move', !forFolder.some((p) => p.includes('Home')), JSON.stringify(forFolder));
await page.keyboard.press('Escape');

await page.focus('#tree');
const creates = (await calls('create_folder')).length;
await page.keyboard.press('Control+Shift+N');
await page.keyboard.press('Escape');
await idle();
check('Esc while naming cancels the new folder', (await calls('create_folder')).length === creates && !(await page.isVisible('#tree li.naming')));
await page.keyboard.press('Control+Shift+N');
await page.keyboard.type('Recipes');
await page.keyboard.press('Enter');
await idle();
check('Ctrl+Shift+N creates a folder inside the selected one', JSON.stringify((await calls('create_folder')).at(-1)) === '{"parent":4,"name":"Recipes"}' && (await tree()).includes('Recipes'), JSON.stringify(await tree()));
await page.keyboard.press('Control+z');
await idle();
check('undoing the new folder takes it out of the tree', !(await tree()).includes('Recipes') && (await page.textContent('#tree li[aria-selected=true] .name')) === 'Bookmarks', JSON.stringify(await tree()));

// ---- import: the tree options are asked before the import runs (XFER-05, KEY-09, Q4) ----
await page.evaluate(() => document.getElementById('import').click());
await page.waitForSelector('#import-sources button');
check('the import dialog pre-fills "folder names as keywords" from Options', await page.isChecked('#import-folder-words'));
check('a new folder named after the source is the default destination', await page.isChecked('input[name=import-into][value=new]'));
await page.uncheck('#import-folder-words');
await page.click('#import-sources button');
await idle();
const imp = (await calls('import_browser')).at(-1);
check('the import gets the choices made in the dialog', imp && imp.folderWords === false && imp.newFolder === true && imp.browser === 'Chrome', JSON.stringify(imp));
check('after the import the new folder is in the tree and selected, its items listed',
  (await page.textContent('#tree li[aria-selected=true] .name')) === 'Chrome (Default)' && JSON.stringify(await titles()) === '["Imported"]', `${JSON.stringify(await tree())} ${JSON.stringify(await titles())}`);
check('the status bar reports added / updated / skipped (XFER-02)', /Merge from Chrome: 1 added in 1 folder \/ 1 updated \/ 1 skipped/.test(await status()), await status());
const report = await page.evaluate(() => ({
  open: document.getElementById('report-dlg').open,
  lists: [...document.querySelectorAll('#report-lists details')].map((d) => [d.querySelector('summary').textContent, d.querySelectorAll('li').length]),
}));
check('the merge report opens with an expandable list per outcome', report.open && JSON.stringify(report.lists.map(([t, n]) => [t.split(':')[0].split(' (')[0], n])) === '[["Added",1],["Updated",1],["Skipped",1]]', JSON.stringify(report));
await page.click('#report-dlg button[value=close]');
await page.focus('#list');
await page.keyboard.press('Control+z');
await idle();
check('Ctrl+Z undoes the whole import', !(await tree()).includes('Chrome (Default)') && (await status()).startsWith('Undid: Import from Chrome'), `${JSON.stringify(await tree())} ${await status()}`);

// ---- export: range "this folder" sends the folder, not ids (XFER-16, XFER-21) ----
await item('Work').click();
await idle();
await page.evaluate(() => document.getElementById('export-open').click());
await page.waitForSelector('#export-dlg[open]');
check('the export dialog offers the selected folder', (await page.textContent('#ex-folder')).includes('“Work”') && !(await page.isDisabled('input[name=ex-scope][value=folder]')));
await page.check('input[name=ex-scope][value=folder]');
await page.click('#ex-go');
await idle();
const ex = (await calls('export_file')).at(-1);
check('a folder export asks for that folder\'s tree', ex && ex.folder === 2 && ex.ids === null && ex.format === 'html', JSON.stringify(ex));
check('the status bar reports the export', /Exported 3 bookmarks to keymarks-bookmarks.html/.test(await status()), await status());
await item('Bookmarks').click();
await idle();
await page.evaluate(() => document.getElementById('export-open').click());
check('at the root "this folder" is off (it would be All)', await page.isDisabled('input[name=ex-scope][value=folder]'));
await page.click('#export-dlg button[data-close]');

// ---- search meets the tree: scope, Folder column, reveal (FIND-07, FIND-08, VIEW-02, FLD-14) ----
check('exact mode offers the Folder field, on by default', await page.isChecked('input[name=field][value=folder]'));
await item('Clients').click();
await idle();
const scope = () => page.evaluate(() => ({ pressed: document.getElementById('scope').ariaPressed, text: document.getElementById('scope').textContent }));
check('the scope starts as the whole collection', (await scope()).pressed === 'false' && /everywhere/i.test((await scope()).text), JSON.stringify(await scope()));
await page.fill('#q', 'wiki');
await idle();
check('an unscoped search finds both wikis', (await titles()).length === 2);
await page.focus('#q');
await page.keyboard.press('Control+Shift+F');
await idle();
check('Ctrl+Shift+F scopes the search to the folder, and says so', (await scope()).pressed === 'true' && (await scope()).text.includes('Clients'), JSON.stringify(await scope()));
check('the scoped search asks for the folder and below', (await calls('search_ids')).at(-1).opts.in_folder === true && (await calls('search_ids')).at(-1).opts.folder === 3);
check('the scoped search lists only what is in the folder', JSON.stringify(await titles()) === '["Client wiki"]', JSON.stringify(await titles()));
check('the status bar names the scope', /^1 matching in “Clients” and below/.test(await status()), await status());
check('the scope is kept with the search options', await page.evaluate(() => JSON.parse(localStorage.getItem('searchOptions')).in_folder === true));
await item('Work').click();
await idle();
check('while scoped, choosing a folder keeps the search and searches there', (await page.inputValue('#q')) === 'wiki' && (await titles()).length === 2 && (await scope()).text.includes('Work'), `${await page.inputValue('#q')} ${JSON.stringify(await titles())}`);
await page.click('#scope');
await idle();
check('clicking the scope goes back to everywhere', (await scope()).pressed === 'false' && (await calls('search_ids')).at(-1).opts.in_folder === false);

await page.click('#view-table');
await page.click('#cols summary');
await page.check('#cols-menu input[value=folder]');
await idle();
const folderCells = await page.$$eval('#table-body tr.vrow', (trs) => trs.map((tr) => tr.lastElementChild.textContent));
check('the Folder column shows each result\'s folder path', folderCells.includes('Bookmarks › Work › Clients') && folderCells.includes('Bookmarks › Work'), JSON.stringify(folderCells));
await page.click('#view-cards');

await item('Work').locator('.twisty').click();
check('(Work collapsed again)', !(await tree()).includes('Clients'));
await page.fill('#q', 'client');
await idle();
await page.locator('#list li.vrow').first().click({ button: 'right' });
await page.click('#bctx [data-act=reveal]');
await idle();
check('Show in folder opens the folders above it and selects its folder', (await tree()).includes('Clients') && (await page.textContent('#tree li[aria-selected=true] .name')) === 'Clients', JSON.stringify(await tree()));
check('Show in folder clears the search and highlights the item in its folder',
  (await page.inputValue('#q')) === '' && (await page.textContent('#list li.vrow[aria-selected=true] .title')) === 'Client wiki', await page.inputValue('#q'));
await page.fill('#q', 'root');
await idle();
await page.focus('#list');
await page.keyboard.press('Control+Shift+L');
await idle();
check('Ctrl+Shift+L shows the selected result in its folder', (await page.textContent('#tree li[aria-selected=true] .name')) === 'Bookmarks' && (await page.inputValue('#q')) === '');

// ---- New Item Folder (ITEM-04, ITEM-01, ITEM-06) ----
const inboxMarked = () => page.$$eval('#tree li.inbox .name', (els) => els.map((e) => e.textContent));
check('no folder is marked as the New Item Folder before one is set', (await inboxMarked()).length === 0);
await item('Home').click({ button: 'right' });
check('the folder menu offers Set as New Item Folder', !(await page.isDisabled('#fctx [data-act=inbox]')));
await page.click('#fctx [data-act=inbox]');
await idle();
check('Set as New Item Folder asks the store for it', JSON.stringify((await calls('set_new_item_folder')).at(-1)) === '{"id":4}');
check('the tree marks the New Item Folder', JSON.stringify(await inboxMarked()) === '["Home"]', JSON.stringify(await inboxMarked()));
check('it is one undo step', (await page.getAttribute('#undo', 'title')).includes('Set New Item Folder'), await page.getAttribute('#undo', 'title'));
await item('Home').click({ button: 'right' });
check('the menu item is off for the folder that already is it', await page.isDisabled('#fctx [data-act=inbox]'));
await page.keyboard.press('Escape');
await item('Work').click();
await idle();
await page.focus('#list');
await page.keyboard.press('Insert');
await page.fill('#n-url', 'https://here.example/');
await page.click('#new-form button[type=submit]');
await idle();
check('a bookmark added in the app goes to the selected folder, not the inbox', (await calls('add_bookmark')).at(-1)?.folder === 2, JSON.stringify((await calls('add_bookmark')).at(-1)));
// The inbox deleted: the next clipboard add recreates it as "New" and the tree shows it.
await item('Home').click();
await idle();
answer();
await page.focus('#tree');
await page.keyboard.press('Delete');
await idle();
check('the inbox folder can be deleted', !(await tree()).includes('Home'), JSON.stringify(await tree()));
await page.focus('#list');
await page.keyboard.press('Alt+Insert');
await idle();
check('a clipboard add after the inbox was deleted shows the recreated "New" folder, marked', JSON.stringify(await inboxMarked()) === '["New"]', JSON.stringify(await tree()));

// ---- Duplicate URLs view (SAVD-10): Ctrl+D lists the groups; Merge resolves the focused one's ----
await page.evaluate(() => window.__addItem(1, 'Wiki copy', 'http://www.Wiki.example'));
await page.focus('#list');
await page.keyboard.press('Control+d');
await idle();
check('Ctrl+D shows the duplicate group, the bar and "Duplicate URLs [1]"', JSON.stringify(await titles()) === '["Wiki","Wiki copy"]' && !(await page.isHidden('#dup-bar'))
  && (await page.textContent('#dup-view')).includes('Duplicate URLs [1]'), `${JSON.stringify(await titles())} ${await page.textContent('#dup-view')}`);
await page.click('#dup-merge');
await idle();
const merged = (await calls('merge_duplicates')).at(-1);
check('Merge merges into the focused bookmark, one undo step', merged?.keep === 11 && merged.merge === true && (await titles()).length === 0
  && (await page.getAttribute('#undo', 'title')).includes('Merge duplicates'), `${JSON.stringify(merged)} ${JSON.stringify(await titles())}`);
await page.keyboard.press('Control+z');
await idle();
check('Ctrl+Z brings the copy back into the view', JSON.stringify(await titles()) === '["Wiki","Wiki copy"]', JSON.stringify(await titles()));
await page.focus('#list');
await page.keyboard.press('Control+d');
await idle();
check('Ctrl+D again leaves the view', await page.isHidden('#dup-bar') && !(await page.textContent('#status')).startsWith('Duplicate URLs'), await page.textContent('#status'));

// ---- Folder notes (NOTE-02): the tree's folder in the panel, saved on blur, undoable ----
await page.click('#tree li:has-text("Work") .name');
await idle();
check('a folder chosen in the tree shows its title and notes box in the panel', (await page.textContent('#fn-title')) === 'Work' && (await page.inputValue('#fn-note')) === '',
  await page.textContent('#panel'));
await page.fill('#fn-note', 'Client work and the wiki ');
await page.focus('#list');
await idle();
const noted = (await calls('set_folder_note')).at(-1);
check('leaving the notes box saves the folder note, one undo step', noted?.id === 2 && noted.note === 'Client work and the wiki '
  && (await page.getAttribute('#undo', 'title')).includes('Note on folder'), JSON.stringify(noted));
check('focusing the list shows the bookmark again', (await page.$('#form')) !== null && (await page.$('#fn-note')) === null);
await page.click('#tree li:has-text("Work") .name');
await idle();
check('the saved note shows when the folder is chosen again', (await page.inputValue('#fn-note')) === 'Client work and the wiki', await page.inputValue('#fn-note'));
await page.keyboard.press('Control+z');
await idle();
check('Ctrl+Z takes the note back', (await page.inputValue('#fn-note')) === '', await page.inputValue('#fn-note'));

// ---- Sort folder (ORD-01): folder menu → dialog → one store call, one undo step ----
await page.click('#tree li:has-text("Work") .name', { button: 'right' });
await page.click('#fctx [data-act=sort]');
await page.selectOption('#so-dir', 'down');
await page.click('#sort-form button[type=submit]');
await idle();
const sorted = await page.evaluate(() => window.__sorted);
check('Sort folder sends the folder, key, direction, folders first and sub-folders', JSON.stringify(sorted) === '{"id":2,"by":"title","descending":true,"foldersAt":"first","recursive":true}'
  && JSON.stringify((await titles()).slice(0, 3)) === '["Wiki","Tracker","✎ Standup notes"]', `${JSON.stringify(sorted)} ${JSON.stringify(await titles())}`);
check('the sort is an undo step', (await page.getAttribute('#undo', 'title')).includes('Sort folder'), await page.getAttribute('#undo', 'title'));

// ---- Dead Links view (SAVD-09): Ctrl+L lists dead links across folders; Esc returns ----
await page.evaluate(() => { window.__addItem(4, 'Gone page', 'https://gone.example/'); });
await page.evaluate(() => window.__setStatus('Gone page', 'not_found'));
await page.focus('#list');
await page.keyboard.press('Control+l');
await idle();
check('Ctrl+L shows the dead links from every folder, "Dead Links [1]"', JSON.stringify(await titles()) === '["✗ Gone page"]' && (await page.textContent('#dead-view')).includes('Dead Links [1]')
  && (await page.textContent('#status')).startsWith('Dead Links'), `${JSON.stringify(await titles())} ${await page.textContent('#dead-view')}`);
await page.focus('#list');
await page.keyboard.press('Escape');
await idle();
check('Esc leaves the Dead Links view', !(await page.textContent('#status')).startsWith('Dead Links'), await page.textContent('#status'));

// ---- Smart selection in the tree (FLD-06) and folder menu Open all (VIEW-16) ----
await item('Bookmarks').click();
await idle();
await item('Work').click();
await idle();
if (!(await tree()).includes('Clients')) await item('Work').locator('.twisty').click();
await item('New').click({ modifiers: ['Control'] });
const newId = Number(await item('New').getAttribute('data-id'));
const chosen = () => page.$$eval('#tree li[aria-selected=true] .name', (els) => els.map((e) => e.textContent));
check('Ctrl+click adds a folder to the tree selection; the list keeps the folder', JSON.stringify(await chosen()) === '["Work","New"]'
  && (await titles()).includes('Wiki'), `${JSON.stringify(await chosen())} ${JSON.stringify(await titles())}`);
await item('New').click({ button: 'right' });
check('the folder menu names the selection and hides single-folder commands', (await page.textContent('#fctx-title')) === '2 folders'
  && await page.isHidden('#fctx [data-act=rename]') && await page.isVisible('#fctx [data-act=openall]'), await page.textContent('#fctx-title'));
await page.click('#fctx [data-act=openall]');
await idle();
const opened = (await calls('open_bookmark')).map((a) => a.id).sort();
const expect = await page.evaluate((id) => window.__TAURI__.core.invoke('selection_bookmarks', { folders: [2, id], bookmarks: [] }), newId);
const links = (await page.evaluate((ids) => window.__TAURI__.core.invoke('get_bookmarks', { ids }), expect)).filter((b) => b.kind !== 'note').map((b) => b.id).sort();
check('Open all asks for the bookmarks under both folders and opens each link once, notes skipped', JSON.stringify((await calls('selection_bookmarks'))[0]) === JSON.stringify({ folders: [2, newId], bookmarks: [] })
  && opened.length > 0 && JSON.stringify(opened) === JSON.stringify(links) && opened.includes(14), `${JSON.stringify(opened)} vs ${JSON.stringify(links)}`);
await page.focus('#tree');
await page.keyboard.press('Control+m');
check('Ctrl+M in the tree moves the whole selection', (await page.textContent('#move-title')) === 'Move 2 folders to', await page.textContent('#move-title'));
await page.keyboard.press('Escape');
await item('Bookmarks').click({ modifiers: ['Shift'] });
check('Shift+click selects the shown range from the selected folder', JSON.stringify(await chosen()) === '["Bookmarks","Work"]', JSON.stringify(await chosen()));
await item('New').click();
await idle();
check('a plain click goes back to one folder', JSON.stringify(await chosen()) === '["New"]', JSON.stringify(await chosen()));

await browser.close();
process.stdout.write(failed ? `${failed} failed\n` : 'all passed\n');
process.exit(failed ? 1 : 0);
