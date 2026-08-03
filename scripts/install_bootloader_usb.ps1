param(
    [string]$Drive = "F:"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Drive)) {
    throw "Drive $Drive does not exist. Check the USB drive letter."
}

& "$PSScriptRoot\build_bootloader_windows.ps1"

Copy-Item -Force "$PSScriptRoot\..\target\bootloader8.img" "$Drive\bootloader8.img"
Copy-Item -Force "$PSScriptRoot\..\config.txt" "$Drive\config.txt"

Write-Host "Installed UART bootloader to $Drive"
Write-Host "Files:"
Write-Host "  $Drive\bootloader8.img"
Write-Host "  $Drive\config.txt"
