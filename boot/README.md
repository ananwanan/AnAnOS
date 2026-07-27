# Boot

Bootloader for AnanOS.

## Version

### v1.0.0

第一版先完成四件事：

- 只让 CPU0 运行内核。
- 设置内核栈。
- 清空 .bss。
- 跳转到 Rust 的 kernel_main()