$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$PythonExe = Join-Path $ProjectRoot ".venv-windows\Scripts\python.exe"
$FirmwareImage = Join-Path $ProjectRoot "firmware\moni-si-hai.bin"
$HiddenRunner = Join-Path $PSScriptRoot "run-display-windows.vbs"

if (-not (Test-Path $PythonExe)) {
    throw "Run host\setup-windows.ps1 before updating the firmware."
}
if (-not (Test-Path $FirmwareImage)) {
    throw "The prebuilt firmware image is missing. Download the latest Moni ZIP again."
}

Write-Host "Stopping the Moni telemetry sender..." -ForegroundColor Cyan
Get-CimInstance Win32_Process |
    Where-Object { $_.CommandLine -like "*monitor_windows.py*" } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 1

Set-Location $ProjectRoot
$Port = (& $PythonExe -c "from host.monitor_windows import resolve_port; print(resolve_port(None) or '')").Trim()
if (-not $Port) {
    throw "ESP32 CH340 COM port not found. Plug in the clock and try again."
}

Write-Host "Flashing Moni to $Port..." -ForegroundColor Cyan
& $PythonExe -m esptool --chip esp32 --port $Port write-flash 0x0 $FirmwareImage
if ($LASTEXITCODE -ne 0) {
    throw "Firmware update failed with exit code $LASTEXITCODE."
}

Write-Host "Firmware updated successfully. Restarting live telemetry..." -ForegroundColor Green
Start-Process "$env:WINDIR\System32\wscript.exe" -ArgumentList ('"' + $HiddenRunner + '"')
