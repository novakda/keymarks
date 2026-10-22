// The user's own Keymarks (installed or a dev build) shares the exe name, identity and data
// folder with the copy under test. Remember which one was running, put exactly that one
// back afterwards, unfocused; if none was running, leave it that way.

import { execSync } from 'node:child_process';

/** Path of the running keymarks.exe, or null. */
export function runningKeymarks() {
  const out = execSync(
    'powershell -NoProfile -Command "(Get-Process keymarks -ErrorAction SilentlyContinue | Select-Object -First 1).Path"',
    { windowsHide: true },
  ).toString().trim();
  return out || null;
}

/** Reopen the user's copy hidden in the tray (--background), so nothing appears or takes
 *  focus; tray or hotkey bring it up. Windows itself starts it (WMI), so it is
 *  not part of this script's process tree: tools that clean up that tree when the script
 *  ends (WSL interop does) can't take it down, and it inherits none of our handles. */
export function restoreKeymarks(path) {
  // KEYMARKS_E2E_NO_RESTORE: leave it closed (e.g. while the installed copy is an older build).
  if (!path || process.env.KEYMARKS_E2E_NO_RESTORE) return;
  const commandLine = `"${path}" --background`.replace(/'/g, "''");
  execSync(`powershell -NoProfile -Command "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = '${commandLine}' } | Out-Null"`, { windowsHide: true });
}
