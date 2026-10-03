$ErrorActionPreference = "Stop"

function Test-AnanImage {
    param([string]$Elf, [string]$Image, [string]$Entry, [switch]$Kernel, [switch]$Userspace)
    $taskElfPath = "target/aarch64-unknown-none/debug/$Elf"
    $taskHeader = (rust-readobj --file-headers $taskElfPath) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $taskHeader -notmatch 'Machine: EM_AARCH64' -or
        $taskHeader -notmatch "Entry: $Entry\s") {
        throw "Unexpected ELF architecture or entry for $Image."
    }
    $taskBytes = [System.IO.File]::ReadAllBytes((Join-Path (Get-Location) "target/$Image"))
    if ($taskBytes.Length -lt 4 -or $taskBytes.Length -gt 64MB -or
        ($taskBytes[0] -eq 0x7f -and $taskBytes[1] -eq 0x45 -and
         $taskBytes[2] -eq 0x4c -and $taskBytes[3] -eq 0x46)) {
        throw "Expected nonempty raw boot image: $Image."
    }
    if (-not $Kernel) { return }
    $taskNm = rust-nm --defined-only $taskElfPath
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect kernel symbols for $Image." }
    $taskSymbolAddresses = @{}
    foreach ($taskSymbol in @("__exception_vectors", "__stack_bottom", "__stack_top",
                             "__text_start", "__text_end", "__rodata_start", "__rodata_end",
                             "__data_start", "__data_end", "__kernel_end")) {
        $taskMatch = $taskNm | Select-String "^([0-9a-fA-F]+)\s+\w\s+$taskSymbol$"
        if (-not $taskMatch) { throw "Missing linker symbol $taskSymbol in $Image." }
        $taskAddress = [Convert]::ToUInt64($taskMatch.Matches[0].Groups[1].Value, 16)
        $taskSymbolAddresses[$taskSymbol] = $taskAddress
        $taskAlignment = if ($taskSymbol -eq "__exception_vectors") { 2048 } else { 4096 }
        if ($taskAddress % $taskAlignment -ne 0) { throw "Misaligned linker symbol $taskSymbol." }
    }
    if ($taskSymbolAddresses["__stack_top"] - $taskSymbolAddresses["__stack_bottom"] -ne 64KB) {
        throw "Kernel stack must remain 64 KiB."
    }
    $taskVector = $taskSymbolAddresses["__exception_vectors"]
    $taskInstructions = rust-objdump -d --no-show-raw-insn `
        "--start-address=0x$($taskVector.ToString('x'))" `
        "--stop-address=0x$(($taskVector + 2048).ToString('x'))" $taskElfPath
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect exception vectors." }
    foreach ($taskSlot in 0..15) {
        $taskSlotAddress = ($taskVector + 128 * $taskSlot).ToString('x')
        if (-not ($taskInstructions | Select-String "^\s*$($taskSlotAddress):\s+b\s")) {
            throw "Vector slot $taskSlot is not a branch at its required 128-byte boundary."
        }
    }
    $taskUserEntry = $taskNm | Select-String '\sarch_enter_user$'
    if ($Userspace) {
        if (-not $taskUserEntry) { throw "Missing EL0 runner in $Image." }
        foreach ($taskHandler in @("rust_user_sync_exception", "rust_user_irq_exception", "arch_user_return")) {
            if (-not ($taskNm | Select-String "\s$taskHandler$")) { throw "Missing $taskHandler." }
        }
        foreach ($taskStem in @("image", "fault", "readonly", "guard", "timeout")) {
            $taskStart = $taskNm | Select-String "^([0-9a-fA-F]+)\s+\w\s+__user_$($taskStem)_start$"
            $taskEnd = $taskNm | Select-String "^([0-9a-fA-F]+)\s+\w\s+__user_$($taskStem)_end$"
            if (-not $taskStart -or -not $taskEnd) { throw "Missing EL0 $taskStem image bounds." }
            $taskStartAddress = [Convert]::ToUInt64($taskStart.Matches[0].Groups[1].Value, 16)
            $taskEndAddress = [Convert]::ToUInt64($taskEnd.Matches[0].Groups[1].Value, 16)
            if ($taskEndAddress -le $taskStartAddress -or $taskEndAddress - $taskStartAddress -gt 4096) {
                throw "EL0 $taskStem image must fit in one page."
            }
            if ($taskStartAddress % 4 -ne 0 -or $taskEndAddress % 4 -ne 0 -or
                $taskStartAddress -lt $taskSymbolAddresses["__rodata_start"] -or
                $taskEndAddress -gt $taskSymbolAddresses["__rodata_end"]) {
                throw "EL0 $taskStem image must be instruction-aligned immutable rodata."
            }
            $taskSymbolAddresses["__user_$($taskStem)_start"] = $taskStartAddress
            $taskSymbolAddresses["__user_$($taskStem)_end"] = $taskEndAddress
        }
        foreach ($taskFault in @(
            @{ Stem = "fault"; Opcode = "f9400001"; Instruction = 'ldr\s+x1,\s*\[x0\]' },
            @{ Stem = "readonly"; Opcode = "f900001f"; Instruction = 'str\s+xzr,\s*\[x0\]' },
            @{ Stem = "guard"; Opcode = "f900001f"; Instruction = 'str\s+xzr,\s*\[x0\]' }
        )) {
            $taskStem = $taskFault.Stem
            $taskPc = $taskNm | Select-String "^([0-9a-fA-F]+)\s+\w\s+__user_$($taskStem)_pc$"
            if (-not $taskPc) { throw "Missing EL0 $taskStem fault instruction label." }
            $taskPcAddress = [Convert]::ToUInt64($taskPc.Matches[0].Groups[1].Value, 16)
            if ($taskPcAddress % 4 -ne 0 -or
                $taskPcAddress -lt $taskSymbolAddresses["__user_$($taskStem)_start"] -or
                $taskPcAddress + 4 -gt $taskSymbolAddresses["__user_$($taskStem)_end"]) {
                throw "EL0 $taskStem fault instruction must be aligned and inside its image."
            }
            # The copied EL0 images are stored as data: -D also disassembles .rodata.
            $taskFaultInstruction = rust-objdump -D `
                "--start-address=0x$($taskPcAddress.ToString('x'))" `
                "--stop-address=0x$(($taskPcAddress + 4).ToString('x'))" $taskElfPath
            if ($LASTEXITCODE -ne 0 -or -not ($taskFaultInstruction | Select-String `
                "^\s*$($taskPcAddress.ToString('x')):\s+$($taskFault.Opcode)\s+$($taskFault.Instruction)\s*$")) {
                throw "EL0 $taskStem fault label does not identify its expected AArch64 access instruction."
            }
        }
    } elseif ($taskUserEntry) {
        throw "EL0 runner must only be linked into the userspace feature image."
    }
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "Formatting check failed." }
    cargo check --workspace --all-features
    if ($LASTEXITCODE -ne 0) { throw "Workspace feature check failed." }
    $taskHost = ((rustc -vV | Select-String '^host: ').ToString() -replace '^host: ', '').Trim()
    if ($LASTEXITCODE -ne 0 -or -not $taskHost) { throw "Cannot determine Rust host target." }
    cargo test -p kernel --lib --target $taskHost
    if ($LASTEXITCODE -ne 0) { throw "Host memory/userspace tests failed." }

    # Check the matching ELF before the next build overwrites the common path.
    & "$PSScriptRoot\build_windows.ps1"
    Test-AnanImage -Elf kernel -Image kernel8.img -Entry 0x200000 -Kernel
    & "$PSScriptRoot\build_windows.ps1" -EnableMmu
    Test-AnanImage -Elf kernel -Image kernel8-mmu.img -Entry 0x200000 -Kernel
    & "$PSScriptRoot\build_windows.ps1" -EnableUserspace
    Test-AnanImage -Elf kernel -Image kernel8-el0.img -Entry 0x200000 -Kernel -Userspace
    & "$PSScriptRoot\build_bootloader_windows.ps1"
    Test-AnanImage -Elf bootloader -Image bootloader8.img -Entry 0x80000
    git diff --check
    if ($LASTEXITCODE -ne 0) { throw "Diff whitespace check failed." }
    Write-Host "Host tests, all image variants, vectors, layout and fault instruction checks passed. Hardware is unverified."
}
finally { Pop-Location }
