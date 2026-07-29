# Boot

Bootloader for AnanOS.

## v20260720

第一版先完成四件事：

- 只让 CPU0 运行内核。
- 设置内核栈。
- 清空 .bss。
- 跳转到 Rust 的 kernel_main()

## V20260730

- 树莓派固件把我们的内核放在 EL2 启动。我们要在 boot.S 中切换到 EL1h，以后内核、异常向量、页表和驱动都统一运行在 EL1。
- EL2 留给未来的虚拟机管理器。
- Arm 的异常级模型中，操作系统通常运行在 EL1，EL2主要用于虚拟化。

### 为什么选择 EL1h

`SPSR_EL2` 最低四位决定 `eret` 后进入哪种模式：

```
0100 = EL1t，使用 SP_EL0
0101 = EL1h，使用 SP_EL1
```

我们选择

```
EL1h + SP_EL1
```

这样内核和异常处理程序使用 EL1 专属栈，后面 EL0 用户程序可以拥有自己的 `SP_EL0`。

### linker.ld

确保栈顶至少是 16 字节对齐:

```
.stack (NOLOAD) : ALIGN(16)
{
    __stack_bottom = .;
    . += 64K;
    . = ALIGN(16);
    __stack_top = .;
}
```

AArch64 函数调用期间要求栈保持 16 字节对齐。