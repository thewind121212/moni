; Moni installer hooks (included by Tauri's NSIS template, which runs elevated
; for a per-machine install).

!macro MoniStopService
  ; Stop + remove an existing service so its exe is not locked during upgrade.
  IfFileExists "$INSTDIR\moni-service.exe" 0 +2
    nsExec::Exec '"$INSTDIR\moni-service.exe" uninstall'
  nsExec::Exec 'sc.exe stop MoniService'
  nsExec::Exec 'taskkill.exe /F /IM moni-sensors.exe'
  Sleep 1000
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MoniStopService
  ; The old Python sender would fight the service for the COM port.
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process | Where-Object { $$_.CommandLine -like '*monitor_windows.py*' } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force -ErrorAction SilentlyContinue }"`
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; LibreHardwareMonitor reads Ryzen/SuperIO sensors through the signed PawnIO
  ; driver. Its setup refuses (silently) when PawnIO is already installed.
  DetailPrint "Installing the PawnIO sensor driver..."
  nsExec::ExecToLog '"$INSTDIR\tools\PawnIO_setup.exe" -install -silent'

  DetailPrint "Starting the Moni service..."
  nsExec::ExecToLog '"$INSTDIR\moni-service.exe" install'

  ; Retire the Python sender's login shortcut (setup-windows.ps1).
  SetShellVarContext current
  Delete "$SMSTARTUP\Moni ESP32 Display.lnk"
  SetShellVarContext all
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MoniStopService
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; PawnIO stays: other tools (LibreHardwareMonitor, FanControl) share it.
  ; Settings in %ProgramData%\Moni are kept for a reinstall.
!macroend
