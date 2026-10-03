# M3 syscall follow-up validation

Date: 2026-10-03. Base `91bcb8b`, branch `codex/el0-syscall`.
This continues the M3 MMU/EL0 implementation with a shared syscall path.
It records development-host verification; Raspberry Pi acceptance is pending.

## Implementation

`svc #0` enters the lower-A64 synchronous vector. The vector saves the full
816-byte exception frame on SP_EL1 and uses an independent kernel FP
environment. `userspace/syscall.rs` checks EL0t/SVC origin, decodes the
provisional register ABI and invokes the write/yield kernel primitive. Returning
calls replace only x0 and preserve the post-SVC ELR. Exit passes a signed
32-bit status back to the runner; root restoration and full TLBI precede
reclamation. No scheduler, ELF loader or libc has been added.

The frame layout is centralized in `arch/context.rs`. ABI/errno constants are
injected into the assembly programs from Rust. User-copy mapping and physical
ownership validation, bounded 128-byte reads, and dispatch are production code
shared with host tests. A Busy construction result no longer reclaims another
runner's root/pages.

The Hello diagnostic image contains 12 executed SVCs on its success path:

| Result | Expected count |
| --- | ---: |
| Successful write (hello, empty, cross-page stderr) | 3 |
| EFAULT (kernel pointer, overflow, upper guard crossing) | 3 |
| EBADF (fd 0, empty write) | 1 |
| EINVAL (oversized write, nonzero SVC immediate with exit number) | 2 |
| ENOSYS | 1 |
| yield | 1 |
| exit(0) | 1 |

Every returning SVC checks SP, x9/x18/x19/x29, q0/q31 (both lanes), nonzero
FPCR/FPSR. Runtime acceptance requires exact counters and exit(0). Diagnostics
are printed after returning to the kernel, outside IRQ/SVC handling:

```text
EL0 SVC: calls=12, writes=3, EFAULT=3, EBADF=1, EINVAL=2, ENOSYS=1, yield=1
[ OK ] EL0 HELLO/SVC
```

## IRQ and fault acceptance refinements

`userspace/preflight.rs` is the shared production/host-tested gate. It requires
two newly observed EL1 timer events within `3 * CNTFRQ`, with IRQ unmasked and
no allocated task pages or task-cell borrow. This checks initial delivery and
the one-shot timer handler's rearm. Stale ticks cannot pass; deadline expiry
takes precedence over late success. Zero/overflowing frequency, changed IRQ
mask and an unchanging physical counter produce distinct failures. The latter
uses a one-million-identical-read safety bound for the Pi 4 fast counter, not a
calibrated elapsed-time timeout. Tick loads precede the ordered counter read.

Failure prints original DAIF, CNTFRQ/CNTPCT/CNTP_CVAL/CTL, ticks and the existing
GIC control/priority/enable/pending/group/HPPIR/AHPPIR/RPR getters. The snapshot
temporarily masks IRQ, then restores the caller's mask. It never reads IAR,
acknowledges an IRQ, or reconfigures the timer. No user root is installed.

`userspace/fault.rs` decodes lower-EL Data Abort. Acceptance checks DFSC category
and level, WnR, IL and FnV/S1PTW/CM/EA, plus exact ELR/FAR/SP. Kernel-text reads
must be permission faults at the actual mapped leaf level; user-code stores
must be L3 permission faults, and stack-guard stores L3 translation faults.
The expected ELR uses linker labels on each faulting instruction, relocated to
USER_CODE. `check_windows.ps1` verifies label bounds/alignment and the actual
`ldr x1, [x0]`/`str xzr, [x0]` instruction encodings in the linked ELF. Cause,
expected result and `match=true/false` print only after returning to EL1,
restoring the kernel root and reclaiming task pages.

## Host checks

Windows x86_64, Rust 1.99.0, `aarch64-unknown-none`.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Pass |
| Workspace default/all-features checks | Pass |
| Host kernel library tests | 110 passed, 0 failed |
| Default/MMU/userspace kernel debug builds | Pass |
| Userspace kernel release build | Pass |
| Bootloader debug build/image | Pass |
| `scripts/check_windows.ps1` | Pass |
| Matching ELF architecture, entry, kernel boundaries and 64 KiB stack | Pass |
| All sixteen vector slot branches and optional runner linkage | Pass |
| Five embedded EL0 image sizes within 4 KiB | Pass |
| Three fault-PC labels and AArch64 access opcodes | Pass |
| Raw image generation and `git diff --check` | Pass |

The new host tests exercise the actual dispatcher and copy path: unchanged
ELR/PSTATE/other GPR/SIMD/FP/SP state, wrong trap origins/classes, unsupported
SVC immediates, descriptor/length error precedence, signed exit statuses,
discontiguous physical pages, 4096-byte acceptance/4097-byte rejection, and zero
physical reads/output for invalid or foreign-owned later pages. The existing
physical memory, heap, MMU permissions and allocation rollback tests remain.
The additional fourteen tests cover stale/missing IRQ events, exact deadline
and mask handling, clock/tick wrap, invalid frequency, counter stall/reset,
all EC/DFSC values, wrong fault flags/directions/levels and wrong PC/FAR/SP.

Debug images retain kernel load/entry `0x200000`, bootloader `0x80000`:

| Image | Bytes |
| --- | ---: |
| `target/kernel8.img` | 163840 |
| `target/kernel8-mmu.img` | 241664 |
| `target/kernel8-el0.img` | 286720 |
| `target/bootloader8.img` | 8034 |

## Board acceptance pending

No boot-drive writes, UART upload, QEMU execution or Raspberry Pi output were
performed. Host tests do not execute the embedded assembly images or validate
BCM2711 IRQ delivery. After M1/M2 hardware acceptance, manually run:

```powershell
.\scripts\upload_kernel_uart.ps1 -EnableUserspace -Monitor
```

Capture successful IRQ PREFLIGHT, expected Hello output (`hello through SVC`,
`stack!`), exact SVC counters, exit(0), decoded kernel/RO/guard faults with
`match=true`, three EL0 timer IRQs,
root/page reclamation, HDMI output and subsequent sustained kernel timer ticks.
The preflight proves only EL1 delivery; it cannot recover the infinite EL0
test if interrupts or the counter fail later. Failure-path register snapshots
and all expected fault checks still require real board evidence.
See [userspace checks](../userspace.md) and [provisional ABI](../abi/README.md).

Architectural reference: Arm's [AArch64 Exception Model](https://documentation-service.arm.com/static/67ac57fb091bfc3e0a9479cc),
sections 4.1, 5.1 and 5.2, describes supervisor-call exceptions and exception
entry/return state. These references do not substitute for board evidence.
DFSC and syndrome fields follow the official [Arm ESR tables](https://documentation-service.arm.com/static/60d32e78677cf7536a55bad7)
and [Cortex-A72 TRM](https://documentation-service.arm.com/static/5e7b6c287158f500bd5bfb61),
section 4.3.50.
