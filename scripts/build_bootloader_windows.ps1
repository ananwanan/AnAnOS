$ErrorActionPreference = "Stop"

cargo build -p bootloader
if ($LASTEXITCODE -ne 0) { throw "Bootloader build failed" }

rust-objcopy `
    --strip-all `
    -O binary `
    target/aarch64-unknown-none/debug/bootloader `
    target/bootloader8.img
if ($LASTEXITCODE -ne 0) { throw "Bootloader image generation failed" }
