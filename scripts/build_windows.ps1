param([switch]$EnableMmu, [switch]$EnableUserspace, [switch]$EnableFilesystem)

$ErrorActionPreference = "Stop"

if (-not (Get-Command rust-objcopy -ErrorAction SilentlyContinue)) {
    throw "rust-objcopy is required. Install cargo-binutils and the llvm-tools component."
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $taskBuildArguments = @("build", "-p", "kernel")
    $taskImagePath = "target/kernel8.img"
    if ($EnableFilesystem) {
        $taskBuildArguments += @("--features", "filesystem")
        $taskImagePath = "target/kernel8-fs.img"
    } elseif ($EnableUserspace) {
        $taskBuildArguments += @("--features", "userspace")
        $taskImagePath = "target/kernel8-el0.img"
    } elseif ($EnableMmu) {
        $taskBuildArguments += @("--features", "mmu")
        $taskImagePath = "target/kernel8-mmu.img"
    }
    cargo @taskBuildArguments
    if ($LASTEXITCODE -ne 0) { throw "Kernel build failed with exit code $LASTEXITCODE." }

    # UART bootloader loads this raw image at 0x200000 (kernel linker address).
    rust-objcopy --strip-all -O binary `
        target/aarch64-unknown-none/debug/kernel $taskImagePath
    if ($LASTEXITCODE -ne 0) { throw "Kernel image generation failed with exit code $LASTEXITCODE." }
}
finally {
    Pop-Location
}
