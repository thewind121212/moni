@echo off
setlocal
cd /d "%~dp0.."
if not exist ".venv-windows\Scripts\python.exe" (
  echo Moni is not installed. Run host\setup-windows.ps1 first.
  pause
  exit /b 1
)
".venv-windows\Scripts\python.exe" "host\monitor_windows.py" %*
