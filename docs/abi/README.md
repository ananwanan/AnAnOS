# Provisional M3 AArch64 userspace ABI

This is the embedded-program bring-up ABI, not a published libc ABI, Linux ABI
or permanent AnanOS target triple. ELF, argc/argv/envp, TLS, signals and dynamic
linking remain later work. Kernel-private Rust structures are never exposed.

## Entry and virtual layout

AArch64 EL0t, little-endian, 16-byte aligned SP_EL0, 4 KiB pages. Each task has
its own TTBR0 root, private physical code/data/stack pages and shared EL1-only
kernel identity mappings. TTBR1 is disabled. TTBR0 remains 39-bit, PA 40-bit;
ASID0/full TLBI is used for every switch. Caches remain off, Normal NC for RAM,
Device-nGnRnE for MMIO. User mappings are non-global.

| User VA span | Purpose | EL0 permissions |
| --- | --- | --- |
| `0x40_0000_0000..0x40_0000_1000` | Copied position-independent code/message | Read/execute, no write |
| `0x40_0001_0000..0x40_0001_1000` | Zeroed private data | Read/write, no execute |
| `0x40_0020_0000..0x40_0020_1000` | Lower stack guard | Unmapped |
| `0x40_0020_1000..0x40_0020_5000` | Zeroed 16 KiB user stack | Read/write, no execute |
| `0x40_0020_5000..0x40_0020_6000` | Upper stack guard | Unmapped |
| Other pages in `0x40_0000_0000..0x40_4000_0000` | Reserved bring-up user window | Unmapped |

Kernel RAM, text, metadata, framebuffer and MMIO all deny EL0 access. User code
also denies EL1 instruction fetch (PXN); user data/stack deny both EL1 and EL0
fetch. Kernel leaves stay UXN. Mixed tables do not apply a blanket hierarchical
EL0 prohibition; leaf AP/PXN/UXN are authoritative.

At first entry GPR/SIMD state is cleared, FPCR/FPSR are zero, and x0 carries the
runner's diagnostic argument (the kernel-address probe in current demos). SP
starts at `0x40_0020_5000`; its contents are zero, with no initial argv stack.
D/A/F stay masked; IRQ masking inherits the initialized kernel's DAIF.I.
SCTLR UMA/DZE/UCI are clear and CNTKCTL_EL1 is zero: EL0 cannot mask IRQs, perform
cache maintenance or reprogram the physical timer to defeat its time budget.

## Syscalls

Instruction `svc #0`; x8 holds the number, x0..x5 arguments, x0 the result.
Results are signed i64 encoded in a u64 register; success is nonnegative and
failure `-errno`. SVC exception ELR already points to the next instruction.
Returning calls preserve x1..x30, SP_EL0, PSTATE, all SIMD registers and
FPCR/FPSR; only x0 is replaced. An exit call has no user return. SVC dispatch
accepts AArch64 EL0t origins only; EL1 exceptions keep their existing handlers.

| Number | Call | Arguments | Result |
| ---: | --- | --- | --- |
| 1 | write | x0 fd (1 stdout / 2 stderr), x1 buffer VA, x2 byte count | Count or negative errno |
| 2 | exit | x0 low signed 32-bit status | Terminates task; no user return |
| 3 | yield | None | 0; resumes the same task in this milestone |

`write` is limited to 4096 bytes per call. The complete range, overflow, page
permissions and physical-page ownership are validated before any console
output. Code is readable; guards, unmapped pages, kernel/MMIO pointers and other
tasks' physical-page aliases are rejected. Copies use privileged Normal NC
identity aliases of owned pages, never an unchecked user VA dereference.
Zero-byte writes validate fd and return zero without touching the pointer.
UART retains its CR/LF convention; the framebuffer's ASCII font substitutes
unsupported bytes. Both descriptors currently use that same early console.

Errors: EBADF=9, EFAULT=14, EINVAL=22, ENOSYS=38. Unknown numbers return ENOSYS;
unsupported SVC immediates return EINVAL. `yield` is a hint and is not a scheduler
or thread API. Current tasks are run sequentially by the kernel.

Validation order is SVC immediate, syscall number, descriptor, length, then
user range/permissions/physical ownership. For example, `write(0, invalid, 0)`
returns EBADF; `write(1, invalid, 0)` returns zero; a 4097-byte write returns
EINVAL before examining the pointer. A valid first page followed by an
unmapped, protected or foreign-owned page returns EFAULT without reading or
emitting any of the buffer. Valid writes copy through a 128-byte kernel buffer.

The ABI constants in `kernel/src/userspace/abi.rs` supply the embedded assembly
programs through `global_asm!` operands. The production SVC dispatcher and
bounded copy implementation live in `kernel/src/userspace/syscall.rs`, shared
by the real vector handler and host tests. Task ownership, root switching and
exit/fault/timeout bookkeeping remain in `runtime.rs`.

## Exceptions, return and resource lifetime

Lower-A64 vectors save 816 bytes on SP_EL1: the original 272-byte GP/ELR/SPSR/ESR
prefix, all q0..q31, FPCR/FPSR, SP_EL0 at800 and FAR_EL1 at808. Kernel handlers
reset their FP environment while retaining the user's saved state. Returning
syscalls/IRQs restore it and ERET to EL0. Other synchronous user exceptions
terminate the task with saved ESR/ELR/FAR/SP diagnostics; kernel faults remain
fatal diagnostics.
Rust's shared kernel-private frame is defined in `kernel/src/arch/context.rs`,
with compile-time size/alignment/offset checks for every assembly field.

The 256-byte suspended kernel runner preserves x19..x30, full q8..q15, kernel
FP state, original SP_EL0 and DAIF. A terminating vector discards its own frame
and returns to that runner at EL1h. No mutable task-cell borrow spans ERET.
Three timer IRQs actually taken from EL0 terminate a non-exiting task. EL1 ticks
between publication and entry do not shorten this budget.
Before allocating or running the demos, an EL1 preflight requires two new timer
events within three counter seconds; failure prints read-only diagnostics and
prevents user entry. This is separate from the actual EL0 IRQ test. Diagnostic
fault programs additionally verify the syndrome subtype/direction/level and
exact instruction/address/stack state; ordinary user faults still terminate
with their original captured diagnostics.

The kernel restores its TTBR0 and completes full TLBI before freeing any task
page or table. All pages are initially zeroed and are reclaimed after exit,
fault or timeout; active descriptors are never edited with the offline mapper.
See [M3 execution and validation](../userspace.md).
