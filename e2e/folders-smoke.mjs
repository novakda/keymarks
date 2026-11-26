// The folder features in the real app (the folders test plan, 21 checks), driven through the
// WebView2 debugging port with real key and pointer events. It runs on a COPY of the user's
// last pre-folders database (bookmarks.db.v5.bak, else bookmarks.pre-folders.db, beside the
// real one), so the upgrade is tested on real data; the original is never opened. The page's
// prompt() is answered through CDP (Page.javascriptDialogOpening), which also proves it opens.
// confirm() is tauri-plugin-dialog's async native message box, which CDP can't answer: it is
// replaced by an async fake the test answers, so a caller that doesn't await it is caught. The native open/save file dialogs are the OS's: KEYMARKS_E2E_OPEN_FILE /
// KEYMARKS_E2E_SAVE_FILE stand in for them. The database is read (read-only) to check what
// the store did. Output is counts and the test's own folder names only.
//
//   npm run folders          (from e2e/, on Windows; closes and reopens the running app)

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { connect, debugEnv, exe, freePort, log, sleep, startApp, stopApp } from './launch.mjs';
import { restoreKeymarks, runningKeymarks } from './running.mjs';

const PORT = await freePort();
const usersKeymarks = runningKeymarks();
stopApp();

const realData = path.join(process.env.APPDATA, 'com.pattern158.keymarks');
const source = ['bookmarks.db.v5.bak', 'bookmarks.pre-folders.db'].map((f) => path.join(realData, f)).find((f) => fs.existsSync(f));
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'km-fld-e2e-'));
const webviewData = fs.mkdtempSync(path.join(os.tmpdir(), 'km-fld-e2e-webview-'));
const dbPath = path.join(work, 'bookmarks.db');
const importFile = path.join(work, 'kmzq-import.html');
const exportFile = path.join(work, 'export.html');

// The copy holds the user's private bookmarks: it must not be left behind.
function cleanupWork() {
  for (const dir of [webviewData, work]) {
    try { fs.rmSync(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 }); } catch (e) { log(`could not delete ${dir}: ${e.message}`); }
  }
  if (fs.existsSync(work)) log(`WARNING: temporary copy still exists: ${work}`);
}
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
setTimeout(() => { log('timed out after 5 minutes'); emergencyExit('timeout'); }, 300_000).unref();
if (!source) emergencyExit(`no pre-folders database (bookmarks.db.v5.bak or bookmarks.pre-folders.db) in ${realData}`);
fs.copyFileSync(source, dbPath);

// Read-only, a fresh connection per question: always the store's latest commit.
function sql(query, ...params) {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try { return db.prepare(query).all(...params); } finally { db.close(); }
}
const userVersion = (file) => {
  const db = new DatabaseSync(file, { readOnly: true });
  try { return db.prepare('PRAGMA user_version').get().user_version; } finally { db.close(); }
};
const before = { version: userVersion(dbPath), count: sql('SELECT count(*) AS n FROM bookmark')[0].n };
const folderRow = (name) => sql('SELECT * FROM folder WHERE name = ?', name)[0];
const itemsIn = (id) => sql('SELECT id FROM bookmark WHERE folder_id = ? ORDER BY position', id).map((r) => r.id);

// A Netscape file with nested folders (unique URLs, so none is skipped as a duplicate).
const link = (n) => `<DT><A HREF="https://kmzq-e2e-import.example/${n}">Kmzq import ${n}</A>`;
fs.writeFileSync(importFile, `<!DOCTYPE NETSCAPE-Bookmark-file-1>
<TITLE>Bookmarks</TITLE><H1>Bookmarks</H1>
<DL><p>
  <DT><H3>Kmzq Top</H3>
  <DL><p>
    ${link(1)}
    <DT><H3>Kmzq Sub</H3>
    <DL><p>
      ${link(2)}
      <DT><H3>Kmzq Deep</H3>
      <DL><p>${link(3)}${link(4)}</DL><p>
    </DL><p>
  </DL><p>
  ${link(5)}
</DL><p>
`);

const env = { ...debugEnv(PORT, webviewData), KEYMARKS_DATA_DIR: work, KEYMARKS_E2E_OPEN_FILE: importFile, KEYMARKS_E2E_SAVE_FILE: exportFile };

// confirm() answers queued by the test (true/false, or 'hold': answer later through
// window.__e2eAnswer). Async like the plugin's, so `!confirm()` never waits for it.
const fakeConfirm = () => {
  window.__e2eConfirms = [];
  window.confirm = () => new Promise((resolve) => {
    const a = window.__e2eConfirms.shift();
    if (a === 'hold') window.__e2eAnswer = resolve;
    else { if (a === undefined) console.warn('E2E unexpected confirm'); resolve(a === true); }
  });
};
const expectConfirm = (a) => page.evaluate((x) => window.__e2eConfirms.push(x), a);
// prompt() answers queued by the test; anything else is dismissed and logged.
const answers = [];
const dialogsSeen = [];
let browser;
let page;
async function launch() {
  startApp(env);
  ({ browser, page } = await connect(PORT));
  await page.context().addInitScript(fakeConfirm); // survives reloads
  await page.evaluate(fakeConfirm);
  page.on('console', (m) => { if (m.text().startsWith('E2E unexpected')) log(m.text()); });
  page.on('dialog', async (d) => {
    dialogsSeen.push({ type: d.type(), message: d.message(), value: d.defaultValue() });
    const a = answers.shift();
    if (a === undefined) { log(`unexpected ${d.type()} dialog`); await d.dismiss(); } else if (a === false) await d.dismiss(); else await d.accept(a === true ? undefined : a);
  });
  await page.waitForSelector('#status');
  await idle();
}
async function restart() {
  await browser.close().catch(() => {});
  stopApp();
  await launch();
}
const idle = () => page.waitForFunction(() => document.body.ariaBusy === 'false', null, { timeout: 15000 });
const status = () => page.textContent('#status');
const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok: !!ok, detail });
  log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
};

// Page state (app.js is a classic script: its top-level bindings are reachable).
const shown = () => page.evaluate(() => ({ folder, ids: [...ids], selected: [...selected], cursor }));
const treeSelected = async () => Number(await page.getAttribute('#tree li[aria-selected=true]', 'data-id'));
const treeLi = (id) => `#tree li[data-id="${id}"]`;
async function openFolder(id) {
  await page.click(treeLi(id));
  await idle();
}
async function key(combo, focus = '#tablewrap') {
  if (focus) await page.focus(focus);
  await page.keyboard.press(combo);
  await idle();
}
const row = (n) => `#table-body tr.vrow:nth-child(${n})`;
async function search(text) {
  await page.fill('#q', text);
  await idle();
}
async function menu(id) {
  await page.click('#more summary');
  await page.click(`#${id}`);
}
async function newFolderHere(name) {
  await key('Control+Shift+N', '#tree');
  await page.waitForSelector('#tree li.naming input');
  const focused = await page.evaluate(() => document.activeElement.closest('li')?.classList.contains('naming'));
  await page.keyboard.type(name);
  await page.keyboard.press('Enter');
  await idle();
  return focused;
}
async function moveSelectionTo(name) {
  await key('Control+m');
  await page.waitForSelector('#move-dlg[open]');
  await page.keyboard.type(name);
  await page.keyboard.press('Enter');
  await idle();
}
const center = async (sel) => { const b = await page.locator(sel).boundingBox(); return { x: b.x + b.width / 2, y: b.y + b.height / 2 }; };
async function drag(fromSel, toSel, dy = 0) {
  const a = await center(fromSel);
  const b = await center(toSel);
  await page.mouse.move(a.x, a.y);
  await page.mouse.down();
  await page.mouse.move(b.x, b.y + dy, { steps: 8 });
  await page.mouse.up();
  await idle();
}

// The browser extension's side: native-messaging requests to keymarks-host.exe (beside the
// app) on the test's copy, framed as the browser frames them. Returns the replies.
function hostSend(...msgs) {
  const frame = (m) => { const b = Buffer.from(JSON.stringify(m)); const n = Buffer.alloc(4); n.writeUInt32LE(b.length); return Buffer.concat([n, b]); };
  const r = spawnSync(path.join(path.dirname(exe), 'keymarks-host.exe'), [], {
    input: Buffer.concat(msgs.map(frame)), env: { ...process.env, KEYMARKS_DATA_DIR: work }, windowsHide: true, timeout: 20000,
  });
  if (r.error) throw r.error;
  const out = [];
  for (let at = 0; at + 4 <= r.stdout.length;) { const n = r.stdout.readUInt32LE(at); out.push(JSON.parse(r.stdout.subarray(at + 4, at + 4 + n))); at += 4 + n; }
  return out;
}
const inboxRows = () => sql("SELECT id, name, parent_id FROM folder WHERE role = 'new_items' ORDER BY id");
const inboxMarked = () => page.$$eval('#tree li.inbox', (lis) => lis.map((li) => Number(li.dataset.id)));
// The app notices the host's writes by polling (1.5 s).
const waitInboxShown = (id) => page.waitForFunction((x) => !!document.querySelector(`#tree li.inbox[data-id="${x}"]`), id, { timeout: 10000 }).then(() => true, () => false);

// Netscape bookmark file → { links, folders: [{ name, links, folders }] }; throws if malformed.
function parseNetscape(text) {
  if (!text.startsWith('<!DOCTYPE NETSCAPE-Bookmark-file-1>')) throw new Error('no Netscape doctype');
  const unescape = (s) => s.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&');
  const top = { name: null, links: 0, folders: [] };
  const stack = [];
  let pending = null; // an H3 waiting for its <DL>
  for (const [tok] of text.matchAll(/<DL>|<\/DL>|<H3[^>]*>[^<]*<\/H3>|<A [^>]*>/gi)) {
    if (/^<DL>/i.test(tok)) {
      if (!stack.length) { if (pending) throw new Error('H3 before the top list'); stack.push(top); continue; }
      if (!pending) throw new Error('a list without a folder heading');
      stack.at(-1).folders.push(pending);
      stack.push(pending);
      pending = null;
    } else if (/^<\/DL>/i.test(tok)) {
      if (pending || !stack.pop()) throw new Error('unbalanced </DL>');
    } else if (/^<H3/i.test(tok)) {
      if (pending || !stack.length) throw new Error('H3 without its list');
      pending = { name: unescape(tok.replace(/<[^>]*>/g, '')), links: 0, folders: [] };
    } else {
      if (!/HREF="[^"]+"/i.test(tok) || !stack.length) throw new Error('a link without HREF, or outside the lists');
      stack.at(-1).links++;
    }
  }
  if (stack.length || pending) throw new Error('lists left open');
  return top;
}
// The same shape from the database: folders in position order, links = items with a URL.
function dbTree(id, name = null) {
  return {
    name,
    links: sql("SELECT count(*) AS n FROM bookmark WHERE folder_id = ? AND url <> ''", id)[0].n,
    folders: sql('SELECT id, name FROM folder WHERE parent_id = ? ORDER BY position, id', id).map((f) => dbTree(f.id, f.name)),
  };
}

try {
  log(`starting ${exe} on a copy of the pre-folders database (v${before.version}), debug port ${PORT}`);
  await launch().catch(emergencyExit);
  await page.evaluate(() => localStorage.clear()); // known defaults: root folder, no sort
  await page.reload();
  await idle();
  await page.click('#view-table');
  await idle();
  const ROOT = 1;

  // 1. Upgrade: everything in the root, schema v8, the old file kept beside it.
  const bak = `${dbPath}.v${before.version}.bak`;
  const after = sql("SELECT count(*) AS n, sum(folder_id <> 1) AS out, sum(kind <> 'bookmark') AS notes FROM bookmark")[0];
  const shownTotal = Number((await status()).match(/· (\d+) bookmarks/)?.[1]);
  check('1 upgrade: all bookmarks in the root, v8, with a .bak', before.version < 6 && userVersion(dbPath) === 8 && after.n === before.count
    && after.out === 0 && after.notes === 0 && sql('SELECT count(*) AS n FROM folder')[0].n === 1 && fs.existsSync(bak) && userVersion(bak) === before.version
    && shownTotal === before.count && (await page.textContent('#tree-head')) === 'Folders [0]', `v${before.version}→v${userVersion(dbPath)}, ${before.count} → ${after.n}, ${after.out} outside the root`);

  // 2. Ctrl+Shift+N: named in place.
  const inPlace = await newFolderHere('Kmzqalpha');
  const alpha = folderRow('Kmzqalpha');
  check('2 Ctrl+Shift+N makes a folder named in place', inPlace && alpha?.parent_id === ROOT && (await treeSelected()) === alpha.id
    && (await page.textContent(`${treeLi(alpha.id)} .name`)) === 'Kmzqalpha');

  // 7. Ctrl+M one item; Ctrl+Z restores it.
  await openFolder(ROOT);
  const rootOrder = itemsIn(ROOT);
  await page.click(row(1));
  await idle();
  const one = (await shown()).selected[0];
  await moveSelectionTo('Kmzqalpha');
  const movedOne = itemsIn(alpha.id).join() === String(one) && !itemsIn(ROOT).includes(one);
  await key('Control+z');
  check('7 Ctrl+M moves one item, Ctrl+Z restores it', movedOne && itemsIn(alpha.id).length === 0 && itemsIn(ROOT).join() === rootOrder.join());

  // 8. Ctrl+M several; one Ctrl+Z undoes all.
  await page.click(row(1));
  await page.click(row(2), { modifiers: ['Control'] });
  await page.click(row(3), { modifiers: ['Control'] });
  await idle();
  const three = (await shown()).selected;
  await moveSelectionTo('Kmzqalpha');
  const movedThree = itemsIn(alpha.id).length === 3 && three.every((id) => itemsIn(alpha.id).includes(id));
  await key('Control+z');
  check('8 Ctrl+M several, one Ctrl+Z undoes all', three.length === 3 && movedThree && itemsIn(alpha.id).length === 0 && itemsIn(ROOT).join() === rootOrder.join());

  // 9. Drag a row onto a tree folder; Ctrl+Z.
  await page.click(row(1));
  await idle();
  const dragged = (await shown()).selected[0];
  await drag(row(1), treeLi(alpha.id));
  const draggedIn = itemsIn(alpha.id).join() === String(dragged);
  const dragMsg = (await status()).startsWith('Moved');
  await key('Control+z');
  check('9 drag a row onto a tree folder, Ctrl+Z', draggedIn && dragMsg && itemsIn(alpha.id).length === 0 && itemsIn(ROOT).join() === rootOrder.join());

  // 11. Reorder rows (three in Alpha; the third dragged above the first).
  await page.click(row(1));
  await page.click(row(3), { modifiers: ['Shift'] });
  await idle();
  await moveSelectionTo('Kmzqalpha');
  await openFolder(alpha.id);
  const [a1, a2, a3] = (await shown()).ids;
  const top = await page.locator(row(1)).boundingBox();
  await drag(row(3), row(1), -top.height / 2 + 3);
  const reordered = (await shown()).ids.join() === [a3, a1, a2].join() && itemsIn(alpha.id).join() === [a3, a1, a2].join();

  // 10. Drag a folder into another; into its own subfolder is refused.
  await openFolder(ROOT);
  await newFolderHere('Kmzqbeta');
  const beta = folderRow('Kmzqbeta');
  await newFolderHere('Kmzqchild'); // inside Beta, now selected
  const child = folderRow('Kmzqchild');
  await drag(treeLi(beta.id), treeLi(alpha.id));
  const betaIn = folderRow('Kmzqbeta').parent_id === alpha.id;
  await page.waitForSelector(treeLi(child.id));
  await drag(treeLi(alpha.id), treeLi(child.id));
  check('10 drag a folder into another; into its own subfolder refused', child.parent_id === beta.id && betaIn && folderRow('Kmzqalpha').parent_id === ROOT
    && folderRow('Kmzqbeta').parent_id === alpha.id);

  // 5. Arrow keys move the tree selection; expand state survives a restart.
  await openFolder(ROOT);
  const walk = [];
  for (const k of ['ArrowDown', 'ArrowDown', 'ArrowLeft', 'ArrowUp']) {
    await key(k, '#tree');
    walk.push(await treeSelected());
  }
  const collapsed = folderRow('Kmzqbeta').folded === 1 && folderRow('Kmzqalpha').folded === 0 && !(await page.isVisible(treeLi(child.id)));
  const orderBefore = itemsIn(alpha.id).join();
  log('restarting the app');
  await restart();
  await page.click('#view-table');
  const expandKept = (await page.getAttribute(treeLi(alpha.id), 'aria-expanded')) === 'true' && (await page.getAttribute(treeLi(beta.id), 'aria-expanded')) === 'false'
    && !(await page.isVisible(treeLi(child.id)));
  check('5 arrow keys move the tree selection; expand state survives a restart', walk.join() === [alpha.id, beta.id, beta.id, alpha.id].join() && collapsed && expandKept, walk.join(' '));
  await openFolder(alpha.id);
  check('11 reorder rows; the order survives a restart', reordered && (await shown()).ids.join() === orderBefore && itemsIn(alpha.id).join() === orderBefore);

  // 3. F2 rename: the prompt opens with the name, the new one is saved.
  await openFolder(beta.id);
  dialogsSeen.length = 0;
  answers.push('Kmzqbeta renamed');
  await key('F2', '#tree');
  const d3 = dialogsSeen[0];
  check('3 F2 rename: the prompt renders and the name saves', d3?.type === 'prompt' && d3.message === 'Rename folder:' && d3.value === 'Kmzqbeta'
    && sql('SELECT name FROM folder WHERE id = ?', beta.id)[0].name === 'Kmzqbeta renamed' && (await page.textContent(`${treeLi(beta.id)} .name`)) === 'Kmzqbeta renamed', JSON.stringify(d3?.type));

  // 6. New note (folder menu): ✎ mark, text not URL, double-click opens nothing.
  await page.click(treeLi(alpha.id), { button: 'right' });
  answers.push('Kmzq note');
  await page.click('#fctx [data-act=note]');
  await idle();
  const note = sql("SELECT id, url, kind, visits FROM bookmark WHERE title = 'Kmzq note'")[0];
  const st = await shown();
  const noteRow = row(st.ids.indexOf(note?.id) + 1);
  const cells = await page.$$eval(`${noteRow} td`, (tds) => tds.map((td) => td.textContent));
  const headsNow = await page.$$eval('#table-head th', (ths) => ths.map((th) => th.dataset.col));
  await page.dblclick(noteRow);
  await idle();
  const noteAfter = sql('SELECT visits FROM bookmark WHERE id = ?', note?.id)[0];
  check('6 new note: ✎ mark, text not URL, double-click opens no browser', dialogsSeen.at(-1)?.type === 'prompt' && note?.kind === 'note' && note.url === ''
    && cells[headsNow.indexOf('title')]?.includes('✎ Kmzq note') && cells[headsNow.indexOf('url')] === '' && (await status()) === 'A note has no link to open.'
    && noteAfter.visits === note.visits);

  // 17. A partial folder name finds its items (Everywhere, from the root).
  await openFolder(ROOT);
  await search('kmzqal');
  const inAlpha = itemsIn(alpha.id);
  const found = (await shown()).ids;
  check('17 a partial folder name finds its items', inAlpha.length === 4 && found.length === inAlpha.length && inAlpha.every((id) => found.includes(id)), `${found.length} of ${inAlpha.length}`);

  // 18. Ctrl+Shift+F toggles Everywhere / In folder (and the results follow).
  await search('');
  await openFolder(alpha.id);
  await search('!zzzzqqq'); // matches everything
  const everywhere = (await shown()).ids.length;
  const label0 = await page.textContent('#scope');
  await key('Control+Shift+F', '#q');
  const label1 = await page.textContent('#scope');
  const scoped = (await shown()).ids.length;
  await key('Control+Shift+F', '#q');
  check('18 Ctrl+Shift+F toggles Everywhere / In folder', label0 === 'Everywhere' && label1 === 'In “Kmzqalpha”' && (await page.textContent('#scope')) === 'Everywhere'
    && scoped === 4 && everywhere === before.count + 1 && (await shown()).ids.length === everywhere, `${everywhere} / ${scoped}`);

  // 19. Show in folder (Ctrl+Shift+L): the folder is selected, the item highlighted.
  await openFolder(ROOT);
  await search('kmzqal');
  const target = a2;
  await page.click(row((await shown()).ids.indexOf(target) + 1));
  await idle();
  await key('Control+Shift+L');
  const s19 = await shown();
  const curRow = await page.$$eval('#table-body tr.vrow', (rows) => rows.findIndex((r) => r.classList.contains('cursor') && r.classList.contains('sel')));
  check('19 Show in folder selects the folder and highlights the item', (await treeSelected()) === alpha.id && (await page.inputValue('#q')) === ''
    && s19.selected.join() === String(target) && s19.ids[s19.cursor] === target && curRow === s19.cursor,
    `tree ${await treeSelected()} (want ${alpha.id}), ${s19.selected.length} selected, cursor ${s19.cursor}, row ${curRow}`);

  // 4. Del on a folder with items: it asks, and nothing happens until the answer (4b);
  // Cancel keeps the folder, OK moves its items up.
  const contents = itemsIn(alpha.id);
  await openFolder(alpha.id);
  await expectConfirm('hold');
  await page.focus('#tree');
  await page.keyboard.press('Delete');
  await sleep(700);
  const waited = !!folderRow('Kmzqalpha') && (await page.evaluate(() => typeof window.__e2eAnswer === 'function'));
  await page.evaluate(() => window.__e2eAnswer(false));
  await idle();
  check('4b Del asks and waits for the answer; Cancel keeps the folder', waited && !!folderRow('Kmzqalpha') && contents.every((id) => itemsIn(alpha.id).includes(id)),
    waited ? 'held until answered' : 'deleted before any answer');
  await expectConfirm(true);
  await key('Delete', '#tree');
  check('4 Del on a folder with items moves them up', !folderRow('Kmzqalpha') && folderRow('Kmzqbeta renamed')?.parent_id === ROOT
    && contents.every((id) => itemsIn(ROOT).includes(id)) && (await treeSelected()) === ROOT, `${contents.length} items moved up`);

  // 12. Import a Netscape file with nested folders: under a new folder named after the file.
  await menu('import');
  await page.waitForSelector('#import-dlg[open]');
  await page.waitForFunction(() => !document.getElementById('import-sources').textContent.includes('Looking for browsers'), null, { timeout: 20000 });
  const intoNew = await page.isChecked('input[name=import-into][value=new]');
  await page.click('#import-html');
  await idle();
  const imported = folderRow('kmzq-import');
  const want = { name: 'kmzq-import', links: 1, folders: [{ name: 'Kmzq Top', links: 1, folders: [{ name: 'Kmzq Sub', links: 1, folders: [{ name: 'Kmzq Deep', links: 2, folders: [] }] }] }] };
  check('12 import nested folders: the tree appears under a new folder', intoNew && imported?.parent_id === ROOT && JSON.stringify(dbTree(imported.id, 'kmzq-import')) === JSON.stringify(want)
    && (await treeSelected()) === imported.id && (await page.isVisible(`${treeLi(imported.id)}`)));

  // 15. Export everything to HTML: a valid Netscape file with the same folder tree.
  const exportNow = async (scope) => {
    fs.rmSync(exportFile, { force: true });
    await menu('export-open');
    await page.waitForSelector('#export-dlg[open]');
    await page.check('input[name=ex-format][value=html]');
    await page.check(`input[name=ex-scope][value=${scope}]`);
    await page.click('#ex-go');
    await idle();
    try { return parseNetscape(fs.readFileSync(exportFile, 'utf8')); } catch (e) { return { error: e.message }; }
  };
  const all = await exportNow('all');
  check('15 export everything: valid Netscape HTML, same folder tree', !all.error && JSON.stringify(all) === JSON.stringify(dbTree(ROOT)), all.error ?? `${all.links} top-level links`);

  // 16. Export one folder: only that subtree.
  await key('ArrowRight', '#tree'); // expand the imported folder (selected after the import)
  await openFolder(folderRow('Kmzq Top').id);
  const sub = await exportNow('folder');
  const topTree = dbTree(folderRow('Kmzq Top').id, 'Kmzq Top');
  const subOk = !sub.error && (JSON.stringify({ ...sub, name: 'Kmzq Top' }) === JSON.stringify(topTree)
    || (sub.links === 0 && sub.folders.length === 1 && JSON.stringify(sub.folders[0]) === JSON.stringify(topTree)));
  check('16 export one folder: only that subtree', subOk, sub.error ?? '');

  // 13. Ctrl+Z removes the whole import.
  await key('Control+z');
  check('13 Ctrl+Z removes the whole import', !folderRow('kmzq-import') && !folderRow('Kmzq Top') && !folderRow('Kmzq Deep')
    && sql("SELECT count(*) AS n FROM bookmark WHERE url LIKE 'https://kmzq-e2e-import.example/%'")[0].n === 0);

  // 14. Options "Also add folder names as keywords" on → the Import dialog opens ticked.
  const setFolderKw = async (on) => {
    await menu('settings-open');
    await page.waitForSelector('#settings[open]');
    await page.setChecked('#s-folderkw', on);
    await page.click('#s-save');
    await idle();
  };
  await setFolderKw(true);
  await menu('import');
  await page.waitForSelector('#import-dlg[open]');
  await page.waitForFunction(() => !document.getElementById('import-sources').textContent.includes('Looking for browsers'), null, { timeout: 20000 });
  const ticked = await page.isChecked('#import-folder-words');
  await page.click('#import-dlg button[value=close]');
  await setFolderKw(false);
  await menu('import');
  await page.waitForSelector('#import-dlg[open]');
  await page.waitForFunction(() => !document.getElementById('import-sources').textContent.includes('Looking for browsers'), null, { timeout: 20000 });
  const unticked = !(await page.isChecked('#import-folder-words'));
  await page.click('#import-dlg button[value=close]');
  check('14 Options "folder names as keywords" on: Import opens with it ticked', ticked && unticked);

  // 20. Extension adds land in the New Item Folder (ITEM-01, ITEM-03, ITEM-04): the popup
  // with no folder picked, and Fast Add (adds the fast-add keywords). The first makes "New".
  const replies = hostSend(
    { type: 'add', url: 'https://kmzq-e2e-inbox.example/popup', title: 'Kmzq popup' },
    { type: 'add', url: 'https://kmzq-e2e-inbox.example/fast', title: 'Kmzq fast', fast: true },
    { type: 'folders' },
  );
  const [inbox] = inboxRows();
  const inInbox = inbox ? sql("SELECT url, keywords FROM bookmark WHERE folder_id = ? AND url LIKE 'https://kmzq-e2e-inbox.example/%' ORDER BY url", inbox.id) : [];
  check('20 extension adds land in the New Item Folder, made as a root "New" and marked in the tree',
    replies.length === 3 && replies.every((r) => r.ok) && inboxRows().length === 1 && inbox.name === 'New' && inbox.parent_id === ROOT
    && inInbox.length === 2 && inInbox[0].keywords.split(' ').includes('new') && replies[2].new_items === inbox.id && (await waitInboxShown(inbox.id)),
    `${replies.length} replies, ${inboxRows().length} inbox folders, ${inInbox.length} in it`);

  // 21. The inbox deleted (contents move up, nothing lost), then the next Fast Add makes it again.
  await openFolder(inbox.id);
  await expectConfirm(true);
  await key('Delete', '#tree');
  const goneOk = !sql('SELECT 1 FROM folder WHERE id = ?', inbox.id).length && inboxRows().length === 0
    && sql("SELECT count(*) AS n FROM bookmark WHERE folder_id = ? AND url LIKE 'https://kmzq-e2e-inbox.example/%'", ROOT)[0].n === 2;
  const [again] = hostSend({ type: 'add', url: 'https://kmzq-e2e-inbox.example/again', title: 'Kmzq again', fast: true });
  const [made] = inboxRows();
  check('21 the inbox deleted: its items move up; the next extension add recreates "New"',
    goneOk && again?.ok && made?.name === 'New' && made.parent_id === ROOT
    && sql('SELECT folder_id FROM bookmark WHERE id = ?', again.id)[0]?.folder_id === made.id && (await waitInboxShown(made.id))
    && JSON.stringify(await inboxMarked()) === JSON.stringify([made.id]), `deleted ${goneOk}, remade ${!!made}`);
} catch (e) {
  check('run completed', false, String(e.message).replace(/"[^"]*"/g, '"…"').split('\n')[0]);
} finally {
  await browser?.close().catch(() => {});
  cleanedUp = true;
  stopApp();
  await sleep(300);
  cleanupWork();
  restoreKeymarks(usersKeymarks);
}

results.sort((a, b) => (parseInt(a.name, 10) || 99) - (parseInt(b.name, 10) || 99));
const width = Math.max(...results.map((r) => r.name.length));
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name.padEnd(width)}  ${r.detail}`);
const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
