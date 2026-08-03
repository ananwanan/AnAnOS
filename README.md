# AnanOS

A tiny operating system written in Rust for Raspberry Pi 4.

Current Target:

- Raspberry Pi 4B
- Cortex-A72
- ARMv8-A
- AArch64

Current Memory Layout:

```
0x80000
│
├──────────────
│ .text
├──────────────
│ .rodata
├──────────────
│ .data
├──────────────
│ .bss
└──────────────
```

CPU:

```

_start

↓

boot.S

↓

kernel_main()

↓

Rust

```


```
arch/timer.rs
    直接操作 Generic Timer 寄存器

arch/exception.rs
    IRQ 入口和分发

time/mod.rs
    ticks、uptime、sleep 等内核时间服务 
```
