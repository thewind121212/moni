param(
    [switch]$NoStartup,
    [switch]$SkipLibreHardwareMonitor
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$VirtualEnv = Join-Path $ProjectRoot ".venv-windows"
$PythonExe = Join-Path $VirtualEnv "Scripts\python.exe"

Write-Host "Setting up Moni for Windows..." -ForegroundColor Cyan

$PythonLauncher = Get-Command py -ErrorAction SilentlyContinue
$PythonCommand = Get-Command python -ErrorAction SilentlyContinue
if ($PythonLauncher) {
    & $PythonLauncher.Source -3 -m venv $VirtualEnv
} elseif ($PythonCommand) {
    & $PythonCommand.Source -m venv $VirtualEnv
} else {
    throw "Python 3 is required. Install it from python.org, then run this script again."
}

& $PythonExe -m pip install --upgrade pip
& $PythonExe -m pip install -r (Join-Path $PSScriptRoot "requirements-windows.txt")

if (-not $SkipLibreHardwareMonitor) {
    $Winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($Winget) {
        Write-Host "Installing LibreHardwareMonitor for Ryzen temperature, power, and CPU-fan RPM..."
        & $Winget.Source install --id LibreHardwareMonitor.LibreHardwareMonitor --exact --source winget --accept-package-agreements --accept-source-agreements
    } else {
        Write-Warning "WinGet was not found. Install LibreHardwareMonitor manually from its official GitHub releases page."
    }
}

if (-not $NoStartup) {
    $StartupFolder = [Environment]::GetFolderPath("Startup")
    $ShortcutPath = Join-Path $StartupFolder "Moni ESP32 Display.lnk"
    $WScript = New-Object -ComObject WScript.Shell
    $Shortcut = $WScript.CreateShortcut($ShortcutPath)
    $Shortcut.TargetPath = "$env:WINDIR\System32\wscript.exe"
    $Shortcut.Arguments = '"' + (Join-Path $PSScriptRoot "run-display-windows.vbs") + '"'
    $Shortcut.WorkingDirectory = $ProjectRoot
    $Shortcut.Description = "Start the Moni ESP32 hardware display"
    $Shortcut.Save()
    Write-Host "Moni will start automatically when you sign in."
}

Write-Host ""
Write-Host "Setup complete." -ForegroundColor Green
Write-Host "1. Start LibreHardwareMonitor as Administrator and leave it running/minimized."
Write-Host "2. Plug the clock into this PC."
Write-Host "3. Run host\run-display-windows.cmd"
Write-Host "4. If CPU fan RPM is wrong, run host\list-sensors-windows.cmd"
