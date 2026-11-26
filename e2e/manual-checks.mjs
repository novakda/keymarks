// The two checks no script can do, with the machine doing the recording: Dan only acts.
//   (a) physical key presses reach the page (CDP key events skip WebView2's accelerator
//       filter, so only a real keyboard proves the 8 shortcuts get through);
//   (b) the real native file pickers, for one Import and one Export.
// It starts the ISOLATED e2e build (own identifier, so beside Dan's own Keymarks, which is never
// touched) on a COPY of the data, in front, with the debug port. A listener in the page reports
// every keydown and every status-line change; progress is written to a JSON file as it comes.
//
//   cd /d C:\main\projects\keymarks-folders\e2e && npm run manual        (Windows)
//   npm run manual -- --selftest     plumbing check with synthetic events; no Dan needed
//
// Progress: %LOCALAPPDATA%\Temp\keymarks-manual-checks.json  (WSL: /mnt/c/Users/xhiri/AppData/Local/Temp/)
//   [{ id, step, captured, detail, at }]   (selftest writes keymarks-manual-checks-selftest.json)
// Exits 0 when all 10 are captured, 1 after 10 minutes. Stops only the instance it started.

import { spawn, execSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

const selftest = process.argv.includes('--selftest');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.KEYMARKS_EXE ??= path.join(root, 'target', 'e2e', 'debug', 'keymarks.exe'); // isolated build
const { exe, freePort, log, sleep, debugEnv, instances } = await import('./launch.mjs');
if (!fs.existsSync(exe)) { log(`no isolated build at ${exe} (build it as km3479-e2e.cmd does)`); process.exit(2); }
if (instances()) { log('the isolated e2e build is already running; close it first (your own Keymarks is fine)'); process.exit(2); }

const OUT = path.join(process.env.LOCALAPPDATA ?? os.tmpdir(), 'Temp', selftest ? 'keymarks-manual-checks-selftest.json' : 'keymarks-manual-checks.json');
const TIMEOUT = selftest ? 90_000 : 600_000;

// ---- the 10 checks, in the order Dan does them -------------------------------------------------
const key = (id, step, test) => ({ id, step, test: (k) => k.trusted !== false || selftest ? test(k) : false });
const checks = [
  key('ctrl-shift-n', 'Click any bookmark in the list, then press Ctrl+Shift+N. A new folder appears in the tree: type a name, Enter.', (k) => k.ctrl && k.shift && !k.alt && k.key.toLowerCase() === 'n'),
  key('f2', 'Click that folder in the tree, then press F2. Rename it, Enter.', (k) => k.key === 'F2'),
  key('ctrl-m', 'Click a bookmark in the list, then press Ctrl+M. Pick the folder, Enter.', (k) => k.ctrl && !k.shift && k.key.toLowerCase() === 'm'),
  key('ctrl-z', 'Click the page background (not a text box), then press Ctrl+Z. The move is undone.', (k) => k.ctrl && !k.shift && k.key.toLowerCase() === 'z'),
  key('ctrl-shift-f', 'Press Ctrl+Shift+F. The scope button flips between Everywhere and In folder.', (k) => k.ctrl && k.shift && k.key.toLowerCase() === 'f'),
  key('ctrl-shift-l', 'Click a bookmark, then press Ctrl+Shift+L. Its folder is selected in the tree.', (k) => k.ctrl && k.shift && k.key.toLowerCase() === 'l'),
  key('f1', 'Press F1. The Keyboard shortcuts box opens; close it with Esc.', (k) => k.key === 'F1'),
  key('del', 'Click the folder you made in the tree, then press Delete. Answer Yes to the question.', (k) => k.key === 'Delete'),
  { id: 'import', step: 'Menu: Import, then "Import bookmarks file". In the real file picker choose any .html bookmarks file and open it.',
    status: (s) => /^Imported \d/.test(s) },
  { id: 'export', step: 'Menu: Export, choose HTML, everything. In the real save picker choose any folder and Save.',
    status: (s) => /^Exported \d/.test(s) },
].map((c) => ({ ...c, captured: false, detail: '', at: null }));

const publish = () => {
  const rows = checks.map(({ id, step, captured, detail, at }) => ({ id, step, captured, detail, at }));
  fs.writeFileSync(`${OUT}.tmp`, JSON.stringify(rows, null, 2));
  fs.renameSync(`${OUT}.tmp`, OUT); // readers never see half a file
};
const capture = (c, detail) => {
  if (c.captured) return;
  Object.assign(c, { captured: true, detail, at: new Date().toISOString() });
  publish();
  log(`captured ${c.id}: ${detail}  (${checks.filter((x) => x.captured).length}/${checks.length})`);
};

// ---- the copy of the data ------------------------------------------------------------------------
const realData = path.join(process.env.APPDATA, 'com.pattern158.keymarks');
const source = ['bookmarks.db.v5.bak', 'bookmarks.pre-folders.db'].map((f) => path.join(realData, f)).find((f) => fs.existsSync(f));
if (!source) { log(`no database copy source in ${realData}`); process.exit(2); }
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'km-manual-'));
const webviewData = fs.mkdtempSync(path.join(os.tmpdir(), 'km-manual-webview-'));
const dbPath = path.join(work, 'bookmarks.db');
fs.copyFileSync(source, dbPath);
const rowCount = () => {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try { return db.prepare('SELECT count(*) AS n FROM bookmark').get().n; } finally { db.close(); }
};
const before = rowCount();

// The save picker reports only the file's name in the status line: look for it where people save.
const home = os.homedir();
const findExport = (name, since) => {
  const dirs = [work, ...['Desktop', 'Documents', 'Downloads', 'OneDrive\\Desktop', 'OneDrive\\Documents'].map((d) => path.join(home, d)), home];
  return dirs.map((d) => path.join(d, name)).find((f) => { try { return fs.statSync(f).mtimeMs >= since; } catch { return false; } });
};

// ---- start, in front (selftest: hidden-focus test mode, so it never takes Dan's focus) ----------------
const port = await freePort();
const env = { ...process.env, ...debugEnv(port, webviewData), KEYMARKS_DATA_DIR: work };
if (!selftest) { delete env.KEYMARKS_NO_FOCUS; env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = `--remote-debugging-port=${port}`; }
let child;
let done = false;
function finish(code) {
  if (done) return;
  done = true;
  if (child?.pid) { try { execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore', windowsHide: true }); } catch { /* already gone */ } }
  setTimeout(() => {
    for (const d of [webviewData, work]) { try { fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 }); } catch (e) { log(`could not delete ${d}: ${e.message}`); } }
    process.exit(code);
  }, 1500);
}
process.on('uncaughtException', (e) => { log(`aborted: ${e.message}`); finish(1); });
process.on('SIGINT', () => finish(1));

publish();
log(`starting ${exe} on a copy of the data, debug port ${port}${selftest ? ' (selftest)' : ''}`);
child = spawn(exe, [], { env, stdio: 'ignore', windowsHide: false });
child.on('exit', () => { if (!done) { log('the app was closed'); finish(checks.every((c) => c.captured) ? 0 : 1); } });
const started = Date.now();

// ---- raw CDP: a binding the page calls, a listener injected into every document ------------------------
let wsUrl;
for (let i = 0; i < 120 && !wsUrl; i++) {
  try { wsUrl = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === 'page' && /tauri/.test(t.url))?.webSocketDebuggerUrl; } catch { /* not up yet */ }
  if (!wsUrl) await sleep(500);
}
if (!wsUrl) { log('app did not expose the debug port'); finish(1); await sleep(5000); }
const ws = new WebSocket(wsUrl);
await new Promise((r) => { ws.onopen = r; });
let nextId = 1;
const pending = new Map();
const cdp = (method, params = {}) => new Promise((resolve) => { const id = nextId++; pending.set(id, resolve); ws.send(JSON.stringify({ id, method, params })); });

const PAGE = `(() => {
  if (window.__kmManual) return; window.__kmManual = 1;
  const send = (o) => { try { window.kmManual(JSON.stringify(o)); } catch {} };
  addEventListener('keydown', (e) => send({ t: 'key', key: e.key, ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey, alt: e.altKey, trusted: e.isTrusted,
    target: e.target.id || e.target.tagName }), true);
  const hook = () => { const s = document.getElementById('status'); if (!s) return false;
    new MutationObserver(() => send({ t: 'status', text: s.textContent })).observe(s, { childList: true, characterData: true, subtree: true }); return true; };
  if (!hook()) addEventListener('DOMContentLoaded', hook);
})()`;

ws.onmessage = ({ data }) => {
  const m = JSON.parse(data);
  if (m.id) { pending.get(m.id)?.(m.result); pending.delete(m.id); return; }
  if (m.method !== 'Runtime.bindingCalled' || m.params.name !== 'kmManual') return;
  const ev = JSON.parse(m.params.payload);
  if (ev.t === 'key') {
    const hit = checks.find((c) => c.test?.(ev));
    if (hit) capture(hit, `arrived at the page${ev.trusted ? '' : ' (synthetic)'}, target ${ev.target}`);
  } else if (ev.t === 'status') {
    const hit = checks.find((c) => c.status?.(ev.text));
    if (!hit) return;
    let extra = '';
    if (hit.id === 'import') { try { extra = `; bookmark rows ${before} -> ${rowCount()}`; } catch { /* db busy */ } }
    if (hit.id === 'export') {
      const name = ev.text.match(/ to (.+)\.$/)?.[1];
      const file = name && findExport(name, started);
      extra = file ? `; file written: ${file}` : `; saved as ${name} (folder not found in Desktop/Documents/Downloads)`;
    }
    capture(hit, `"${ev.text}"${extra}`);
  }
  if (checks.every((c) => c.captured) && !selftest) finish(0);
};
await cdp('Runtime.enable');
await cdp('Runtime.addBinding', { name: 'kmManual' });
await cdp('Page.enable');
await cdp('Page.addScriptToEvaluateOnNewDocument', { source: PAGE });
await cdp('Runtime.evaluate', { expression: PAGE });
log(selftest ? 'listening; injecting synthetic events' : 'listening: the app is open, work through the steps in the JSON');

// ---- selftest: synthetic events through the same page listener, JSON read back from disk ------------------
if (selftest) {
  const fail = [];
  const onDisk = () => JSON.parse(fs.readFileSync(OUT, 'utf8'));
  const expect = (what, ok) => { log(`${ok ? 'PASS' : 'FAIL'} ${what}`); if (!ok) fail.push(what); };
  const init = onDisk();
  expect('JSON starts with 10 uncaptured steps', init.length === 10 && init.every((r) => !r.captured && r.id && r.step && r.at === null));
  const press = (k, mods = {}) => cdp('Runtime.evaluate', { expression: `document.body.dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(k)}, bubbles: true, ${Object.entries(mods).map(([a, b]) => `${a}: ${b}`).join(', ')} }))` });
  const keys = [['N', { ctrlKey: true, shiftKey: true }], ['F2'], ['m', { ctrlKey: true }], ['z', { ctrlKey: true }], ['F', { ctrlKey: true, shiftKey: true }], ['L', { ctrlKey: true, shiftKey: true }], ['F1'], ['Delete']];
  for (let i = 0; i < keys.length; i++) {
    await press(...keys[i]);
    await sleep(300);
    expect(`key ${i + 1} captured, then ${i + 1} total on disk`, onDisk().filter((r) => r.captured).length === i + 1 && onDisk()[i].captured);
  }
  fs.writeFileSync(path.join(work, 'st-export.html'), '<html></html>');
  const say = (t) => cdp('Runtime.evaluate', { expression: `document.getElementById('status').textContent = ${JSON.stringify(t)}` });
  await say('Imported 3 bookmarks from st-import.html. Ctrl+Z undoes it.');
  await sleep(300);
  expect('import captured from the status line', onDisk().find((r) => r.id === 'import').captured);
  await say('Exported 3 bookmarks to st-export.html.');
  await sleep(300);
  const ex = onDisk().find((r) => r.id === 'export');
  expect('export captured with the written file path', ex.captured && ex.detail.includes('file written:') && ex.detail.includes('st-export.html'));
  expect('all 10 captured', onDisk().every((r) => r.captured && r.at));
  console.log(fail.length ? `SELFTEST FAIL (${fail.length})` : 'SELFTEST PASS (10/10)');
  finish(fail.length ? 1 : 0);
}

setTimeout(() => { log('timed out after 10 minutes'); finish(checks.every((c) => c.captured) ? 0 : 1); }, TIMEOUT);
await new Promise(() => {}); // keep the loop alive until finish() exits
