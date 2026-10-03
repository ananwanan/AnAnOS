$ErrorActionPreference = "Stop"

if (-not (Get-Command rust-objcopy -ErrorAction SilentlyContinue)) {
    throw "rust-objcopy is required. Install cargo-binutils and the llvm-tools component."
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    cargo build -p kernel
    if ($LASTEXITCODE -ne 0) { throw "Kernel build failed with exit code $LASTEXITCODE." }

    # UART bootloader loads this raw image at 0x200000 (kernel linker address).
    rust-objcopy --strip-all -O binary `
        target/aarch64-unknown-none/debug/kernel target/kernel8.img
    if ($LASTEXITCODE -ne 0) { throw "Kernel image generation failed with exit code $LASTEXITCODE." }
}
finally {
    Pop-Location
}
