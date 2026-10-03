# Physical memory and kernel heap bring-up

This is the default no-MMU, CPU0-only foundation milestone. Addresses are physical.
The opt-in [M2 MMU path](mmu.md) uses those same addresses through an identity map;
caches, user address spaces and userspace allocators remain future work.

## Bootstrap sequence

1. Firmware passes the DTB in `x0` to the UART bootloader at `0x80000`.
2. Before using a stack, clearing BSS or receiving an image, CPU0 copies a
   validated DTB (at most 256 KiB) into a dedicated bootloader `NOLOAD` buffer.
   `x20` preserves the relocated address; the kernel receives it in `x0` at
   `0x200000`. A linker assertion keeps the bootloader, DTB and bootloader stack
   entirely below the kernel load address. The UART transfer protocol is unchanged.
3. UART is initialized, then exception vectors are installed before accessing
   firmware pointers or initializing the mailbox and optional framebuffer.
4. `memory::init` builds reservations before resolving DT memory requests. It
   requires a readable valid DTB and a successful VideoCore memory query; an
   error is printed and the existing UART/timer diagnostics continue.
5. RAM and reserved intervals are printed, then the physical page allocator
   initializes without a heap. Its boot self-test allocates two aligned pages,
   clears and checks their full contents, checks volatile writes at the ends,
   frees them and verifies accounting.
6. A 256-page (1 MiB) contiguous run is permanently owned by the kernel heap.
   The registered `GlobalAlloc` is tested for alignment, zeroing, free/reuse and
   a fallible `Vec<u64>` allocation before reporting success.

There is no fallback guessed RAM size when discovery fails. A missing DTB,
invalid reservation, insufficient bitmap capacity or exhausted memory is an
explicit error. In particular, old UART bootloaders that only forward a DTB
pointer cannot protect its contents against an overlapping uploaded image.

DTB physical-pointer access is concentrated in `kernel/src/memory/runtime.rs`.
The boot path requires 8-byte alignment, a 40-byte to 256 KiB blob wholly below
1 GiB, no overlap with the kernel, and full containment in a discovered RAM bank.
These checks and parser validation do not
prove that an arbitrary physical pointer is readable: the firmware/bootloader
must supply readable immutable storage for the entire declared DTB span.

## RAM and reservations

`kernel/src/memory/fdt.rs` is a bounded parser with no external dependencies or
heap use. It supports v16 and v17-compatible DTBs, big-endian 1/2-cell address
and size fields, multiple root memory nodes/banks, and skips disabled nodes.
The actual boot path accepts a minimum 40-byte header, as used by Pi firmware.
Kernel parsing uses `parse_into` with a static result buffer to avoid large
RAM/reservation/no-map return-value copies on the fixed boot stack. Errors clear
the output. The returning API remains available for host tests.
Malformed offsets/tokens/strings, overflow, unsupported cell encodings or
translated reserved-memory addresses stop discovery.
Enabled memory/chosen nodes with `linux,usable-memory*` restrictions are rejected
until a supported restriction policy exists. Reserved-child `no-map`/`reusable`
properties must be empty, unique and mutually exclusive.

Reservations include:

- The FDT header's 64-bit memory reservation map.
- Enabled `/reserved-memory` children, including `no-map` and `reusable` spans.
- The initial ramdisk described by `/chosen/linux,initrd-start` and `-end`.
- `[0, __kernel_end)`: low firmware/secondary-core startup state, the UART
  bootloader and relocated DTB buffer, kernel text/data/BSS, bitmap metadata,
  the kernel's 64 KiB stack, and the gaps between them.
- The complete DTB span, even if it is not in the DT reservation map.
- The firmware-returned VideoCore carveout and complete framebuffer allocation.
- BCM2711 MMIO windows `[0xFC00_0000, 0x1_0000_0000)`, including the GIC.

Dynamic reserved-memory `size` requests (including Linux CMA) are resolved to
an exact aligned free span after all fixed reservations are collected. RAM and
optional `alloc-ranges` bound the candidates; `alignment` is honored with at
least 4 KiB alignment. The chosen span is reserved for this boot, including
requests marked reusable. This does not implement CMA or activate a DMA device.
An unsatisfied request fails discovery rather than silently ignoring it or
reserving the entire allowed placement window.

Raspberry Pi DTS memory `reg` values can be placeholders before firmware fills
them in. Discovery uses the firmware-provided DTB; a source DTS is not a RAM-size
substitute. In particular, the placement window for a dynamic CMA pool may span
the first GiB without making that entire window reserved RAM.

Region sets use at most 128 sorted, merged intervals. Parser depth is limited
to 32 and dynamic requests to 16. These limits are explicit failures. Firmware
data is trusted only for provenance/readability; contents undergo validation.

## Physical page ownership

`PageAllocator` manages 4 KiB pages. RAM is aligned inward; a reservation
touching any byte excludes the entire page. Adjacent RAM spans merge, and an
allocation never crosses a real hole. Compact bank indexes support physical
addresses above 4 GiB without representing MMIO holes in the bitmap.

The three bitmaps record eligible pages, live pages and allocation starts. The
default capacity is 2,097,152 pages (8 GiB of actual RAM), using 768 KiB of BSS
plus small bank metadata. Static `const` initialization and in-place bitmap
updates avoid allocating bitmap-sized temporaries on the boot stack. RAM beyond
capacity is rejected; it is never truncated silently.

`allocate_pages(count, alignment_pages)` returns an owned `PhysicalPages` token.
Alignment is a power of two measured in physical pages. `free_pages(token)`
consumes the complete run and validates its bounds, owner and allocation
boundaries before changing state. Tokens cannot be copied or cloned. The
allocator's address must remain stable while tokens exist. Dropping a token
without explicitly freeing it retains the allocation. The heap's token is held
for the lifetime of the kernel and cannot be freed through callers' tokens.

`allocate_pages` does not zero pages. Callers must initialize them before reading.
`allocate_zeroed_pages` clears the complete run with aligned volatile stores
before returning its ownership token; future userspace must use cleared pages.
`PageStats::total_pages` counts complete discovered RAM pages before reservations;
`reserved_pages`, `allocated_pages` and `free_pages` describe their ownership.

## Kernel heap and synchronization

The first-fit free list stores 16-byte nodes in free heap memory. Allocation
honors `Layout` alignment and rounds size to 16 bytes. Free reinserts in address
order and coalesces neighboring spans; this is a reusable allocator, rather
than a monotonic bump arena. Exhaustion/uninitialized allocation returns null.
The heap has a fixed 1 MiB size in this milestone; it does not grow or return its
backing pages. Prefer fallible allocation for recoverable operations.

Public page allocation stays disabled until both page and heap self-tests pass.
The heap is enabled temporarily for its boot self-test; a failed self-test
disables subsequent global allocations and leaves its backing pages reserved.

All global allocator operations execute on CPU0 with IRQ/FIQ masked, then restore
the saved DAIF state. No exclusive-load/store atomics are used. This is **not**
SMP synchronization. Secondary cores must remain parked. IRQ handlers should
stay allocation-free to bound latency; allocator internals never log or
recursively allocate. Before enabling caches or SMP, revise this contract,
RAM/device attributes and mailbox/framebuffer coherency together.

## Validation and board check

Host tests execute the same pure parser/page/free-list code as the kernel.

The unified Windows check runs these tests, workspace checks, both image builds
and image/linker layout checks without installing or uploading either image:

```powershell
.\scripts\check_windows.ps1
```

The individual commands are:

```powershell
cargo test -p kernel --lib --target x86_64-pc-windows-msvc
cargo fmt --all -- --check
cargo check --workspace
cargo build -p kernel
cargo build -p bootloader
.\scripts\build_windows.ps1
.\scripts\build_bootloader_windows.ps1
```

Host tests and successful image generation do not validate Pi hardware.
The [M1 validation record](validation/M1.md) captures the earlier physical-page
foundation snapshot; its test count and image sizes do not establish validation
of this DTB-relocation and heap implementation. Use fresh check results for the
combined source tree.
Install the updated bootloader through the existing deployment workflow when
ready, then upload the new kernel. Installation writes a removable boot drive
and is deliberately a separate manual step.

Check UART output for relocated `DTB address`, actual `RAM` and `RESERVED` spans,
`PHYSICAL PAGE SELF-TEST`, page counters, `KERNEL HEAP` and
`HEAP ALLOCATION/FREE SELF-TEST`. The heap should account for 256 allocated pages.
Check that the existing GIC state output and recurring timer ticks still appear.
Framebuffer failure must still leave all diagnostics available over UART.

The optional [M3 EL0 runner](userspace.md) holds private code/data/stack and table
tokens, switches back to the kernel root with full TLBI, then reclaims them.
The global page/heap counters must return to their prior values after each task.

`test::test_exception_simd_context()` is an optional board BRK diagnostic after
installing the EL1h vectors. It samples both 64-bit lanes of q0/q1/q31 and
FPCR/FPSR across exception return. It does not run by default and does not replace
sustained timer IRQ validation. The updated bootloader is also required to
observe the relocated DTB and complete UART upload acknowledgement before kernel
UART initialization.

## References

- [Devicetree flattened format and reservation map](https://devicetree-specification.readthedocs.io/en/stable/flattened-format.html)
- [Memory and reserved-memory nodes](https://devicetree-specification.readthedocs.io/en/stable/devicenodes.html)
- [Raspberry Pi firmware property interface](https://github.com/raspberrypi/firmware/wiki/Mailbox-property-interface)
- [BCM2711 device-tree address ranges](https://github.com/raspberrypi/linux/blob/rpi-6.18.y/arch/arm/boot/dts/broadcom/bcm2711.dtsi)
