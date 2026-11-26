// Shared by smoke.mjs and folders-smoke.mjs: start and stop the real app, on a desktop of its
// own, with its WebView2 debugging port open.

import { chromium } from 'playwright-core';
import { execSync } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const exe = process.env.KEYMARKS_EXE ?? path.join(root, 'target', 'debug', 'keymarks.exe');
// KEYMARKS_EXE: a build with an identifier of its own (TAURI_CONFIG at build time), so its own
// single-instance lock. It runs beside the user's copy: only that exe is stopped, the user's
// copy is never closed or reopened (running.mjs).
export const isolated = !!process.env.KEYMARKS_EXE;
const ps = (cmd) => execSync(`powershell -NoProfile -Command "${cmd}"`, { windowsHide: true }).toString().trim();
const ours = `Get-Process keymarks -ErrorAction SilentlyContinue | ? Path -eq '${exe.replace(/'/g, "''")}'`;
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const log = (msg) => process.stderr.write(`· ${new Date().toTimeString().slice(0, 8)} ${msg}\n`);

// Other apps (e.g. Electron) squat on common debug ports: ask the OS for a free one.
export const freePort = () => new Promise((resolve) => {
  const srv = net.createServer().listen(0, '127.0.0.1', () => {
    const { port } = srv.address();
    srv.close(() => resolve(port));
  });
});

// Waits until the app has really exited, so its files can be deleted.
/** How many copies of the app under test run (only `exe` when isolated, else any keymarks.exe). */
export const instances = () => (isolated ? Number(ps(`@(${ours}).Count`))
  : execSync('tasklist /FI "IMAGENAME eq keymarks.exe" /NH', { windowsHide: true }).toString().split('keymarks.exe').length - 1);

export function stopApp() {
  if (isolated) ps(`${ours} | Stop-Process -Force`);
  else try { execSync('taskkill /IM keymarks.exe /F', { stdio: 'ignore', windowsHide: true }); } catch { /* not running */ }
  for (let i = 0; i < 50; i++) {
    if (!instances()) return;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100); // sleep without spawning a process
  }
}

// On a desktop of its own (desktop.ps1): nothing the app does can reach the user's screen or focus.
export function startApp(env) {
  execSync(`powershell -NoProfile -ExecutionPolicy Bypass -File "${path.join(root, 'e2e', 'desktop.ps1')}" "${exe}"`, {
    env: { ...process.env, ...env }, stdio: 'ignore', windowsHide: true,
  });
}

/** The env that opens the debugging port on `port`, for the WebView2 profile in `webviewData`. */
export const debugEnv = (port, webviewData) => ({
  WEBVIEW2_USER_DATA_FOLDER: webviewData,
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --disable-features=CalculateNativeWinOcclusion --disable-backgrounding-occluded-windows`,
  KEYMARKS_NO_FOCUS: '1',
});

export async function connect(port) {
  for (let i = 0; i < 60; i++) {
    try {
      const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 5000 });
      const page = browser.contexts()[0]?.pages().find((p) => /tauri/.test(p.url()));
      if (page) return { browser, page };
      await browser.close();
    } catch { /* not up yet */ }
    await sleep(500);
  }
  // Exited at once = handed over to another instance with the same identifier (single-instance).
  const running = ps("Get-Process keymarks -ErrorAction SilentlyContinue | % { $_.Id.ToString() + ' ' + $_.Path }").replace(/\r?\n/g, '; ');
  throw new Error(`app did not expose port ${port}; ${instances()} of the app under test running; keymarks processes: ${running || 'none'}`);
}
