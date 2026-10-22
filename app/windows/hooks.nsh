; NSIS hooks for the Keymarks installer: keep the browser host registration in step with
; the install. (The app also re-registers it on every start, so this is belt and braces.)
!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\keymarks-host.exe" --install'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\keymarks-host.exe" --uninstall'
!macroend
