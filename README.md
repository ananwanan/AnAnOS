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
