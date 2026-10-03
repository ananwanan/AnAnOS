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

### 黑屏问题记录

这次启动后黑屏的根因不是 Rust 入口或 framebuffer 绘制逻辑，而是从 EL2 切到 EL1 时，EL1 的执行环境没有被完整初始化。

`eret` 进入 EL1 之前，需要把几个关键寄存器设置好：

- `SP_EL1`：提前设置 EL1 专用栈。`SPSR_EL2` 选择 EL1h 后，CPU 会使用 `SP_EL1`。
- `HCR_EL2`：只保留 `RW = 1`，表示 EL1 运行 AArch64；其他 trap 和虚拟化相关位清零，避免 EL1 的普通访问被 EL2 截获。
- `VTTBR_EL2`：清除二阶段地址转换配置，确保当前裸机阶段没有残留的 Stage-2 翻译状态。
- `CNTHCTL_EL2` 和 `CNTVOFF_EL2`：允许 EL1 读取物理计数器、使用物理定时器，并清零虚拟计数器偏移。
- `CPTR_EL2`：清零后，EL2 不捕获 EL1 的 FP/SIMD 指令。
- `CPACR_EL1`：设置 `FPEN = 0b11`，允许 EL1 使用 FP/SIMD。Rust 编译器或运行时代码可能生成相关指令，如果没有打开，可能触发异常并卡死。
- `SCTLR_EL1`：不要直接写死为一个常量。应先读取固件留下的值，补上 ARMv8-A/Cortex-A72 要求为 1 的 RES1 位，然后只关闭当前阶段暂不使用的 `M`、`C`、`I`，并确保小端序。
- `SPSR_EL2`：设置 `D/A/I/F = 1` 屏蔽异常，设置模式为 `EL1h`。
- `ELR_EL2`：设置为 EL1 入口 `.Lenter_el1`，让 `eret` 后从统一入口继续执行。

进入 `.Lenter_el1` 后，再明确执行：

```
msr     spsel, #1
ldr     x0, =__stack_top
mov     sp, x0
```

这样可以确认当前使用的是 EL1 栈，便于排除栈选择错误导致的早期黑屏。

当前阶段保持 MMU、数据缓存、指令缓存关闭：

```
M = 0
C = 0
I = 0
```

这样 mailbox、UART、framebuffer 等早期设备访问可以先按物理地址工作。后续开启 MMU/cache 时，需要重新检查 mailbox buffer、framebuffer、设备 MMIO 区域的缓存属性和地址映射。

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

## DTB 与物理内存

UART bootloader 会在建栈、清 BSS、接收内核之前，把固件 DTB 复制到
`bootloader/linker.ld` 定义的独立 256 KiB `NOLOAD` 区。链接断言确保
bootloader、DTB 缓冲区和栈都不超过内核加载地址 `0x200000`。
复制后的地址仍通过 `x20` / `x0` 传入 `kernel_main`。

内核从 DTB 识别 RAM 和保留区，将 `[0, __kernel_end)` 连同 DTB、固件
VideoCore/framebuffer 和 MMIO 排除后，再初始化物理页分配器和内核堆。
`__kernel_end` 包含 BSS 中的页位图与 64 KiB 栈。
详见 [内存设计说明](../docs/memory.md)。
