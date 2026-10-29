; Moni installer hooks (included by Tauri's NSIS template, which runs elevated
; for a per-machine install). All the work lives in moni-service.exe:
;   install   - installs the PawnIO sensor driver, retires the old Python
;               sender, registers and starts the service
;   uninstall - stops and removes the service (and with it Moni's OpenRGB)

!macro NSIS_HOOK_PREINSTALL
  ; Upgrade: stop the running service so its files can be replaced.
  IfFileExists "$INSTDIR\moni-service.exe" 0 +2
    nsExec::Exec '"$INSTDIR\moni-service.exe" uninstall'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Starting the Moni service..."
  nsExec::ExecToLog '"$INSTDIR\moni-service.exe" install'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\moni-service.exe" uninstall'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Moni's OpenRGB keeps its settings next to it (written at run time).
  RMDir /r "$INSTDIR\openrgb\config"
!macroend
