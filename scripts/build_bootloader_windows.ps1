$ErrorActionPreference = "Stop"

cargo build -p bootloader

rust-objcopy `
    --strip-all `
    -O binary `
    target/aarch64-unknown-none/debug/bootloader `
    target/bootloader8.img
