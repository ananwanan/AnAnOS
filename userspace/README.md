# Static userspace diagnostic programs

`init.S`, `child.S` and `exec.S` are independently assembled and linked AArch64
programs. They use the provisional syscall ABI in `abi.inc`, checked against
`kernel/src/userspace/abi.rs` by the regeneration script. They have no libc,
hosted runtime, relocations, interpreter or dynamic linker. `linker.ld` places
distinct RX text, R constants and RW data/BSS pages at `0x40_0000_0000`.

The small ELF64 ET_EXEC files and `initramfs.tar` in `images/` are intentional
source-controlled test inputs and initramfs applications. Cargo builds need
no external LLVM installation. They were generated using LLVM clang/LLD 22.1.0:

```powershell
.\scripts\build_userspace_windows.ps1 -UpdateFixtures
```

Without `-UpdateFixtures`, the script rebuilds under `target/userspace/` and
requires byte-identical SHA256 values for all checked-in ELF/tar files.
`-VerifyFixtures` checks `images/manifest.json` against the current normalized
source and fixture hashes without LLVM; it also verifies syscall numbers
against the kernel ABI. Run it in ordinary validation to detect stale fixtures
after a source change. The manifest records the producing toolchain versions.

The deterministic archive contains `bin/`, `bin/init`, `bin/child`, `bin/exec`,
`etc/`, and `etc/message`, mounted under `/init`. It uses strict USTAR headers,
uid/gid/mtime zero, no links or extensions, immutable modes and two zero
terminal blocks. `message.txt` is stored with LF line endings. Kernel host
tests mount the actual archive, parse its ELF payloads and verify permissions,
BSS zeroing and the initial stack.

The initial `init` stack needs at least one nonempty argument and one nonempty
environment string. It checks argc/argv/envp, initialized data and BSS. Before
file operations it verifies that yield preserves x9/x18, both lanes of q0/q31,
FPCR/FPSR, NZCV, SP and its PID, then checks a monotonic clock result has
nanoseconds below one billion. It retains the context markers across blocking
wait while the child runs and execs, and verifies them again before exiting.
It creates
`/tmp/m4/note`, writes/reads with shared dup offsets, examines fstat, cwd and
directory entries, renames/unlinks, then reads `/init/etc/message`. It spawns
`/init/bin/child` with `child-token` and `X=1`; child replaces itself with
`/init/bin/exec` using `exec-token` and `X=1`. The replacement exits 7; init
checks the wait status and exits 0. Init failures exit 101..109, child failures
111..112, replacement failures 121.

Compilation and ELF parser tests do not prove SVC, MMU, scheduler or filesystem
behavior on Raspberry Pi 4. Observe these diagnostics through real board UART
before recording hardware acceptance.
