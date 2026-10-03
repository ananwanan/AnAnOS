param([switch]$UpdateFixtures, [switch]$VerifyFixtures)

$ErrorActionPreference = "Stop"

function Get-NormalizedSourceHash {
    param([string]$Path)
    $taskText = [System.IO.File]::ReadAllText($Path).Replace("`r`n", "`n")
    $taskBytes = [System.Text.Encoding]::UTF8.GetBytes($taskText)
    $taskHash = [System.Security.Cryptography.SHA256]::Create()
    try { return ([System.BitConverter]::ToString($taskHash.ComputeHash($taskBytes))).Replace("-", "") }
    finally { $taskHash.Dispose() }
}

function Set-TarText {
    param([byte[]]$Header, [int]$Offset, [int]$Width, [string]$Value)
    $taskBytes = [System.Text.Encoding]::ASCII.GetBytes($Value)
    if ($taskBytes.Length -gt $Width) { throw "Tar field too long: $Value." }
    [System.Array]::Copy($taskBytes, 0, $Header, $Offset, $taskBytes.Length)
}

function Add-TarEntry {
    param([System.IO.Stream]$Stream, [string]$Name, [byte[]]$Bytes, [switch]$Directory)
    $taskHeader = New-Object byte[] 512
    Set-TarText $taskHeader 0 100 $Name
    $taskMode = if ($Directory) { "0000555" } elseif ($Name.StartsWith("bin/")) { "0000555" } else { "0000444" }
    Set-TarText $taskHeader 100 8 $taskMode
    Set-TarText $taskHeader 108 8 "0000000"
    Set-TarText $taskHeader 116 8 "0000000"
    Set-TarText $taskHeader 124 12 ([Convert]::ToString($Bytes.Length, 8).PadLeft(11, '0'))
    Set-TarText $taskHeader 136 12 "00000000000"
    Set-TarText $taskHeader 148 8 "        "
    $taskHeader[156] = if ($Directory) { 53 } else { 48 }
    Set-TarText $taskHeader 257 6 "ustar"
    Set-TarText $taskHeader 263 2 "00"
    Set-TarText $taskHeader 329 8 "0000000"
    Set-TarText $taskHeader 337 8 "0000000"
    $taskChecksum = ($taskHeader | Measure-Object -Sum).Sum
    Set-TarText $taskHeader 148 8 ([Convert]::ToString([long]$taskChecksum, 8).PadLeft(6, '0') + [char]0 + ' ')
    $Stream.Write($taskHeader, 0, $taskHeader.Length)
    $Stream.Write($Bytes, 0, $Bytes.Length)
    $taskPadding = (512 - $Bytes.Length % 512) % 512
    if ($taskPadding -gt 0) { $Stream.Write((New-Object byte[] $taskPadding), 0, $taskPadding) }
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    if ($VerifyFixtures -and $UpdateFixtures) { throw "Choose -VerifyFixtures or -UpdateFixtures." }
    $taskFixtures = Join-Path (Get-Location) "userspace/images"
    $taskSources = @("userspace/abi.inc", "userspace/init.S", "userspace/child.S", "userspace/exec.S",
                     "userspace/linker.ld", "userspace/message.txt", "scripts/build_userspace_windows.ps1")
    $taskInputs = [ordered]@{}
    foreach ($taskSource in $taskSources) { $taskInputs[$taskSource] = Get-NormalizedSourceHash (Join-Path (Get-Location) $taskSource) }
    $taskAbi = Get-Content "kernel/src/userspace/abi.rs" -Raw
    foreach ($taskConstant in (Get-Content "userspace/abi.inc" | Select-String '^\.equ (SYS_\w+), (\d+)$')) {
        $taskName = $taskConstant.Matches[0].Groups[1].Value
        $taskValue = $taskConstant.Matches[0].Groups[2].Value
        if ($taskAbi -notmatch "pub const ${taskName}: u64 = ${taskValue};") {
            throw "userspace/abi.inc disagrees with kernel ABI: $taskName."
        }
    }
    if ($VerifyFixtures) {
        $taskManifest = Get-Content (Join-Path $taskFixtures "manifest.json") -Raw | ConvertFrom-Json
        foreach ($taskSource in $taskSources) {
            if ($taskManifest.sources.$taskSource -ne $taskInputs[$taskSource]) {
                throw "Fixture source changed; run -UpdateFixtures and review generated files: $taskSource."
            }
        }
        foreach ($taskFixture in @("init.elf", "child.elf", "exec.elf", "initramfs.tar")) {
            $taskActual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $taskFixtures $taskFixture)).Hash
            if ($taskManifest.fixtures.$taskFixture -ne $taskActual) { throw "Fixture hash mismatch: $taskFixture." }
        }
        Write-Host "Checked-in userspace source/ELF/initramfs SHA256 provenance verified (no LLVM required)."
        return
    }
    $taskClang = Get-Command clang -ErrorAction Stop
    $taskLinker = Get-Command ld.lld -ErrorAction Stop
    $taskOutput = Join-Path (Get-Location) "target/userspace"
    New-Item -ItemType Directory -Force -Path $taskOutput | Out-Null
    foreach ($taskProgram in @("init", "child", "exec")) {
        $taskObject = Join-Path $taskOutput "$taskProgram.o"
        $taskElf = Join-Path $taskOutput "$taskProgram.elf"
        & $taskClang.Source --target=aarch64-none-elf -march=armv8-a `
            -I userspace -c "userspace/$taskProgram.S" -o $taskObject
        if ($LASTEXITCODE -ne 0) { throw "AArch64 assembly failed: $taskProgram." }
        & $taskLinker.Source -m aarch64elf --build-id=none --strip-all `
            -z max-page-size=4096 -T userspace/linker.ld $taskObject -o $taskElf
        if ($LASTEXITCODE -ne 0) { throw "Static ELF link failed: $taskProgram." }
    }
    $taskTar = Join-Path $taskOutput "initramfs.tar"
    $taskStream = [System.IO.File]::Create($taskTar)
    try {
        Add-TarEntry -Stream $taskStream -Name "bin/" -Bytes @() -Directory
        foreach ($taskProgram in @("init", "child", "exec")) {
            Add-TarEntry -Stream $taskStream -Name "bin/$taskProgram" -Bytes ([System.IO.File]::ReadAllBytes((Join-Path $taskOutput "$taskProgram.elf")))
        }
        Add-TarEntry -Stream $taskStream -Name "etc/" -Bytes @() -Directory
        $taskMessage = [System.IO.File]::ReadAllText((Join-Path (Get-Location) "userspace/message.txt")).Replace("`r`n", "`n")
        Add-TarEntry -Stream $taskStream -Name "etc/message" -Bytes ([System.Text.Encoding]::UTF8.GetBytes($taskMessage))
        $taskStream.Write((New-Object byte[] 1024), 0, 1024)
    }
    finally { $taskStream.Dispose() }
    $taskHashes = [ordered]@{}
    foreach ($taskFixture in @("init.elf", "child.elf", "exec.elf", "initramfs.tar")) {
        $taskGenerated = Join-Path $taskOutput $taskFixture
        $taskGeneratedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $taskGenerated).Hash
        $taskHashes[$taskFixture] = $taskGeneratedHash
        if ($UpdateFixtures) {
            New-Item -ItemType Directory -Force -Path $taskFixtures | Out-Null
            # Write the complete file before replacing the checked-in fixture.
            $taskStaged = Join-Path $taskFixtures "$taskFixture.new"
            Copy-Item -LiteralPath $taskGenerated -Destination $taskStaged
            Move-Item -LiteralPath $taskStaged -Destination (Join-Path $taskFixtures $taskFixture) -Force
        } else {
            $taskExpected = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $taskFixtures $taskFixture)).Hash
            if ($taskExpected -ne $taskGeneratedHash) { throw "Regenerated fixture differs: $taskFixture. Review and use -UpdateFixtures." }
        }
        Write-Host "$taskFixture SHA256 $taskGeneratedHash"
    }
    if ($UpdateFixtures) {
        $taskClangVersion = (& $taskClang.Source --version | Select-Object -First 1)
        $taskLinkerVersion = (& $taskLinker.Source --version | Select-Object -First 1)
        $taskManifest = [ordered]@{ format = 1; clang = $taskClangVersion; linker = $taskLinkerVersion; sources = $taskInputs; fixtures = $taskHashes }
        $taskManifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $taskFixtures "manifest.json") -Encoding ASCII
    }
    Write-Host "Static AArch64 ELF/initramfs fixtures built and verified. Raspberry Pi execution is unverified."
}
finally { Pop-Location }
