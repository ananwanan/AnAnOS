# 构建内核
cargo build -p kernel

# 转换为二进制镜像
rust-objcopy `
    --strip-all `
    -O binary `
    target/aarch64-unknown-none/debug/kernel `
    target/kernel8.img