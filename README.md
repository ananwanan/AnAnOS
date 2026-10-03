# AnanOS

A tiny operating system written in Rust for Raspberry Pi 4.

Physical RAM discovery, reservations, the 4 KiB page allocator and the 1 MiB
kernel heap are described in [docs/memory.md](docs/memory.md), including host
tests and the separate Raspberry Pi hardware verification steps.

Implementation milestones and acceptance gates are tracked in
[docs/ROADMAP.md](docs/ROADMAP.md). Run
`.\scripts\check_windows.ps1` for host logic tests, workspace checks and both
boot images; this does not deploy to a board or prove hardware behavior.

Current Target:

- Raspberry Pi 4B
- Cortex-A72
- ARMv8-A
- AArch64

Current Memory Layout:

```
0x200000
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

UART boot flow:

```
PC
 │ USB TTL
 ▼
Raspberry Pi
 ├── bootloader8.img at 0x80000
 └── kernel8.img uploaded to 0x200000
```

Install the persistent UART bootloader to the USB boot partition:

```powershell
.\scripts\install_bootloader_usb.ps1 -Drive F:
```

Build and upload the kernel over USB TTL:

```powershell
.\scripts\upload_kernel_uart.ps1 -Monitor
```

Wiring:

```text
USB TTL GND -> Raspberry Pi GND
USB TTL TXD -> Raspberry Pi GPIO15 / RXD
USB TTL RXD -> Raspberry Pi GPIO14 / TXD
```

Use a 3.3V USB TTL adapter. Do not connect the adapter VCC when the Raspberry Pi
has its own power supply.
