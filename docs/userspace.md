# M3: MMU to EL0 execution

The optional `userspace` feature includes `mmu` and completes the Gate A code
path: private user mappings, real EL0t entry, SVC output, exit and return to the
kernel, fault isolation, timer IRQ recovery and resource cleanup. Default and
MMU-only images remain available. Caches, scheduler, ELF/VFS and hosted libc are
separate future work. No hardware acceptance is claimed by this document.

Kernel code keeps its current physical identity addresses; EL0 occupies a
disjoint 256 GiB-based window in each root. AP permissions isolate kernel pages
without needing an early high-half relocation. This is a deliberate bring-up
layout; future process/VM work may introduce a kernel direct map. The concrete
entry, memory, syscall and errno contracts are in [abi/README.md](abi/README.md).

## Built-in execution checks

After MMU, GIC and timer initialization, five tiny copied assembly images run
in newly allocated address spaces:

1. Hello uses write/exit/yield, expects EFAULT for a kernel pointer and ENOSYS
   for an unknown call, and checks SP, x19, both lanes of q0/q31 and FP state
   across each SVC. Initial user register/FP state is checked for clearing.
2. A kernel-text read produces a controlled lower-EL data abort.
3. Writing user code produces a read-only protection fault.
4. Writing below the private stack produces a guard-page fault.
5. An infinite loop returns after three actual EL0 timer interrupts.

Software walks and AT S1E0R/W permission probes precede ERET. Completed NC code
writes and IC invalidation precede the first user fetch. Exit/fault/timeout
restore the validated kernel root before reclamation. Page and heap accounting
must return to the previous values after every image. Normal kernel timer
diagnostics then continue, exercising return to the existing EL1 runtime.

## Current-function refinements

- Mapping/descriptor code now distinguishes EL1 and EL0 permissions, preserving
  them through offline block splitting and rejecting writable executable pages.
- Full exception and runner frames preserve integer/SIMD/FP/stack state, including
  a kernel FP environment independent of user-controlled FPCR/FPSR.
- EL0 cannot change DAIF/cache maintenance controls or the kernel physical timer.
- User copy validates the full buffer and physical ownership before output.
- The timeout counts actual lower-EL timer events, avoiding a pending EL1-tick
  race at entry. IRQ handlers perform no console output or allocation.
- Each image is checked against its matching ELF before another build overwrites
  the shared ELF path. Vector slot layout and optional runner linkage are checked.

## Build and manual board check

```powershell
.\scripts\check_windows.ps1
# Builds target/kernel8-el0.img; implies MMU, does not enable caches.
.\scripts\build_windows.ps1 -EnableUserspace
```

After M1/M2 have been accepted on the updated bootloader, manual upload is:

```powershell
.\scripts\upload_kernel_uart.ps1 -EnableUserspace -Monitor
```

Observe M1/MMU self-tests followed by HELLO/SVC, KERNEL ISOLATION, READ-ONLY CODE,
STACK GUARD and EL0 TIMER TIMEOUT results. Expected user faults print captured
ESR/ELR/FAR/SP and return to EL1. The timeout needs three one-second IRQs;
an absent hardware IRQ will leave that program running and requires board-side
diagnosis. Check ADDRESS-SPACE/PAGE RECLAIM, MMU -> EL0 GATE A SELF-TEST, HDMI and
subsequent sustained TIMER TICK output. Record actual logs in
[validation/M3.md](validation/M3.md).

There is one synchronous runner and no general multi-process scheduler. The
timed runner is a bring-up bound, not production scheduling or Unix signals.
User buffers stay private and stable while copied; no SMP, concurrent unmap or
page-fault fixup is implemented. Cache enable must be a separate coherency change.

References: [Arm exception model](https://documentation-service.arm.com/static/67ac57fb091bfc3e0a9479cc), [Cortex-A72 TRM](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e), [Arm address translation](https://documentation-service.arm.com/static/5efa1d23dbdee951c1ccdec5).
