# Starts a program on a separate, never-shown desktop. Windows there can't take the user's
# focus or appear on screen, whatever the program does. The environment is inherited.
# This script holds the desktop's only handle, so it waits for the program to open a window
# there (then the program holds the desktop too) before exiting. A program that attached after
# the desktop was gone failed at start with 0xC0000142: the first launch of a fresh build,
# which loads slower (#3502).
#   powershell -File desktop.ps1 <exe>
param([string]$Exe)
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class HiddenDesktop {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct STARTUPINFO { public int cb; public string r1, desktop, title; public int x, y, w, h, cx, cy, fill, flags; public short show, r2; public IntPtr r3, hin, hout, herr; }
  [StructLayout(LayoutKind.Sequential)]
  struct PROCESS_INFORMATION { public IntPtr process, thread; public int pid, tid; }
  [DllImport("user32", CharSet = CharSet.Unicode, SetLastError = true)]
  static extern IntPtr CreateDesktopW(string name, IntPtr d, IntPtr m, int flags, uint access, IntPtr sa);
  delegate bool EnumProc(IntPtr hwnd, IntPtr lp);
  [DllImport("user32")] static extern bool EnumDesktopWindows(IntPtr desk, EnumProc cb, IntPtr lp);
  [DllImport("user32")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out int pid);
  [DllImport("kernel32")] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
  [DllImport("kernel32", CharSet = CharSet.Unicode, SetLastError = true)]
  static extern bool CreateProcessW(string app, string cmd, IntPtr pa, IntPtr ta, bool inherit, int flags, IntPtr env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
  public static int Start(string exe) {
    IntPtr desk;
    if ((desk = CreateDesktopW("keymarks-e2e", IntPtr.Zero, IntPtr.Zero, 0, 0x10000000u, IntPtr.Zero)) == IntPtr.Zero)
      throw new System.ComponentModel.Win32Exception();
    var si = new STARTUPINFO { desktop = @"WinSta0\keymarks-e2e" };
    si.cb = Marshal.SizeOf(si);
    PROCESS_INFORMATION pi;
    if (!CreateProcessW(exe, "\"" + exe + "\"", IntPtr.Zero, IntPtr.Zero, false, 0, IntPtr.Zero, null, ref si, out pi))
      throw new System.ComponentModel.Win32Exception();
    for (int i = 0; i < 300; i++) {
      uint code;
      if (GetExitCodeProcess(pi.process, out code) && code != 259) // 259 = STILL_ACTIVE
        throw new Exception(string.Format("{0} exited at start, code 0x{1:X8}", exe, code));
      bool joined = false;
      EnumDesktopWindows(desk, (h, lp) => { int p; GetWindowThreadProcessId(h, out p); joined |= p == pi.pid; return !joined; }, IntPtr.Zero);
      if (joined) break;
      System.Threading.Thread.Sleep(100);
    }
    return pi.pid;
  }
}
'@
[HiddenDesktop]::Start($Exe)
