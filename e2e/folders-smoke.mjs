// The folder features in the real app (the folders test plan, 21 checks plus 22-24 for step 8, 25 for step 9, 26-27 for step 10, 28+ for the rest of P1), driven through the
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
  window.confirm = (message) => new Promise((resolve) => {
    window.__e2eLastConfirm = message;
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
  check('1 upgrade: all bookmarks in the root, v10, with a .bak', before.version < 6 && userVersion(dbPath) === 10 && after.n === before.count
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

  // 4. Del on a folder with items (ITEM-14): it asks with the count of folders and bookmarks
  // in the subtree, and nothing happens until the answer (4b); Cancel keeps it all, OK
  // deletes the folder and everything below it, and one Ctrl+Z brings it all back.
  const subtree = `WITH RECURSIVE t(id) AS (SELECT ? UNION SELECT f.id FROM folder f JOIN t ON f.parent_id = t.id)`;
  const nFolders = sql(`${subtree} SELECT count(*) AS n FROM t`, alpha.id)[0].n;
  const under = sql(`${subtree} SELECT b.id FROM bookmark b WHERE b.folder_id IN (SELECT id FROM t)`, alpha.id).map((r) => r.id);
  const p = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
  await openFolder(alpha.id);
  await expectConfirm('hold');
  await page.focus('#tree');
  await page.keyboard.press('Delete');
  await sleep(700);
  const waited = !!folderRow('Kmzqalpha') && (await page.evaluate(() => typeof window.__e2eAnswer === 'function'));
  const asked = await page.evaluate(() => window.__e2eLastConfirm);
  await page.evaluate(() => window.__e2eAnswer(false));
  await idle();
  check('4b Del asks with the subtree count and waits for the answer; Cancel keeps it all', waited && !!folderRow('Kmzqalpha')
    && asked?.startsWith(`Delete ${p(nFolders, 'folder')} and ${p(under.length, 'bookmark')}?`) && under.every((id) => sql('SELECT 1 FROM bookmark WHERE id = ?', id).length),
    waited ? asked : 'deleted before any answer');
  await expectConfirm(true);
  await key('Delete', '#tree');
  const allGone = !folderRow('Kmzqalpha') && !folderRow('Kmzqbeta renamed') && under.every((id) => !sql('SELECT 1 FROM bookmark WHERE id = ?', id).length);
  const rootSel = (await treeSelected()) === ROOT;
  await key('Control+z', '#list');
  check('4 Del on a folder with items deletes the subtree; one Ctrl+Z restores it all', allGone && rootSel && folderRow('Kmzqalpha')?.parent_id === ROOT
    && folderRow('Kmzqbeta renamed')?.parent_id === alpha.id && under.every((id) => sql('SELECT 1 FROM bookmark WHERE id = ?', id).length),
    `${nFolders} folders, ${under.length} bookmarks; gone ${allGone}, root selected ${rootSel}`);

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
  // Every import ends on its merge report (XFER-02); close it so the menus are reachable again.
  await page.click('#report-dlg button[value=close]');

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

  // 21. The inbox deleted (with what was in it, ITEM-14), then the next Fast Add makes it again.
  await openFolder(inbox.id);
  await expectConfirm(true);
  await key('Delete', '#tree');
  const goneOk = !sql('SELECT 1 FROM folder WHERE id = ?', inbox.id).length && inboxRows().length === 0
    && sql("SELECT count(*) AS n FROM bookmark WHERE url LIKE 'https://kmzq-e2e-inbox.example/%'")[0].n === 0;
  const [again] = hostSend({ type: 'add', url: 'https://kmzq-e2e-inbox.example/again', title: 'Kmzq again', fast: true });
  const [made] = inboxRows();
  check('21 the inbox deleted with its items; the next extension add recreates "New"',
    goneOk && again?.ok && made?.name === 'New' && made.parent_id === ROOT
    && sql('SELECT folder_id FROM bookmark WHERE id = ?', again.id)[0]?.folder_id === made.id && (await waitInboxShown(made.id))
    && JSON.stringify(await inboxMarked()) === JSON.stringify([made.id]), `deleted ${goneOk}, remade ${!!made}`);

  // 22. Merge with report (XFER-02, LINK-18): a link already here by the URL rules takes the
  // import's <DD> as its note (it had none; XFER-03), a second copy is skipped, a new one is added; the
  // report shows 1 / 1 / 1 and one Ctrl+Z takes back both the fill and the add.
  const [seed] = hostSend({ type: 'add', url: 'https://www.kmzq-e2e-merge.example/a/', title: 'Kmzq merge A', folder: ROOT });
  fs.writeFileSync(importFile, `<!DOCTYPE NETSCAPE-Bookmark-file-1>
<DL><p>
  <DT><A HREF="http://kmzq-e2e-merge.example/a">Kmzq merge A (import)</A>
  <DD>Kmzq filled from the import
  <DT><A HREF="https://KMZQ-E2E-MERGE.example/a">Kmzq merge A again</A>
  <DT><A HREF="https://kmzq-e2e-merge.example/b">Kmzq merge B</A>
</DL><p>
`);
  const mergeRows = () => sql("SELECT id, title, notes FROM bookmark WHERE url LIKE '%kmzq-e2e-merge.example/%' COLLATE NOCASE ORDER BY id");
  await menu('import');
  await page.waitForSelector('#import-dlg[open]');
  await page.waitForFunction(() => !document.getElementById('import-sources').textContent.includes('Looking for browsers'), null, { timeout: 20000 });
  await page.click('#import-html');
  await idle();
  const report = await page.evaluate(() => ({
    open: document.getElementById('report-dlg').open,
    sum: document.getElementById('report-sum').textContent,
    lists: [...document.querySelectorAll('#report-lists details')].map((d) => d.querySelectorAll('li').length),
  }));
  const afterMerge = mergeRows();
  check('22 merge import: 1 added / 1 updated / 1 skipped, the match filled, listed in the report',
    seed?.ok && !seed.existed && report.open && /1 added.*\/ 1 updated \/ 1 skipped/.test(report.sum) && JSON.stringify(report.lists) === '[1,1,1]'
    && afterMerge.length === 2 && afterMerge[0].id === seed.id && afterMerge[0].title === 'Kmzq merge A' && afterMerge[0].notes === 'Kmzq filled from the import',
    `${report.sum} ${JSON.stringify(afterMerge)}`);
  await page.click('#report-dlg button[value=close]');
  await key('Control+z');
  const undone = mergeRows();
  check('22b one Ctrl+Z takes back the fill and the add', undone.length === 1 && undone[0].notes === '', JSON.stringify(undone));

  // 23. The extension's add and lookup match by the same rules (ITEM-02): http vs https,
  // www., host case and the trailing / don't make a new bookmark.
  const [again2, look] = hostSend({ type: 'add', url: 'http://KMZQ-E2E-MERGE.example/a', title: 'Kmzq dup add' }, { type: 'lookup', url: 'https://kmzq-e2e-merge.example/a' });
  check('23 extension add and lookup find the existing bookmark by the URL rules', again2?.ok && again2.existed && again2.id === seed.id && look?.found && look.title === 'Kmzq merge A'
    && mergeRows().length === 1, `${JSON.stringify(again2)} ${JSON.stringify(look)}`);

  // 24. Duplicate URLs view (SAVD-10): copies made before LINK-18 (written straight into the
  // database) are grouped; Ctrl+D shows them, Merge keeps the focused one, Ctrl+Z restores.
  {
    const db = new DatabaseSync(dbPath);
    try {
      db.exec('PRAGMA busy_timeout = 5000');
      const ins = db.prepare("INSERT INTO bookmark (url, title, keywords, added, folder_id) VALUES (?, ?, ?, unixepoch(), ?)");
      ins.run('http://kmzq-e2e-dup.example/x', 'Kmzq dup 1', 'kmzqone', ROOT);
      ins.run('https://www.kmzq-e2e-dup.example/x/', 'Kmzq dup 2', 'kmzqtwo', ROOT);
    } finally { db.close(); }
  }
  const dupRows = () => sql("SELECT id, keywords FROM bookmark WHERE url LIKE '%kmzq-e2e-dup.example/%' ORDER BY id");
  await key('Control+d');
  // The copy of real data may hold other groups too: ask the page's list (ids, in view order),
  // then scroll the virtual list to the older copy and click it like a user would.
  const [first, second] = dupRows();
  const viewIds = await page.evaluate(() => ids);
  const viewOk = !(await page.isHidden('#dup-bar')) && viewIds.includes(first.id) && viewIds.indexOf(second.id) === viewIds.indexOf(first.id) + 1
    && (await page.textContent('#dup-view')).startsWith('⧉Duplicate URLs [');
  await page.evaluate((id) => { scroller().scrollTop = ids.indexOf(id) * ROW_H[view]; }, first.id);
  await page.waitForSelector(`#table-body tr.vrow:has-text("Kmzq dup 1"), #list li.vrow:has-text("Kmzq dup 1")`);
  await page.click(`#table-body tr.vrow:has-text("Kmzq dup 1"), #list li.vrow:has-text("Kmzq dup 1")`);
  await page.click('#dup-merge');
  await idle();
  const mergedRows = dupRows();
  check('24 Ctrl+D lists the pre-LINK-18 copies; Merge keeps the focused one with both keywords',
    viewOk && mergedRows.length === 1 && mergedRows[0].id === first.id && mergedRows[0].keywords.split(' ').includes('kmzqtwo'), `${viewOk} ${JSON.stringify(mergedRows)}`);
  await key('Control+z');
  check('24b Ctrl+Z brings the merged copy back', dupRows().length === 2, JSON.stringify(dupRows()));
  await key('Control+d');

  // 25. Folder notes (NOTE-02): a folder chosen in the tree shows its notes in the panel; the
  // note saves on blur, is found by search with the folder path, and Ctrl+Z takes it back.
  {
    await openFolder(ROOT);
    await newFolderHere('Kmzq noted');
    const [noted] = sql("SELECT id FROM folder WHERE name = 'Kmzq noted'");
    await page.click(treeLi(noted.id));
    await idle();
    const titleOk = (await page.textContent('#fn-title').catch(() => '')) === 'Kmzq noted';
    await page.click('#fn-note');
    await page.keyboard.type('kmzqnotetext about this folder');
    await page.focus('#tablewrap'); // blur saves
    await idle();
    const saved = sql('SELECT note FROM folder WHERE id = ?', noted.id)[0]?.note;
    const [seed2] = sql("SELECT id FROM bookmark WHERE url LIKE '%kmzq-e2e-dup.example/%' ORDER BY id");
    await page.evaluate(async ([id, to]) => { await invoke('move_items', { folders: [], bookmarks: [id], to, beside: null }); }, [seed2.id, noted.id]);
    await search('kmzqnotetext');
    const found = (await shown()).ids;
    await search('');
    check('25 folder note saves on blur and searches with the folder path', titleOk && saved === 'kmzqnotetext about this folder' && found.includes(seed2.id), `${saved} ${JSON.stringify(found)}`);
    await key('Control+z'); // the move
    await key('Control+z'); // the note
    check('25b Ctrl+Z takes the folder note back', sql('SELECT note FROM folder WHERE id = ?', noted.id)[0]?.note === '', JSON.stringify(sql('SELECT note FROM folder WHERE id = ?', noted.id)));
  }

  // 26. Sort folder (ORD-01..03): folder menu → Sort folder… → Title, descending, folders
  // first, sub-folders too; the stored order changes; one Ctrl+Z restores it.
  {
    const [noted] = sql("SELECT id FROM folder WHERE name = 'Kmzq noted'");
    await openFolder(noted.id);
    await newFolderHere('Kmzq sub');
    await openFolder(noted.id);
    const db = new DatabaseSync(dbPath);
    try {
      db.exec('PRAGMA busy_timeout = 5000');
      const ins = db.prepare("INSERT INTO bookmark (url, title, added, folder_id) VALUES (?, ?, unixepoch(), ?)");
      for (const t of ['Kmzq sort b', 'Kmzq sort C', 'Kmzq sort a']) ins.run(`https://kmzq-e2e-sort.example/${t.at(-1)}`, t, noted.id);
    } finally { db.close(); }
    const order = () => sql(`SELECT name AS t, position FROM folder WHERE parent_id = ?1 UNION ALL SELECT title, position FROM bookmark WHERE folder_id = ?1 ORDER BY 2`, noted.id).map((r) => r.t);
    const startOrder = order();
    await page.click(treeLi(noted.id), { button: 'right' });
    await page.click('#fctx [data-act=sort]');
    await page.selectOption('#so-by', 'title');
    await page.selectOption('#so-dir', 'down');
    await page.selectOption('#so-folders', 'first');
    await page.click('#sort-form button[type=submit]');
    await idle();
    const sortedOrder = order();
    check('26 Sort folder rewrites the stored order: folders first, then titles Z→A', JSON.stringify(sortedOrder) === JSON.stringify(['Kmzq sub', 'Kmzq sort C', 'Kmzq sort b', 'Kmzq sort a']), JSON.stringify(sortedOrder));
    await key('Control+z');
    check('26b one Ctrl+Z restores the order', JSON.stringify(order()) === JSON.stringify(startOrder), JSON.stringify(order()));
  }

  // 27. Dead Links view (SAVD-09): a bookmark whose check found it gone (status written as the
  // checker would) is listed across folders with the Folder column; Esc returns.
  {
    const [one] = sql("SELECT id FROM bookmark WHERE url = 'https://kmzq-e2e-sort.example/a'");
    {
      const db = new DatabaseSync(dbPath);
      try { db.exec('PRAGMA busy_timeout = 5000'); db.prepare("UPDATE bookmark SET check_status = 'not_found', http_status = 404, check_enabled = 1 WHERE id = ?").run(one.id); } finally { db.close(); }
    }
    await key('Control+l');
    const deadIds = (await shown()).ids;
    const folderCol = (await page.$$eval('#table-head th', (ths) => ths.map((t) => t.textContent))).some((t) => t.startsWith('Folder'));
    check('27 Ctrl+L lists the dead link with the Folder column', deadIds.includes(one.id) && (await status()).startsWith('Dead Links') && folderCol, `${deadIds.length} ${folderCol}`);
    await key('Escape');
    check('27b Esc leaves the Dead Links view', !(await status()).startsWith('Dead Links'), await status());
  }

  // 28. Smart selection (FLD-06): Ctrl+click picks a folder, a folder inside it and a second
  // folder; Ctrl+M moves the selection as one undo step, the inner folder staying inside its parent.
  {
    const named = (n) => sql('SELECT id, parent_id FROM folder WHERE name = ?', n)[0];
    for (const n of ['Kmzq sel target', 'Kmzq sel A', 'Kmzq sel B']) { await openFolder(1); await newFolderHere(n); }
    await openFolder(named('Kmzq sel A').id);
    await newFolderHere('Kmzq sel A1');
    const [a, a1, b, target] = ['Kmzq sel A', 'Kmzq sel A1', 'Kmzq sel B', 'Kmzq sel target'].map(named);
    await openFolder(a.id);
    await page.click(treeLi(a1.id), { modifiers: ['Control'] });
    await page.click(treeLi(b.id), { modifiers: ['Control'] });
    const marked = await page.$$eval('#tree li[aria-selected=true]', (lis) => lis.map((li) => Number(li.dataset.id)));
    await key('Control+m', '#tree');
    const title = await page.textContent('#move-title');
    await page.keyboard.type('Kmzq sel target');
    await page.keyboard.press('Enter');
    await idle();
    const [a2, a12, b2] = ['Kmzq sel A', 'Kmzq sel A1', 'Kmzq sel B'].map(named);
    check('28 Ctrl+click selects three folders; Ctrl+M moves them, the inner one stays in its parent',
      marked.length === 3 && title === 'Move 3 folders to' && a2.parent_id === target.id && b2.parent_id === target.id && a12.parent_id === a.id,
      `${marked.length} ${title} ${JSON.stringify([a2, a12, b2])}`);
    await key('Control+z');
    check('28b one Ctrl+Z puts both back in the top folder', named('Kmzq sel A').parent_id === 1 && named('Kmzq sel B').parent_id === 1, JSON.stringify([named('Kmzq sel A'), named('Kmzq sel B')]));
  }

  // 29. Explorer-style Cut (ITEM-15): Ctrl+X dims the row and removes nothing; Esc clears it;
  // Ctrl+X again, then Ctrl+V on another tree folder moves it there; one Ctrl+Z puts it back.
  {
    const [c] = hostSend({ type: 'add', url: 'https://kmzq-e2e-cut.example/', title: 'Kmzq cut', fast: true });
    const home = sql('SELECT folder_id FROM bookmark WHERE id = ?', c.id)[0].folder_id;
    const target = sql('SELECT id FROM folder WHERE name = ?', 'Kmzq sel target')[0].id;
    await openFolder(home);
    const rowSel = async () => `#table-body tr.vrow >> nth=${(await shown()).ids.indexOf(c.id)}`;
    await page.click(await rowSel());
    await key('Control+x');
    const dimmed = (await page.getAttribute(await rowSel(), 'class')).includes('cut');
    const kept = sql('SELECT folder_id FROM bookmark WHERE id = ?', c.id)[0]?.folder_id === home;
    await key('Escape');
    const cleared = !(await page.getAttribute(await rowSel(), 'class')).includes('cut');
    check('29 Ctrl+X dims the row and removes nothing; Esc clears the mark', dimmed && kept && cleared, `dimmed ${dimmed}, kept ${kept}, cleared ${cleared}`);
    await key('Control+x');
    await openFolder(target);
    await key('Control+v', '#tree');
    const moved = sql('SELECT folder_id FROM bookmark WHERE id = ?', c.id)[0]?.folder_id === target;
    await key('Control+z');
    check('29b Ctrl+V on a folder moves the cut bookmark there; one Ctrl+Z puts it back', moved && sql('SELECT folder_id FROM bookmark WHERE id = ?', c.id)[0]?.folder_id === home,
      `moved ${moved}`);
  }

  // 30. Tree tracking (FLD-14, F10): off, selecting a search result leaves the tree; F10 on,
  // the tree selects the result's folder and the search stays; F10 again turns it off.
  {
    const [t] = hostSend({ type: 'add', url: 'https://kmzq-e2e-track.example/', title: 'Kmzq track', fast: true });
    const home = sql('SELECT folder_id FROM bookmark WHERE id = ?', t.id)[0].folder_id;
    await openFolder(ROOT);
    await search('kmzq track');
    const at = () => "#table-body tr.vrow >> nth=0";
    await page.click(at());
    await idle();
    const stayed = (await treeSelected()) === ROOT;
    await key('F10');
    await page.click(at());
    await idle();
    const followed = (await treeSelected()) === home && (await page.inputValue('#q')) === 'kmzq track';
    await key('F10');
    const off = (await page.getAttribute('#track-tree', 'aria-checked')) === 'false';
    check('30 F10 tracking: the tree follows the selected result only while on', stayed && followed && off, `stayed ${stayed}, followed ${followed}, off ${off}`);
  }

  // 31. Folder column (VIEW-02): the header sorts by path (root items first); double-clicking
  // its divider fits it so no shown path is cut off. Sort cleared after.
  {
    await openFolder(ROOT);
    await search('kmzq');
    await page.click('#cols summary');
    if (!(await page.isChecked('#cols-menu input[value=folder]'))) await page.check('#cols-menu input[value=folder]');
    await page.evaluate(() => { document.getElementById('cols').open = false; });
    await page.click('#table-head th[data-col=folder] button');
    await idle();
    const n = await page.$$eval('#table-head th', (ths) => ths.findIndex((th) => th.dataset.col === 'folder'));
    const paths = await page.$$eval('#table-body tr.vrow', (trs, i) => trs.map((tr) => tr.children[i].textContent), n);
    const inOrder = paths.every((p, i) => !i || paths[i - 1].localeCompare(p, undefined, { sensitivity: 'base' }) <= 0);
    await page.dblclick('#table-head th[data-col=folder] .grip');
    const fits = await page.$$eval('#table-body tr.vrow', (trs, i) => trs.every((tr) => tr.children[i].scrollWidth <= tr.children[i].clientWidth), n);
    await page.click('#table-head th[data-col=folder] button');
    await page.click('#table-head th[data-col=folder] button');
    await idle();
    check('31 Folder column sorts by path; divider double-click fits it', paths.length > 1 && inOrder && fits, `${paths.length} rows, in order ${inOrder}, fits ${fits}`);
  }

  // 32. Popup lookup names the existing bookmark's folder (ITEM-02): the host's reply carries
  // the path of the folder the Fast Add put it in (the New Item Folder, at the root).
  {
    const [t] = hostSend({ type: 'lookup', url: 'https://kmzq-e2e-track.example/' });
    const id = sql('SELECT id FROM bookmark WHERE url = ?', 'https://kmzq-e2e-track.example/')[0]?.id;
    const name = sql('SELECT f.name FROM folder f JOIN bookmark b ON b.folder_id = f.id WHERE b.id = ?', id)[0]?.name;
    check('32 the popup lookup returns the existing bookmark\'s folder path', t?.found === true && t.folder === name, `${t?.folder} vs ${name}`);
  }

  // 33. Import dialog (XFER-05): "Replace the collection" is shown but disabled while the
  // collection has items; a new folder stays the default. (pm35 into a new folder needs an
  // empty collection and the native file dialog: covered by store's pm35_import_into_a_new_folder.)
  {
    await page.evaluate(() => document.getElementById('import').click());
    await page.waitForSelector('#import-dlg[open]');
    const off = await page.isDisabled('input[name=import-into][value=replace]');
    const dflt = await page.isChecked('input[name=import-into][value=new]');
    await page.click('#import-dlg button[value=close]');
    check('33 Import offers "replace an empty collection", disabled while not empty', off && dflt, `disabled ${off}, new folder default ${dflt}`);
  }
} catch (e) {
  log(e.stack);
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
