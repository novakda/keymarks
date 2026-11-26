# Sends one real key chord through Windows SendInput (the path a physical keyboard takes,
# WebView2's accelerator filter included) to the window of process <ProcessId>, after bringing
# that window to the front. If the window isn't in front, nothing is sent (exit 3): keys never
# go to whatever else has focus.
#   powershell -File os-keys.ps1 <pid> ctrl+shift+n      (names: ctrl shift alt f1..f12 del esc a..z)
param([int]$ProcessId, [string]$Chord)
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class OsKeys {
  [StructLayout(LayoutKind.Explicit, Size = 40)]
  struct INPUT { [FieldOffset(0)] public int type; [FieldOffset(8)] public ushort vk; [FieldOffset(10)] public ushort scan; [FieldOffset(12)] public uint flags; }
  [DllImport("user32")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32")] static extern uint MapVirtualKey(uint code, uint type);
  [DllImport("user32")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32")] static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32")] static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32")] static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32")] static extern bool IsIconic(IntPtr h);
  [DllImport("user32")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32")] static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("kernel32")] static extern uint GetCurrentThreadId();
  public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
  // Windows lets a process take the foreground only while attached to the input of the one that has it.
  public static void Front(IntPtr h) {
    if (IsIconic(h)) ShowWindow(h, 9);
    uint pid, fg = GetWindowThreadProcessId(GetForegroundWindow(), out pid), me = GetCurrentThreadId();
    AttachThreadInput(me, fg, true);
    BringWindowToTop(h);
    SetForegroundWindow(h);
    AttachThreadInput(me, fg, false);
  }
  public static void Send(ushort[] vks, bool[] extended) {
    var list = new INPUT[vks.Length * 2];
    for (int i = 0; i < vks.Length; i++) {
      uint ext = extended[i] ? 1u : 0u;
      list[i] = new INPUT { type = 1, vk = vks[i], scan = (ushort)MapVirtualKey(vks[i], 0), flags = ext };
      list[list.Length - 1 - i] = new INPUT { type = 1, vk = vks[i], scan = (ushort)MapVirtualKey(vks[i], 0), flags = ext | 2u };
    }
    SendInput((uint)list.Length, list, 40);
  }
}
'@
$named = @{ ctrl = 0x11; shift = 0x10; alt = 0x12; del = 0x2E; esc = 0x1B }
$vks = @(); $ext = @()
foreach ($k in $Chord.ToLower().Split('+')) {
  if ($named.ContainsKey($k)) { $vks += [uint16]$named[$k] }
  elseif ($k -match '^f(\d+)$') { $vks += [uint16](0x6F + [int]$Matches[1]) }
  elseif ($k -match '^[a-z0-9]$') { $vks += [uint16][char]$k.ToUpper() }
  else { Write-Error "unknown key $k"; exit 2 }
  $ext += ($k -eq 'del')
}
$h = (Get-Process -Id $ProcessId -ErrorAction Stop).MainWindowHandle
if ($h -eq 0) { Write-Error "process $ProcessId has no window"; exit 3 }
for ($i = 0; $i -lt 20 -and [OsKeys]::ForegroundPid() -ne $ProcessId; $i++) { [OsKeys]::Front($h); Start-Sleep -Milliseconds 100 }
if ([OsKeys]::ForegroundPid() -ne $ProcessId) { Write-Error "could not bring process $ProcessId to the front; nothing sent"; exit 3 }
[OsKeys]::Send([uint16[]]$vks, [bool[]]$ext)
