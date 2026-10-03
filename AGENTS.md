# AGENTS.md

## Purpose

This file defines repository-specific instructions for Codex and other coding agents working on **AnanOS**.

AnanOS is a small bare-metal operating system written in Rust for the Raspberry Pi 4B. Changes must prioritize correctness on real Raspberry Pi 4 hardware over convenience, abstraction, or assumptions inherited from Linux/QEMU environments.

## Repository facts

- Repository: `ananwanan/AnAnOS`
- Default branch: `dev`
- Primary target: Raspberry Pi 4B
- SoC: BCM2711
- CPU: Cortex-A72
- Architecture: ARMv8-A / AArch64
- Rust target: `aarch64-unknown-none`
- Rust edition: 2024
- Kernel style: `#![no_std]`, `#![no_main]`
- Current execution model: CPU0 runs; secondary cores park in `wfe`
- Current kernel runtime EL: EL1h
- Optional `userspace` feature: private EL0t tasks with SVC entry and return to EL1h; see `docs/abi/README.md`
- MMU: disabled in the default build; opt-in `mmu` feature enables an EL1 identity map
- Data cache: disabled
- Instruction cache: disabled

Do not assume `main` is the active branch. Inspect the current branch and existing work before editing.

## Repository layout

```text
.
├── .cargo/
│   └── config.toml              # default target = aarch64-unknown-none
├── arch/                        # workspace crate; currently mostly placeholder
├── boot/
│   ├── boot.S                   # kernel early boot / EL2 -> EL1h transition
│   ├── vectors.S                # AArch64 exception vectors
│   ├── linker.ld                # kernel linked at 0x200000
│   └── README.md                # boot design notes and invariants
├── bootloader/
│   ├── boot.S
│   ├── linker.ld                # UART bootloader linked at 0x80000
│   └── src/main.rs              # receives kernel over Mini UART
├── drivers/                     # workspace crate; currently mostly placeholder
├── kernel/
│   ├── build.rs
│   └── src/
│       ├── arch/
│       │   ├── exception.rs
│       │   └── timer.rs
│       ├── drivers/
│       │   ├── framebuffer.rs
│       │   ├── gic.rs
│       │   ├── mailbox.rs
│       │   └── uart.rs
│       ├── graphics/
│       ├── console.rs
│       ├── main.rs
│       └── test.rs
├── scripts/
│   ├── build_windows.ps1
│   ├── build_bootloader_windows.ps1
│   ├── install_bootloader_usb.ps1
│   ├── upload_kernel_uart.ps1
│   └── upload_uart.py
├── config.txt
├── Cargo.toml
└── rust-toolchain.toml
```

Important: the active architecture and device implementations currently live under `kernel/src/arch` and `kernel/src/drivers`. The top-level `arch` and `drivers` workspace crates are not yet the canonical implementation. Do not move code into those crates merely because their names look more appropriate unless the task explicitly includes that refactor.

## Boot flow

The current boot chain is:

```text
Raspberry Pi firmware
    |
    | loads bootloader8.img
    v
bootloader @ 0x80000
    |
    | receives kernel8.img over Mini UART
    | validates magic / size / checksum
    v
kernel image @ 0x200000
    |
    v
boot/boot.S
    |
    | CPU0 only
    | preserve DTB address
    | EL2 -> EL1h when required
    | initialize SP_EL1
    | clear .bss
    v
kernel_main(dtb_address)
```

The firmware-provided DTB address arrives in `x0`. Early assembly preserves it in `x20`, and it must ultimately be passed to Rust as the first argument to `kernel_main`.

Do not casually change:

- the bootloader link/load address `0x80000`;
- the kernel link/load address `0x200000`;
- DTB preservation through `x0`/`x20`;
- EL1h stack selection;
- 16-byte stack alignment;
- early `.bss` clearing;
- CPU0-only startup behavior.

If any of these must change, update the linker script, assembly, bootloader assumptions, scripts, and documentation together.

## EL2 -> EL1 invariants

`boot/boot.S` deliberately initializes the EL1 environment before `eret`.

Preserve the intent of the existing setup:

- `HCR_EL2.RW = 1`;
- Stage-2 translation disabled;
- `VTTBR_EL2 = 0`;
- EL1 physical counter/timer access enabled through `CNTHCTL_EL2`;
- `CNTVOFF_EL2 = 0`;
- EL2 FP/SIMD trapping disabled;
- EL1 FP/SIMD enabled;
- required `SCTLR_EL1` RES1 bits preserved/set;
- `SCTLR_EL1.M = 0`;
- `SCTLR_EL1.C = 0`;
- `SCTLR_EL1.I = 0`;
- little-endian EL1/EL0;
- `SPSR_EL2` targets EL1h;
- exceptions remain masked until the Rust kernel deliberately enables them.

Do not replace the `SCTLR_EL1` setup with a magic zero value.

When modifying exception-level code, use explicit `dsb` / `isb` barriers where architecturally required. Do not remove existing barriers without a specific reason.

## Memory and MMIO rules

The default kernel runs without an MMU. The opt-in M2 path uses an EL1 identity map with caches disabled, so driver address values remain physical/identical virtual addresses. See `docs/mmu.md` for mapping permissions, `no-map` holes, table ownership and hardware validation requirements. Do not apply the offline page-table mutation API to active tables.

Current important physical addresses include:

```text
BCM2711 peripheral base  0xFE00_0000
GIC-400 Distributor      0xFF84_1000
GIC-400 CPU interface    0xFF84_2000
Kernel image             0x0020_0000
UART bootloader          0x0008_0000
```

For MMIO:

- use `read_volatile` / `write_volatile`;
- do not replace volatile accesses with ordinary references or ordinary loads/stores;
- preserve required ordering barriers;
- keep register widths correct;
- document non-obvious register bits and hardware assumptions;
- avoid broad read-modify-write operations that may disturb firmware-initialized bits unless the hardware requires them.

Because MMU/cache are currently disabled, do not assume normal cache-coherent memory semantics for low-level synchronization. In particular, be cautious with atomics that compile to exclusive load/store loops (`LDXR`/`STXR`). Existing timer IRQ code intentionally avoids `fetch_add` for this reason.

## Interrupt controller

Raspberry Pi 4 uses the GIC-400 / GICv2 interface in this project, not GICv3.

Current timer IRQ:

```text
AArch64 Generic Physical Timer PPI = IRQ 30
```

Important GIC rules:

- do not rewrite the entire Distributor configuration just to configure one PPI;
- preserve firmware-established Distributor state where practical;
- PPI 30 is CPU-private;
- `GICC_IAR` returns the acknowledge value;
- `GICC_EOIR` must receive the complete acknowledge value read from `GICC_IAR`, not just the low interrupt ID;
- interrupt ID `1023` is spurious;
- avoid logging from IRQ handlers unless the task is specifically diagnostic and the consequences are understood.

The current kernel contains deliberate IRQ diagnostics. When changing GIC/timer behavior, preserve useful diagnostics until the hardware issue being investigated is demonstrably resolved.

## Generic timer

The kernel uses the ARM Generic Physical Timer:

- `CNTFRQ_EL0`
- `CNTPCT_EL0`
- `CNTP_CVAL_EL0`
- `CNTP_CTL_EL0`

It does not use a BCM2711 MMIO system timer for the kernel tick.

The current periodic interval is one second. The physical timer is one-shot at the register level, so the IRQ handler schedules the next deadline again.

Do not convert it to a different timer source unless the task explicitly calls for that architectural change.

## UART

Both the UART bootloader and kernel use the Raspberry Pi Mini UART.

Current assumptions:

- GPIO14 = TX
- GPIO15 = RX
- ALT5
- `core_freq = 250 MHz`
- baud rate = 115200
- `AUX_MU_BAUD_REG = 270`

`config.txt` fixes the VPU core clock to keep this baud divisor valid.

If UART clocking or baud configuration changes, update both bootloader/kernel code and `config.txt` consistently.

The USB TTL adapter must be 3.3 V logic. VCC should not be connected when the Raspberry Pi has its own power supply.

## UART bootloader protocol

The bootloader expects:

```text
magic       4 bytes: "ANAN"
size        u32 little-endian
payload     `size` bytes
checksum    u32 little-endian
```

The checksum is the wrapping sum of all payload bytes.

Current maximum kernel image size is 64 MiB.

After a valid image is received, the bootloader jumps to `0x200000` and passes the saved DTB address in `x0`.

Any protocol change must update both `bootloader/src/main.rs` and `scripts/upload_uart.py` in the same change.

## Mailbox

The kernel uses the Raspberry Pi firmware Property Mailbox.

Important constraints:

- message buffers must be 16-byte aligned;
- the mailbox register transports a 32-bit value;
- the low 4 address bits are occupied by the channel number;
- the current VideoCore bus alias is `0xC000_0000`;
- current buffers are expected to remain in low physical memory;
- retain ordering barriers around mailbox requests.

Do not introduce stack/layout changes that silently break message alignment.

## Framebuffer and console

The current kernel requests a firmware framebuffer at 1920x1080 and then installs it as an additional console sink.

Early boot logs must continue to work over UART even if framebuffer initialization fails.

Do not make successful framebuffer initialization a prerequisite for obtaining useful boot diagnostics.

When changing framebuffer code, do not assume width, height, pitch, pixel order, or returned address solely from requested values. Treat firmware-returned values as authoritative.

## Rust constraints

This is bare-metal Rust.

Prefer:

- `core` over `std`;
- small, explicit hardware abstractions;
- `Result` for recoverable initialization failures;
- typed constants for register addresses and bit fields;
- narrowly scoped `unsafe`;
- comments explaining why unsafe hardware operations are valid.

Avoid:

- `std`;
- filesystem/process/network APIs from hosted Rust;
- heap allocation unless an allocator is deliberately introduced;
- hidden global constructors;
- dependencies that require an OS;
- adding crates without checking `no_std` support and whether the dependency is actually necessary.

The kernel has a 1 MiB general-purpose heap initialized after DTB-based RAM/reservation discovery and physical page allocation. Use `Vec`, `String`, `Box`, and other allocation-backed types only after successful `memory::init`; prefer fallible allocation for recoverable failures. Early boot, DTB parsing, and physical page bookkeeping must remain allocation-free. See `docs/memory.md` for ownership, capacity, and CPU0/IRQ synchronization constraints.

## Assembly rules

For AArch64 assembly changes:

- preserve the AAPCS64 stack alignment requirement;
- explicitly document clobbered or preserved architectural state when non-obvious;
- keep boot entry code allocation-free and runtime-independent;
- do not assume exception level unless it has been checked or established;
- use `global_asm!`/`asm!` constraints accurately;
- use `options(nomem)`, `nostack`, and `preserves_flags` only when they are actually true;
- keep linker symbols synchronized with assembly references.

Changes to vector layout must preserve the architectural alignment requirements of `VBAR_EL1`.

## Source of truth

When documentation, comments, and executable code disagree, investigate before editing.

Use the following precedence for current hardware behavior:

1. linker scripts and executable boot/driver code;
2. build/upload scripts and `config.txt`;
3. focused design notes such as `boot/README.md`;
4. root `README.md`;
5. old comments.

Example of a known stale comment: `scripts/build_windows.ps1` says the UART bootloader loads the kernel at `0x80000`, but the actual bootloader loads the kernel at `0x200000` and the kernel linker script is also based at `0x200000`. Do not propagate the stale `0x80000` kernel-load statement.

If you touch that script, correct the comment.

## Build commands

The repository-wide default Rust target is configured in `.cargo/config.toml`.

Before finishing a Rust change, run the relevant checks when the environment supports them:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo build -p kernel
cargo build -p bootloader
```

On Windows, the repository-provided image build commands are:

```powershell
.\scripts\build_windows.ps1
.\scripts\build_bootloader_windows.ps1
```

The kernel build script produces:

```text
target/kernel8.img
```

The bootloader build script produces:

```text
target/bootloader8.img
```

Do not edit files under `target/`.

If `rust-objcopy` is unavailable, report that tooling issue rather than silently substituting a different image format.

## Hardware deployment

Persistent bootloader installation:

```powershell
.\scripts\install_bootloader_usb.ps1 -Drive F:
```

Kernel build/upload/monitor:

```powershell
.\scripts\upload_kernel_uart.ps1 -Monitor
```

Default UART settings in the upload script are currently:

```text
COM5
115200 baud
```

Treat destructive/removable-drive operations as hardware deployment operations. Do not run them automatically.

Do not claim a change is validated on Raspberry Pi hardware unless real UART/HDMI output from the board was actually observed.

A successful Cargo build is build validation, not hardware validation.

## Testing strategy

For low-level changes, validate in layers:

1. formatting and compilation;
2. image generation;
3. UART early-boot output;
4. subsystem-specific diagnostic output;
5. real interrupt/device behavior;
6. only then remove temporary diagnostics if appropriate.

For boot failures, prefer adding minimal UART diagnostics before framebuffer or complex subsystems.

For IRQ failures, inspect and report relevant state such as:

- `DAIF`;
- timer control/pending state;
- `GICD_CTLR`;
- `GICC_CTLR`;
- `GICC_PMR`;
- `GICD_ISENABLER0`;
- `GICD_ISPENDR0`;
- `GICD_IGROUPR0`;
- `GICC_HPPIR`;
- `GICC_AHPPIR`;
- `GICC_RPR`.

Do not treat QEMU success as proof that BCM2711/GIC behavior is correct on real Raspberry Pi 4 hardware.

## Change discipline

Before editing:

1. inspect the files directly involved;
2. read nearby boot/driver documentation;
3. trace address/ABI/register assumptions across assembly, Rust, linker scripts, and scripts;
4. identify whether the behavior is firmware-, CPU-, or BCM2711-specific.

While editing:

- keep patches focused;
- preserve existing diagnostics unless they obstruct the requested change;
- do not perform unrelated refactors;
- do not rename public/internal interfaces without need;
- do not overwrite unrelated user changes;
- keep comments synchronized with behavior;
- prefer fixing the underlying invariant over adding timing delays.

After editing:

- run formatting/build checks that are available;
- summarize exactly what changed;
- distinguish compiled/tested behavior from untested hardware behavior;
- call out any remaining hardware verification steps.

## Style

Follow the existing code style:

- Rust identifiers in English;
- hardware comments may be Chinese or English;
- favor descriptive comments for architectural/hardware reasoning;
- avoid comments that merely restate the code;
- use hexadecimal formatting with separators for important addresses where practical;
- use explicit names for register constants;
- keep boot and driver code straightforward rather than overly generic.

For user-visible boot diagnostics, preserve the existing concise UART-oriented style unless the task is specifically to redesign logging.


## Long-term goal: hosted C, C++, and Rust toolchains

A major long-term project goal is for AnanOS to become capable of running development toolchains **inside AnanOS itself**, including:

```text
gcc
g++
binutils
rustc
cargo
```

This means two different milestones must be kept distinct:

1. **AnanOS as a target**: programs can be cross-compiled on another OS and run on AnanOS.
2. **AnanOS as a host**: GCC/G++/rustc themselves execute on AnanOS and compile programs locally.

The second milestone is much harder and depends on the first. Do not treat "GCC supports AArch64" as meaning GCC can already run on AnanOS. AArch64 code generation already exists; what AnanOS must provide is the operating-system ABI and hosted runtime environment that compiler executables expect.

The intended direction is a Unix/POSIX-like userspace ABI. Exact syscall numbers and ABI details do not need to copy Linux, but compatibility with established Unix conventions is preferred where it substantially reduces porting work.

A future OS/toolchain target name may be something such as:

```text
aarch64-unknown-ananos
```

Do not permanently hard-code or upstream that target triple until the userspace ABI is sufficiently stable. Once published and used by libc/toolchains, target ABI decisions become expensive to change.

### Roadmap overview

Development should broadly proceed in this dependency order:

```text
hardware/kernel foundation
        ↓
physical + virtual memory management
        ↓
EL0 userspace + context switching
        ↓
syscall ABI + process model
        ↓
ELF64 executable loader
        ↓
VFS + filesystems + file descriptors
        ↓
C ABI + libc
        ↓
Binutils / cross GCC
        ↓
hosted C programs
        ↓
libstdc++ / G++
        ↓
Rust target + core/alloc/std
        ↓
native GCC/G++
        ↓
native rustc/cargo
```

Do not skip layers by embedding compiler-specific hacks in the kernel.

### Phase 1: stabilize the kernel foundation

Before serious userspace/toolchain work, the kernel needs reliable low-level facilities:

- exception handling;
- IRQ delivery;
- Generic Timer;
- physical page allocator;
- kernel heap allocator;
- MMU and page-table management;
- cache/MMIO memory attributes;
- safe user-memory access primitives;
- basic synchronization primitives;
- scheduler-ready timer infrastructure.

The current no-MMU phase is appropriate for bring-up, but it is not the final execution environment for hosted compilers.

When the MMU is introduced, define and document a stable virtual address layout for:

```text
kernel text/data
kernel heap
physical-memory mappings
MMIO mappings
per-process userspace
userspace stack
shared libraries / mmap area
```

Do not enable caches before device-memory attributes and DMA/mailbox coherency requirements are understood.

### Phase 2: EL0 userspace

Implement a real userspace execution mode at EL0.

Required pieces include:

- per-process user stack;
- user address-space creation;
- page-table switching;
- kernel/user privilege separation;
- saved register context;
- EL0 -> EL1 syscall entry;
- EL1 -> EL0 exception return;
- process termination;
- fault handling for invalid user accesses;
- copying data safely between user and kernel memory.

Use the AArch64 ABI deliberately. Keep the syscall ABI documented rather than spreading register conventions across unrelated assembly files.

A likely syscall entry mechanism is:

```asm
svc #0
```

The precise syscall-number register and argument registers must be defined once in an ABI document and used consistently by the kernel and libc.

### Phase 3: task, process, and thread model

A compiler is a normal hosted userspace program and needs normal process facilities.

Implement, incrementally:

- task/thread structure;
- process IDs;
- scheduler;
- context switching;
- process address spaces;
- process exit status;
- parent/child relationship;
- `exec` semantics;
- `wait` semantics;
- environment variables;
- command-line arguments;
- current working directory;
- credentials/permissions at least in a minimal form;
- thread-local storage;
- sleeping and wakeups;
- futex-like primitive or another libc-compatible userspace synchronization mechanism.

Eventually provide enough semantics for common POSIX APIs such as:

```text
_exit / exit
getpid
fork or posix_spawn equivalent
execve
waitpid
clone/thread creation
sched_yield
nanosleep
clock_gettime
```

A literal Linux implementation of every syscall is not required. The libc port may translate POSIX APIs to an AnanOS-specific syscall ABI.

For initial bring-up, `fork()` may be deferred if `posix_spawn`/`exec` workflows can support early tooling. For broad Unix software compatibility, process duplication or an equivalent implementation will eventually become important.

### Phase 4: ELF64 userspace ABI

Use ELF64 as the userspace executable format.

Initial loader support should include at least:

- ELF header validation;
- AArch64 machine validation;
- `PT_LOAD`;
- readable/writable/executable page permissions;
- zero-fill for `p_memsz > p_filesz`;
- aligned segment mapping;
- userspace stack setup;
- entry-point transfer.

Start with **statically linked `ET_EXEC` binaries**. This deliberately avoids adding a dynamic linker too early.

Then add:

- PIE / `ET_DYN` executables;
- relocations required by the chosen ABI;
- TLS;
- auxiliary vectors;
- `argc`;
- `argv`;
- `envp`;
- dynamic linking;
- shared libraries.

The initial process stack layout must be documented as part of the userspace ABI. Do not invent a private layout separately for every runtime.

### Phase 5: file descriptors, VFS, and storage

Compiler toolchains perform large amounts of filesystem I/O.

Implement a Unix-like file descriptor layer with at least:

- `stdin`, `stdout`, `stderr`;
- regular files;
- directories;
- pipes;
- terminal/TTY objects;
- file offsets;
- descriptor duplication;
- descriptor flags.

The VFS should eventually support operations corresponding to:

```text
open/openat
close
read
write
pread/pwrite
lseek
fstat/stat
getdents/readdir
mkdir
unlink
rename
chdir
getcwd
dup/dup2
pipe
ioctl
```

A practical sequence is:

```text
embedded initramfs/tarfs
    ↓
RAM filesystem
    ↓
persistent block device
    ↓
writable persistent filesystem
```

Do not block initial userspace work on a complete SD-card or USB storage stack. A built-in initramfs is enough to validate ELF loading and early libc.

Before native compilers become practical, however, AnanOS needs a writable filesystem with:

- directories;
- temporary files;
- atomic-ish rename behavior;
- sufficient file sizes;
- metadata;
- many simultaneous files;
- reasonable performance.

### Phase 6: virtual memory syscalls

Hosted toolchains and modern runtimes need more than a kernel heap.

Provide userspace memory-management operations corresponding to:

```text
brk/sbrk
mmap
munmap
mprotect
anonymous mappings
file-backed mappings
```

Requirements include:

- zero-filled anonymous pages;
- protection enforcement;
- page-aligned mappings;
- userspace address-space isolation;
- correct cleanup at process exit.

Native `rustc`, LLVM, GCC, linkers, and large builds can consume substantial memory. Design VM APIs so they can scale beyond tiny demonstration programs.

### Phase 7: stable userspace ABI

Before porting large toolchains, create an explicit userspace ABI specification, ideally under a future location such as:

```text
docs/abi/
```

Document at least:

- target triple;
- AArch64 calling convention assumptions;
- ELF format;
- userspace virtual-memory layout;
- process entry stack;
- syscall instruction;
- syscall number assignment;
- syscall argument/result registers;
- errno convention;
- signal ABI;
- `struct stat` layout;
- time types;
- file/open flags;
- thread-local storage ABI;
- auxiliary-vector values.

Avoid exposing Rust-specific kernel structs directly as syscall ABI structures.

Use fixed-width ABI types where layout matters.

Once libc and compiler ports depend on an ABI, changes must be handled deliberately rather than silently breaking userspace.

### Phase 8: libc

A usable C library is a prerequisite for a normal hosted C/C++ environment.

The preferred first serious libc candidate is **mlibc**, because it is explicitly designed to be ported to new operating systems through an OS-specific syscall/sysdeps layer and already supports AArch64. A smaller custom libc may still be useful during very early userspace bring-up.

Do not attempt a full glibc port as the first libc milestone.

A staged libc plan is:

#### 8.1 Minimal freestanding C runtime

Provide basic compiler/runtime functions such as:

```text
memcpy
memmove
memset
memcmp
strlen
```

Also provide startup objects/runtime entry code as needed.

This is sufficient for tiny C programs but is not sufficient for GCC itself.

#### 8.2 Minimal hosted libc

Implement enough libc/sysdeps for a statically linked program to do:

```c
int main(int argc, char **argv) {
    printf("hello\n");
    return 0;
}
```

At this point AnanOS should have:

- CRT startup (`crt1.o` or equivalent);
- `errno`;
- file I/O;
- memory allocation;
- time;
- process exit;
- basic environment support.

#### 8.3 POSIX-capable libc

Expand toward:

- pthreads;
- mutexes/condition variables;
- futex backend;
- signals;
- process APIs;
- directory APIs;
- locale basics;
- `poll`/`select`;
- sockets later if desired;
- dynamic-loader integration.

Pin the libc version/commit used for the port. Do not track unstable upstream ABI assumptions blindly.

### Phase 9: sysroot

Maintain an AnanOS sysroot for cross-toolchain work.

Conceptually it should contain:

```text
sysroot/
├── usr/
│   ├── include/
│   ├── lib/
│   │   ├── crt1.o
│   │   ├── crti.o
│   │   ├── crtn.o
│   │   ├── libc.a / libc.so
│   │   ├── libm.a / libm.so
│   │   └── ...
│   └── bin/
└── ...
```

The sysroot is part of the userspace ABI/toolchain boundary. Keep it separate from kernel-private headers.

Do not make GCC consume kernel internal headers directly.

### Phase 10: Binutils port

Before GCC can target AnanOS cleanly, create an OS-specific Binutils target.

The desired tool family will eventually look similar to:

```text
aarch64-unknown-ananos-as
aarch64-unknown-ananos-ld
aarch64-unknown-ananos-ar
aarch64-unknown-ananos-nm
aarch64-unknown-ananos-objcopy
aarch64-unknown-ananos-objdump
aarch64-unknown-ananos-strip
```

Since AArch64 ELF support already exists in Binutils, the main work should be OS target recognition, emulation/defaults, sysroot conventions, linker behavior, and ABI integration rather than implementing a new CPU backend.

Keep any external-toolchain patches outside kernel implementation code, in a clearly separated future location such as:

```text
toolchain/
ports/
patches/
```

### Phase 11: cross GCC for AnanOS

Build an AnanOS-targeting GCC on an existing host before trying to run GCC natively.

Recommended progression:

```text
Binutils target
    ↓
GCC compiler only
    ↓
libgcc
    ↓
libc headers/runtime
    ↓
full C cross compiler
    ↓
C++ frontend
    ↓
libstdc++
```

First acceptance target:

```text
host$ aarch64-unknown-ananos-gcc hello.c -o hello
AnanOS$ ./hello
hello
```

GCC's freestanding mode alone is not the final objective. GCC can compile kernels with `-ffreestanding`, but a hosted compiler environment requires a C library and OS runtime.

### Phase 12: G++ and libstdc++

`g++` frontend support is not enough by itself. Useful C++ requires libstdc++ and runtime support.

Before calling C++ support mature, cover:

- global constructors/destructors;
- `new` / `delete`;
- RTTI;
- exception unwinding;
- `libgcc_s`/unwind behavior or a suitable static equivalent;
- atomics;
- TLS;
- pthread-backed synchronization;
- standard I/O;
- filesystem APIs;
- clocks/time;
- locale behavior at least sufficient for common software.

A milestone should be able to compile and run increasingly demanding examples:

```cpp
#include <iostream>

int main() {
    std::cout << "hello from AnanOS\n";
}
```

Then:

```text
std::string
std::vector
exceptions
std::thread
std::filesystem
```

Do not claim "G++ support" merely because a freestanding C++ file compiles.

### Phase 13: Rust target support

Rust support has several distinct levels.

#### 13.1 `core`

Create an AnanOS Rust target specification and cross-compile `core`.

This is the lowest-level Rust userspace milestone.

#### 13.2 `alloc`

Provide a userspace allocator ABI/runtime and support `alloc`.

Then normal heap-backed Rust types become possible:

```text
Box
Vec
String
Arc
```

#### 13.3 `std`

Port the Rust standard library platform abstraction to AnanOS.

This requires OS facilities for areas such as:

- files;
- directories;
- environment variables;
- processes;
- threads;
- synchronization;
- time;
- TLS;
- networking if full networking APIs are desired;
- dynamic library behavior if supported.

Do not conflate the current kernel's `aarch64-unknown-none` target with a future hosted userspace Rust target. The kernel should remain a freestanding target; userspace should use a distinct AnanOS target.

A future userspace build might conceptually use:

```text
--target aarch64-unknown-ananos
```

until or unless AnanOS becomes an upstream Rust target.

Custom Rust target specifications can change between compiler versions. Pin the Rust toolchain used by the port.

### Phase 14: native GCC/G++

Only after the cross-compiled userspace environment is stable should AnanOS itself become a GCC host.

Native GCC requires substantially more OS functionality than `hello.c`, including:

- reliable processes;
- pipes;
- temporary files;
- shell/tool invocation;
- large writable filesystem;
- virtual memory;
- signals;
- time;
- environment variables;
- robust libc;
- C++ runtime for GCC's own implementation;
- Binutils available on AnanOS.

A staged native goal is:

```text
AnanOS$ gcc hello.c -o hello
AnanOS$ ./hello
hello
```

Then:

```text
AnanOS$ g++ hello.cpp -o hello-cpp
AnanOS$ ./hello-cpp
```

Do not start native GCC bootstrap until the cross GCC can build a substantial AnanOS userspace reliably.

### Phase 15: native rustc

Running `rustc` natively is a later milestone than merely compiling Rust programs for AnanOS.

A native Rust compiler requires:

- AnanOS to be recognized as a Rust **host** platform, not only a target;
- working AnanOS `std`;
- native process spawning;
- filesystem and temporary-file support;
- threads;
- TLS;
- large virtual-memory allocations;
- a linker/toolchain;
- the Rust compiler runtime;
- an AnanOS-capable LLVM/codegen backend build or another supported Rust codegen strategy.

Because the CPU is already AArch64, LLVM does not need a new AArch64 instruction backend, but LLVM/rustc still need a functioning AnanOS host runtime and platform integration.

Native acceptance target:

```text
AnanOS$ rustc hello.rs
AnanOS$ ./hello
hello
```

### Phase 16: Cargo

`cargo` should be treated separately from `rustc`.

For basic offline builds, Cargo needs:

- filesystem;
- process spawning;
- environment variables;
- clocks;
- pipes;
- executable lookup;
- working rustc;
- working linker.

For normal online Cargo workflows, additionally provide:

- TCP/IP networking;
- DNS;
- TLS;
- certificate storage;
- HTTP;
- Git support or an alternative supported registry transport.

Therefore an acceptable early milestone is:

```text
AnanOS$ cargo build --offline
```

before requiring:

```text
AnanOS$ cargo build
```

with online crate downloads.

### Phase 17: shell and build tools

A practical self-hosting development environment also needs tools around the compilers.

Eventually plan for ports of at least:

```text
sh
make or ninja
ar
ld
as
nm
objcopy
objdump
strip
pkg-config-like functionality
```

Later useful tools include:

```text
cmake
git
python
```

These are not kernel requirements, but many real projects and compiler builds depend on them.

Design syscalls and libc for normal Unix software rather than adding one-off kernel APIs for individual build tools.

### Phase 18: dynamic linking

Dynamic linking is not required for the first C/C++/Rust programs.

Prefer this order:

```text
static binaries
    ↓
PIE if useful
    ↓
shared objects
    ↓
runtime dynamic linker
```

A future dynamic loader must handle:

- ELF dynamic sections;
- shared-object loading;
- symbol lookup;
- relocations;
- TLS;
- constructors/destructors;
- library search paths;
- process startup integration.

Keep dynamic linking out of early kernel bring-up unless a concrete dependency requires it.

### Phase 19: self-hosting milestones

Use explicit capability gates instead of declaring vague "compiler support".

#### Gate A — userspace execution

```text
EL0 program runs
SVC syscall works
stdout works
program exits
```

#### Gate B — libc hello world

```text
cross C compiler
        ↓
libc
        ↓
static ELF
        ↓
AnanOS executes it successfully
```

#### Gate C — real cross C environment

AnanOS can run cross-compiled programs using:

```text
malloc/free
files
directories
argv/envp
time
multiple processes
```

#### Gate D — C++

Cross `g++` can build programs using:

```text
iostream
containers
exceptions
threads
```

and those programs run on AnanOS.

#### Gate E — Rust `std`

Cross `rustc` can compile an ordinary hosted Rust program for AnanOS using `std`, and the program runs correctly.

#### Gate F — native C compiler

Inside AnanOS:

```text
gcc hello.c -o hello
./hello
```

works without another OS doing the compilation.

#### Gate G — native C++ compiler

Inside AnanOS:

```text
g++ hello.cpp -o hello
./hello
```

works with hosted libstdc++.

#### Gate H — native Rust compiler

Inside AnanOS:

```text
rustc hello.rs
./hello
```

works.

#### Gate I — native Cargo workflow

Inside AnanOS:

```text
cargo build --offline
cargo run --offline
```

works for a normal local project.

Full online Cargo registry/Git support is a later networking milestone.

### Toolchain architecture rule

Keep these domains separated:

```text
kernel
    = scheduling, VM, syscalls, VFS, devices

userspace ABI
    = ELF, syscall contract, process startup, public types

libc
    = POSIX/C API -> AnanOS syscall translation

toolchain
    = Binutils/GCC/Rust target and host support

applications
    = shell, build tools, editors, user programs
```

Do not put libc policy into the kernel unless it represents a genuine kernel primitive.

Do not implement GCC-specific or Rust-specific syscalls if a general POSIX-style primitive can solve the problem.

### Design decisions that should favor the toolchain goal

When there are several reasonable kernel designs, prefer designs that make a conventional hosted userspace possible.

In particular:

- use ELF64 for userspace;
- keep AAPCS64 compatibility;
- support proper page protections;
- design a scalable VFS/file-descriptor model;
- support normal `argv`/`envp`;
- support TLS;
- provide monotonic and realtime clock concepts;
- design synchronization suitable for pthread/Rust threads;
- provide `mmap`-style virtual memory;
- make pipes and process spawning first-class;
- keep syscall ABI independent of Rust compiler internals;
- preserve the possibility of dynamic linking even if static linking comes first.

The long-term goal is not to turn the kernel itself into a Unix clone. The goal is to expose a sufficiently conventional, documented userspace ABI that established compilers and runtimes can be ported without pervasive source changes.

### Rules for Codex when working toward self-hosting

When a requested change relates to userspace or toolchain support:

1. identify which roadmap phase the task belongs to;
2. do not implement later-phase features by bypassing missing lower layers;
3. prefer a minimal vertical slice that can actually be tested;
4. keep kernel ABI changes documented;
5. add tests or diagnostic programs for new syscalls;
6. test cross-compiled userspace before attempting native tooling;
7. keep static linking as the default early path;
8. avoid premature dynamic-loader complexity;
9. avoid Linux-specific behavior unless intentionally adopted;
10. do not call compiler support complete without meeting the corresponding capability gate.

The preferred progression for new userspace features is:

```text
kernel primitive
    ↓
syscall
    ↓
small userspace test
    ↓
libc wrapper
    ↓
real application/toolchain use
```

This project should optimize for steady, testable progress toward self-hosting rather than jumping directly to large compiler ports.

## Priorities

When trade-offs are necessary, use this order:

1. correct behavior on Raspberry Pi 4B hardware;
2. debuggable early boot and UART visibility;
3. architectural correctness for AArch64/BCM2711/GIC;
4. simple and auditable unsafe code;
5. maintainability;
6. abstraction/generalization.

Do not generalize for hypothetical boards at the cost of making the current Raspberry Pi 4 path harder to understand or debug.

## Completion checklist

Before considering a task complete, check the applicable items:

- [ ] No `std` or accidental allocator dependency was introduced.
- [ ] MMIO accesses remain volatile.
- [ ] Required barriers were preserved.
- [ ] Link/load addresses remain consistent.
- [ ] DTB propagation remains correct.
- [ ] EL transition and stack alignment remain correct.
- [ ] UART assumptions remain synchronized with `config.txt`.
- [ ] Bootloader protocol changes, if any, are synchronized with the uploader.
- [ ] IRQ acknowledge/EOI semantics remain correct.
- [ ] `cargo fmt --all -- --check` passes when available.
- [ ] Relevant Cargo packages build.
- [ ] Generated images are not committed unintentionally.
- [ ] Documentation/comments match executable behavior.
- [ ] Hardware behavior is not claimed without hardware evidence.
