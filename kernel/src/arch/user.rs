//! EL1h/EL0 transition for the CPU0-only, synchronous userspace bring-up runner.
//!
//! Address-space ownership, validation and switching live in `userspace`. This
//! module changes only architectural execution state and never writes TTBRs.

use core::arch::global_asm;

global_asm!(include_str!("../../../boot/user.S"));

unsafe extern "C" {
    fn arch_enter_user(entry: u64, stack_top: u64, argument: u64) -> u64;
    static __user_image_start: u8;
    static __user_image_end: u8;
    static __user_fault_start: u8;
    static __user_fault_end: u8;
    static __user_readonly_start: u8;
    static __user_readonly_end: u8;
    static __user_guard_start: u8;
    static __user_guard_end: u8;
    static __user_timeout_start: u8;
    static __user_timeout_end: u8;
}

/// Execute one EL0 image and return when its exception handler terminates it.
///
/// User IRQ masking inherits the caller's DAIF.I; debug, SError and FIQ remain
/// masked. All user general-purpose and SIMD registers start at zero, except
/// x0 which receives `argument`. The original kernel callee-saved registers,
/// FP environment, SP_EL0 and DAIF are restored before this function returns.
///
/// # Safety
///
/// The caller must run on CPU0 at EL1h with a valid installed vector table and
/// a live address space that maps the runner, vectors, kernel stack and all
/// handler data as EL1-only. `entry` must be executable at EL0 and `stack_top`
/// must be 16-byte aligned with writable EL0 stack pages below it. Only one
/// runner may be active; terminating handlers must return through the lower-EL
/// vector action contract. If IRQs are unmasked, the GIC/timer handler must be
/// initialized and runnable with the user address space installed.
pub unsafe fn run(entry: u64, stack_top: u64, argument: u64) -> u64 {
    unsafe { arch_enter_user(entry, stack_top, argument) }
}

/// Position-independent syscall exercise, including private-stack stores.
pub fn demo_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_image_start),
        core::ptr::addr_of!(__user_image_end),
    )
}

/// Read the address in x0 to exercise EL0 access to an EL1-only mapping.
pub fn kernel_fault_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_fault_start),
        core::ptr::addr_of!(__user_fault_end),
    )
}

/// Store into this image's RX mapping to exercise write permission faults.
pub fn readonly_fault_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_readonly_start),
        core::ptr::addr_of!(__user_readonly_end),
    )
}

/// Store immediately below the four-page bring-up stack, into its guard page.
pub fn guard_fault_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_guard_start),
        core::ptr::addr_of!(__user_guard_end),
    )
}

/// Run until lower-A64 timer IRQ handling terminates the process.
pub fn timeout_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_timeout_start),
        core::ptr::addr_of!(__user_timeout_end),
    )
}

fn image(start: *const u8, end: *const u8) -> &'static [u8] {
    let len = end as usize - start as usize;
    // SAFETY: Each pair delimits immutable linker-owned bytes in .rodata.user.
    // The caller checks the copied image against its one-page code capacity;
    // these position-independent symbols live for the kernel's lifetime.
    unsafe { core::slice::from_raw_parts(start, len) }
}
