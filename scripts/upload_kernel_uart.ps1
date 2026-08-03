param(
    [string]$Port = "COM5",
    [int]$Baud = 115200,
    [switch]$Monitor
)

$ErrorActionPreference = "Stop"

& "$PSScriptRoot\build_windows.ps1"

$arguments = @(
    "$PSScriptRoot\upload_uart.py",
    "$PSScriptRoot\..\target\kernel8.img",
    "--port",
    $Port,
    "--baud",
    $Baud
)

if ($Monitor) {
    $arguments += "--monitor"
}

python @arguments

if ($LASTEXITCODE -ne 0) {
    throw "UART upload failed with exit code $LASTEXITCODE."
}
