$ErrorActionPreference = "Stop"

if (-not (Get-Command rust-objcopy -ErrorAction SilentlyContinue)) {
    throw "rust-objcopy is required. Install cargo-binutils and the llvm-tools component."
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    cargo build -p bootloader
    if ($LASTEXITCODE -ne 0) { throw "Bootloader build failed with exit code $LASTEXITCODE." }

    # Firmware loads this raw image at 0x80000.
    rust-objcopy --strip-all -O binary `
        target/aarch64-unknown-none/debug/bootloader target/bootloader8.img
    if ($LASTEXITCODE -ne 0) { throw "Bootloader image generation failed with exit code $LASTEXITCODE." }
}
finally {
    Pop-Location
}
