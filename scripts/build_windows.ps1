$ErrorActionPreference = "Stop"

# 构建内核
cargo build -p kernel

# 转换为二进制镜像，UART bootloader 会把它加载到 0x80000
rust-objcopy `
    --strip-all `
    -O binary `
    target/aarch64-unknown-none/debug/kernel `
    target/kernel8.img
