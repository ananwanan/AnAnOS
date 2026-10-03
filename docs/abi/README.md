# Provisional M3/M4 AArch64 userspace ABI

This is a bring-up ABI, not a published libc ABI, Linux ABI or permanent AnanOS
target triple. M3 retains its embedded-program entry; the optional `filesystem`
feature adds the M4 ELF/process/file contract below. TLS, signals and dynamic
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

## M4 static ELF entry

M4 maps the same private user window and four-page guarded stack. Instead of
the M3 copied code/data addresses, sorted non-overlapping ELF PT_LOAD ranges
determine user text/rodata/data addresses. Each owns separate physical pages;
W^X, EL0 AP and PXN/UXN rules remain identical. Initial GPR/SIMD/FP state is zero.
SP is 16-byte aligned and points at this little-endian u64 word sequence:

```text
argc
argv[0] ... argv[argc-1], NULL
envp[0] ... envp[envc-1], NULL
AT_PAGESZ (6), 4096
AT_ENTRY (9), ELF entry
AT_NULL (0), 0
```

argv/envp values are user pointers to NUL-terminated strings stored higher in
the same stack. Strings and arrays never point into kernel memory. There is no
loader pointer in x0, no TLS and no random/capability auxv yet. See the
[System V ELF program-header specification](https://refspecs.linuxfoundation.org/elf/gabi4%2B/ch5.pheader.html)
for PT_LOAD, zero-fill, alignment and segment permissions. M4 additionally
rejects overlapping rounded pages, dynamic/interpreter/TLS segments, writable
code, non-readable segments and entries outside initialized executable bytes.

## M4 syscall extension (`filesystem` feature)

The register/SVC/error contract above is unchanged. `write` now accepts any
open writable descriptor in the current process; zero-byte calls still check
descriptor/access before skipping the pointer. Filesystem and process calls
return ENOSYS in the standalone M3 image. Paths are `(pointer, byte length)`
without a NUL terminator; embedded NUL is invalid. Maximum path length is 256,
component length 48; read/write/getdents buffers are bounded to 4096 bytes.

| Number | Call | x0..x5 arguments | Result |
| ---: | --- | --- | --- |
| 4 | read | fd, destination, length | Read count; regular-file EOF=0 |
| 5 | open | path, length, flags | Lowest free FD |
| 6 | close | fd | 0 |
| 7 | lseek | fd, signed i64 offset, whence (0 SET/1 CUR/2 END) | New offset |
| 8 | fstat | fd, destination | 0, writes 32-byte stat |
| 9 | getdents | directory FD, destination, capacity | 80 or EOF=0 |
| 10 | mkdir | path, length | 0 |
| 11 | unlink | path, length | 0; empty unreferenced directories supported |
| 12 | rename | old path, old length, new path, new length | 0 |
| 13 | chdir | path, length | 0 |
| 14 | getcwd | destination, capacity | Bytes including trailing NUL |
| 15 | dup | fd | Lowest free FD, shared open description |
| 16 | dup2 | fd, target FD | Target FD, atomically replaces its old reference |
| 17 | getpid | None | Current nonzero PID |
| 18 | spawn | path, length, argv pairs, argc, envp pairs, envc | Child PID |
| 19 | exec | path, length, argv pairs, argc, envp pairs, envc | Success does not return; error preserves old process |
| 20 | waitpid | PID or -1 (any), status pointer or 0, options | Child PID or WNOHANG=0 |
| 21 | clock_gettime | clock ID (1 MONOTONIC), destination | 0, writes timespec |

Public open flags: access `O_RDONLY=0`, `O_WRONLY=1`, `O_RDWR=2`, plus
`O_CREAT=0x40`, `O_EXCL=0x80`, `O_TRUNC=0x200`, `O_APPEND=0x400`,
`O_DIRECTORY=0x10000`. Unknown bits/access3, EXCL without CREAT and readonly
TRUNC/APPEND fail EINVAL. These bits are explicitly translated to private VFS
flags. Terminal read polls UART and returns EAGAIN if no input; lseek on a
terminal returns ESPIPE. Directory seeks use enumeration cookies, not bytes;
seek(0,SET) rewinds. getdents emits one fixed 80-byte record per call and requires
capacity>=80, validating the complete requested capacity before consuming a
cookie. Entries include `.` and `..`.

ABI outputs have fixed-width little-endian fields; they never expose Rust layout:

| Object | Byte offsets |
| --- | --- |
| stat (32 bytes) | inode u64@0, size u64@8, kind u32@16, mode u32@20, nlink u64@24 |
| dirent (80 bytes) | inode u64@0, kind u32@8, name length u32@12, zero-padded name bytes[64]@16 |
| timespec (16 bytes) | seconds i64@0, nanoseconds i64@8 (0..999999999) |
| wait status (4 bytes) | signed i32, `(exit & 255)<<8`; fault=11, diagnostic timeout=9 |
| spawn/exec string pair | address u64@0, byte length u64@8 |

Kinds: regular=1, directory=2, character terminal=3. Mode combines POSIX file
type bits with bootstrap read/write permissions; credentials enforcement has
not been implemented. An unlinked open file reports nlink=0. argv/envp pair
arrays each have at most eight entries; strings each have at most 255 bytes and
no embedded NUL. The kernel copies all strings before ELF construction, then
builds the normal NUL-terminated initial stack. Zero arrays ignore their pointer.
No argument/environment inheritance is implicit; cwd/FD inheritance is explicit
spawn behavior. No close-on-exec flag, fork, process groups or signal delivery yet.

wait options are 0 (block) or 1 (WNOHANG); no matching child returns ECHILD.
Invalid status memory never reaps a completed child. Waiting saves the complete
post-SVC frame and lets ready children run; exit wakes the parent. exec preserves
PID/cwd/descriptors but resets the register/FP state and timer budget. Yield and
Generic Physical Timer IRQs select ready processes through the CPU0 runner.
The kernel root/full TLBI are restored before any old page or FD is reclaimed.

Added errno values: ENOENT=2, EIO=5, ENOEXEC=8, ECHILD=10, EAGAIN=11, ENOMEM=12,
EACCES=13, EBUSY=16, EEXIST=17, ENOTDIR=20, EISDIR=21, EMFILE=24, EFBIG=27,
ENOSPC=28, ESPIPE=29, EROFS=30, ERANGE=34, ENAMETOOLONG=36, ENOTEMPTY=39.
Existing EBADF/EFAULT/EINVAL/ENOSYS retain their values and signed return convention.
See [M4 implementation](../filesystem.md) for capacity and hardware boundaries.
