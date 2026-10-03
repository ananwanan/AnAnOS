# M2: EL1 translation and memory attributes

The M2 code implements a first identity-mapped EL1 address space for Raspberry
Pi 4B. It uses M1 physical-page ownership, no new dependencies, and keeps data
and instruction caches disabled. Only CPU0 runs. The default image retains the
MMU-off path; Cargo feature `mmu` explicitly selects M2. Real-board acceptance
for both M1 and M2 is pending.

## Current virtual layout

TTBR0 uses 39-bit low virtual addresses, a 4 KiB granule and a level-1 root.
Mapped VAs equal PAs, preserving existing code, stack, DTB and driver pointers.
TTBR1 walks are disabled. Output addresses are limited to 40 bits; unsupported
firmware ranges fail rather than being clipped.

| Region | Permissions / mapping | Memory type |
| --- | --- | --- |
| `0x0..0x1000` | Unmapped null page | None |
| Kernel text starting at `0x200000` | EL1 read-only, executable | Normal non-cacheable |
| Kernel rodata | EL1 read-only, execute-never | Normal non-cacheable |
| Kernel data/BSS, metadata and 64 KiB stack | EL1 read/write, execute-never | Normal non-cacheable |
| Kernel heap and discovered RAM | Physical identity; EL1 read/write, execute-never | Normal non-cacheable |
| Authorized framebuffer pages | Physical identity; EL1 read/write, execute-never | Normal non-cacheable |
| `0xFC00_0000..0x1_0000_0000` | Physical identity; EL1 read/write, execute-never | Device-nGnRnE |
| DT `no-map`, RAM holes and other gaps | Unmapped | None |
| Per-process userspace, user stacks, libraries / mmap | Not installed in M2; all EL0 access is denied | None |

The linker isolates text, rodata and writable data at 4 KiB boundaries. Load
addresses stay `0x200000` for kernel and `0x80000` for bootloader. The heap is
still a 1 MiB physical run. Kernel leaves deny EL0 access/execution; mixed
hierarchical entries leave permissions to leaves. Writable executable mappings are rejected, with
SCTLR.WXN enabled as an additional safeguard.

This is the M2 bring-up layout, not a published userspace ABI. The optional
[M3 path](userspace.md) adds private user pages in a disjoint low-VA window and
per-task TTBR0 roots; AP permissions provide isolation without relocating the
kernel. A high-half/direct map can be introduced later. M2 itself does not
publish a target triple or add userspace VM syscalls.

## Policy and ownership

`mapping.rs` builds a bounded, sorted, disjoint plan in static storage. RAM is
rounded inward; `no-map` and framebuffer spans outward. FDT retains enabled
static/dynamic `no-map` separately from allocation reservations. Kernel or
framebuffer overlap with these holes fails. The explicit MMIO window overrides
any overbroad RAM declaration. A framebuffer outside ARM RAM maps only its
actual firmware-returned span.

Other reserved RAM can have translations but remains unallocatable; mapping
alone does not authorize touching reserved firmware contents. RAM holes stay
unmapped. `parse_into` fills a static DT result to avoid large debug return-value
copies on the unchanged 64 KiB boot stack; errors clear partial/old results.

`paging.rs` prefers aligned 1 GiB/2 MiB blocks and uses 4 KiB boundary pages.
It provides queries, offline unmapping/block splitting, empty-table pruning and
destruction. Collision validation precedes writes; OOM can leave an offline
partial map, which must be discarded. The runtime retains page ownership tokens
for up to 128 tables. Pre-enable failures reclaim every table page; active tables
are frozen for kernel life. Live editing, break-before-make, per-address TLBI,
ASIDs are not implemented. M3 switches complete offline-built roots with full
TLBI and reclaims them only after restoring the kernel root.

## Activation and coherency

M2 runs after M1 page/heap tests and before IRQ enable. It checks CPU0/EL1h,
masked IRQ/FIQ, MMU/cache off, Cortex-A72 MIDR, 4 KiB support and sufficient PA
range. Firmware must set CPUECTLR_EL1.SMPEN before TLB maintenance/MMU enable;
EL1 writes can be restricted by EL2/EL3. Missing SMPEN reports CoherencyDisabled
without attempting a write. [Cortex-A72 TRM](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e), sections 4.3.67 and 5.5.

MAIR index0 is Normal NC `0x44`, index1 Device-nGnRnE `0x00`; table walks are NC
with SH0 configured Inner Shareable. NC/device memory has effective Outer
Shareable behavior. Both caches remain off, with no cacheable alias. Mailbox
buffers keep 16-byte alignment and existing ordering barriers, and MMIO keeps
volatile accesses/widths. [Arm TF-A attributes](https://github.com/ARM-software/arm-trusted-firmware/blob/master/lib/xlat_tables_v2/xlat_tables_core.c).

A pre-enable software walker checks all plan edges, table pages, current stack,
activation code, vectors and required UART/mailbox/GIC pages. Publication uses
DSB SY, MAIR/TCR/TTBR programming, ISB, TLBI VMALLE1, DSB SY/ISB, SCTLR.M/WXN,
and final ISB. SCTLR RES1 and little-endian state are preserved. The extra ISB
also covers Cortex-A72 erratum1387635. [Arm activation sequence](https://github.com/ARM-software/arm-trusted-firmware/blob/master/lib/xlat_tables_v2/aarch64/enable_mmu.S), [official errata](https://documentation-service.arm.com/static/6362f9b7c5a70d2cdb15fe3e).

After M=1, AT S1E1R/W and PAR_EL1 verify identity translation, writable data/devices,
read-only text/rodata and null faults. These probes check data access rather than
instruction-fetch XN; descriptor tests inspect PXN/UXN. Actual heap, zeroed-page
and mailbox roundtrip tests follow. Existing timer/GIC diagnostics remain.
A pre-enable error returns to MMU-off diagnostics; a post-enable error retains
live tables and halts before IRQ enable rather than freeing active tables.

## Build and board check

```powershell
# Tests, all feature checks and all four raw images; no deployment.
.\scripts\check_windows.ps1
# Default image: target/kernel8.img
.\scripts\build_windows.ps1
# Opt-in image: target/kernel8-mmu.img
.\scripts\build_windows.ps1 -EnableMmu
```

Both kernel variants link at `0x200000` and use the same UART protocol. After
verifying M1 with the updated bootloader, upload M2 manually:

```powershell
.\scripts\upload_kernel_uart.ps1 -EnableMmu -Monitor
```

Observe MMU ROOT/table count, EL1 MMU ENABLED (C/I OFF), SCTLR/TCR/MAIR/TTBR0,
AT and memory/mailbox self-tests, HDMI scrolling and sustained timer ticks.
Framebuffer failure must preserve UART diagnostics. Record real logs in
[validation/M2.md](validation/M2.md) before declaring hardware acceptance.
Cache enable requires a separate coherency change and board validation.
