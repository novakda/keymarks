// Real key presses, no Dan needed: the shortcut keys of manual-checks.mjs (plus Ctrl+N) go
// through Windows SendInput (os-keys.ps1), the path a physical keyboard takes after its driver,
// WebView2's accelerator handling included; CDP key events (smoke.mjs) don't take that path.
// Checks each key arrives at the page as a trusted keydown, and that F1 opens the Keyboard
// shortcuts box (#3479: F1 does, so a missing F1 in a manual run is upstream of Windows).
// Runs only the ISOLATED e2e build (KEYMARKS_EXE, own identifier), on an empty data folder, IN
// FRONT: the window takes focus for a few seconds per key. Dan's own Keymarks is never touched.
//   set KEYMARKS_EXE=C:\main\projects\keymarks-folders\target\e2e\debug\keymarks.exe
//   cd e2e && npm run keys

import { spawn, execFileSync, execSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { exe, isolated, freePort, log, sleep, connect, instances } from './launch.mjs';

if (!isolated || !fs.existsSync(exe)) { log('set KEYMARKS_EXE to the isolated e2e build (see km3479-e2e.cmd)'); process.exit(2); }
if (instances()) { log('the isolated e2e build is already running; close it first'); process.exit(2); }

const work = fs.mkdtempSync(path.join(os.tmpdir(), 'km-keys-'));
const webviewData = fs.mkdtempSync(path.join(os.tmpdir(), 'km-keys-webview-'));
const port = await freePort();
const env = { ...process.env, KEYMARKS_DATA_DIR: work, WEBVIEW2_USER_DATA_FOLDER: webviewData, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` };
delete env.KEYMARKS_NO_FOCUS; // the window must be able to take focus to get real keys
const child = spawn(exe, [], { env, stdio: 'ignore' });
function finish(code) {
  try { execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore', windowsHide: true }); } catch { /* gone */ }
  setTimeout(() => {
    for (const d of [webviewData, work]) { try { fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 }); } catch (e) { log(`could not delete ${d}: ${e.message}`); } }
    process.exit(code);
  }, 1500);
}
process.on('uncaughtException', (e) => { log(`aborted: ${e.message}`); finish(1); });
setTimeout(() => { log('timed out'); finish(1); }, 120_000).unref();

const { page } = await connect(port);
await page.waitForSelector('#status');
// Record every keydown the page gets; refuse dialogs (a real confirm() window would take focus).
const listen = () => {
  window.__keys = [];
  addEventListener('keydown', (e) => window.__keys.push({ key: e.key, ctrl: e.ctrlKey, shift: e.shiftKey, trusted: e.isTrusted }), true);
  window.confirm = async () => false;
  window.prompt = () => null;
};
await page.evaluate(listen);

const press = (chord) => execFileSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(import.meta.dirname, 'os-keys.ps1'), String(child.pid), chord], { windowsHide: true });
const results = [];
const check = (name, ok) => { results.push(ok); log(`${ok ? 'ok  ' : 'FAIL'} ${name}`); };
async function arrives(chord) {
  await page.evaluate(() => { window.__keys = []; });
  press(chord);
  await sleep(400);
  const parts = chord.split('+');
  const want = parts.at(-1);
  const got = await page.evaluate(() => window.__keys);
  const hit = got.find((k) => k.trusted && k.key.toLowerCase() === ({ del: 'delete' }[want] ?? want)
    && k.ctrl === parts.includes('ctrl') && k.shift === parts.includes('shift'));
  check(`${chord} reaches the page as a real key${hit ? '' : ` (got ${JSON.stringify(got.map((k) => k.key))})`}`, hit);
  press('esc'); // close whatever the key opened
  await sleep(200);
}

for (const chord of ['ctrl+shift+n', 'f2', 'ctrl+m', 'ctrl+z', 'ctrl+shift+f', 'ctrl+shift+l', 'del', 'ctrl+n']) await arrives(chord);
await page.evaluate(() => { window.__keys = []; });
press('f1');
await sleep(500);
check('F1 (real key) opens the Keyboard shortcuts box', await page.evaluate(() => document.getElementById('keys-dlg').open));
press('esc');
await sleep(300);
check('Esc closes it', !(await page.evaluate(() => document.getElementById('keys-dlg').open)));

const failed = results.filter((r) => !r).length;
console.log(failed ? `KEYS FAIL (${failed}/${results.length})` : `KEYS PASS (${results.length}/${results.length})`);
finish(failed ? 1 : 0);
