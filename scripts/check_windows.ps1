$ErrorActionPreference = "Stop"

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "Formatting check failed." }
    cargo check --workspace
    if ($LASTEXITCODE -ne 0) { throw "Workspace check failed." }

    # Override the repository's bare-metal target for hardware-independent tests.
    $taskHost = ((rustc -vV | Select-String '^host: ').ToString() -replace '^host: ', '').Trim()
    if ($LASTEXITCODE -ne 0 -or -not $taskHost) { throw "Cannot determine Rust host target." }
    cargo test -p kernel --lib --target $taskHost
    if ($LASTEXITCODE -ne 0) { throw "Host memory tests failed." }

    & "$PSScriptRoot\build_windows.ps1"
    & "$PSScriptRoot\build_bootloader_windows.ps1"

    foreach ($taskImage in @(
        @{ Elf = "kernel"; Entry = "0x200000"; Image = "kernel8.img" },
        @{ Elf = "bootloader"; Entry = "0x80000"; Image = "bootloader8.img" }
    )) {
        $taskHeader = (rust-readobj --file-headers "target/aarch64-unknown-none/debug/$($taskImage.Elf)") -join "`n"
        if ($LASTEXITCODE -ne 0 -or $taskHeader -notmatch 'Machine: EM_AARCH64' -or
            $taskHeader -notmatch "Entry: $($taskImage.Entry)\s") {
            throw "Unexpected ELF architecture or entry for $($taskImage.Elf)."
        }
        $taskImageBytes = [System.IO.File]::ReadAllBytes((Join-Path (Get-Location) "target/$($taskImage.Image)"))
        if ($taskImageBytes.Length -lt 4 -or $taskImageBytes.Length -gt 64MB -or
            ($taskImageBytes[0] -eq 0x7f -and $taskImageBytes[1] -eq 0x45 -and
             $taskImageBytes[2] -eq 0x4c -and $taskImageBytes[3] -eq 0x46)) {
            throw "Expected nonempty raw boot image: $($taskImage.Image)."
        }
    }

    $taskSymbols = rust-nm --defined-only target/aarch64-unknown-none/debug/kernel
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect kernel linker symbols." }
    foreach ($taskSymbol in @("__exception_vectors", "__stack_bottom", "__stack_top")) {
        $taskMatch = $taskSymbols | Select-String "^([0-9a-fA-F]+)\s+\w\s+$taskSymbol$"
        if (-not $taskMatch) { throw "Missing linker symbol $taskSymbol." }
        $taskAddress = [Convert]::ToUInt64($taskMatch.Matches[0].Groups[1].Value, 16)
        $taskAlignment = if ($taskSymbol -eq "__exception_vectors") { 2048 } else { 16 }
        if ($taskAddress % $taskAlignment -ne 0) { throw "Misaligned linker symbol $taskSymbol." }
    }

    git diff --check
    if ($LASTEXITCODE -ne 0) { throw "Diff whitespace check failed." }
    Write-Host "Build, host memory tests and image checks passed. Hardware is unverified."
}
finally {
    Pop-Location
}
