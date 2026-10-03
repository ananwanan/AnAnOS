param(
    [string]$Port = "COM5",
    [int]$Baud = 115200,
    [switch]$Monitor,
    [switch]$EnableMmu,
    [switch]$EnableUserspace
)

$ErrorActionPreference = "Stop"

& "$PSScriptRoot\build_windows.ps1" -EnableMmu:$EnableMmu -EnableUserspace:$EnableUserspace

$taskKernelImage = if ($EnableUserspace) { "kernel8-el0.img" } elseif ($EnableMmu) { "kernel8-mmu.img" } else { "kernel8.img" }

$arguments = @(
    "$PSScriptRoot\upload_uart.py",
    "$PSScriptRoot\..\target\$taskKernelImage",
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
