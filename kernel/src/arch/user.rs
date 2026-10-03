//! EL1h/EL0 transition for the CPU0-only, synchronous userspace bring-up runner.
//!
//! Address-space ownership, validation and switching live in `userspace`. This
//! module changes only architectural execution state and never writes TTBRs.

use core::arch::global_asm;

use crate::userspace::abi::{self, Errno};
use crate::userspace::space::USER_CODE;

#[cfg(feature = "filesystem")]
global_asm!(include_str!("../../../boot/resume.S"));

#[cfg(feature = "filesystem")]
unsafe extern "C" {
    fn arch_resume_user(frame: *const crate::arch::context::ExceptionFrame) -> u64;
}

/// Resume a frozen kernel-owned frame under its validated task root.
/// # Safety
/// Same CPU0/vector/root requirements as `run`; the complete frame must stay
/// readable until ERET, originate from EL0t, and contain valid EL0 PC/SP state.
#[cfg(feature = "filesystem")]
pub unsafe fn resume(frame: *const crate::arch::context::ExceptionFrame) -> u64 {
    unsafe { arch_resume_user(frame) }
}

global_asm!(
    include_str!("../../../boot/user.S"),
    sys_write = const abi::SYS_WRITE,
    sys_exit = const abi::SYS_EXIT,
    sys_yield = const abi::SYS_YIELD,
    stdout = const abi::STDOUT,
    stderr = const abi::STDERR,
    max_write_bytes = const abi::MAX_WRITE_BYTES,
    svc_immediate = const abi::SVC_IMMEDIATE,
    errno_ebadf = const Errno::BadFileDescriptor as i64,
    errno_efault = const Errno::Fault as i64,
    errno_einval = const Errno::InvalidArgument as i64,
    errno_enosys = const Errno::NotImplemented as i64,
);

unsafe extern "C" {
    fn arch_enter_user(entry: u64, stack_top: u64, argument: u64) -> u64;
    static __user_image_start: u8;
    static __user_image_end: u8;
    static __user_fault_start: u8;
    static __user_fault_end: u8;
    static __user_fault_pc: u8;
    static __user_readonly_start: u8;
    static __user_readonly_end: u8;
    static __user_readonly_pc: u8;
    static __user_guard_start: u8;
    static __user_guard_end: u8;
    static __user_guard_pc: u8;
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

/// Position-independent syscall/error exercise with a cross-page stack buffer
/// and preserved GP/SIMD/FP/SP checks after every returning SVC.
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

/// EL0 address of the kernel-read instruction after copying its image to USER_CODE.
pub fn kernel_fault_pc() -> u64 {
    fault_pc(
        core::ptr::addr_of!(__user_fault_start),
        core::ptr::addr_of!(__user_fault_pc),
    )
}

/// Store into this image's RX mapping to exercise write permission faults.
pub fn readonly_fault_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_readonly_start),
        core::ptr::addr_of!(__user_readonly_end),
    )
}

/// EL0 address of the RX-page store after copying its image to USER_CODE.
pub fn readonly_fault_pc() -> u64 {
    fault_pc(
        core::ptr::addr_of!(__user_readonly_start),
        core::ptr::addr_of!(__user_readonly_pc),
    )
}

/// Store immediately below the four-page bring-up stack, into its guard page.
pub fn guard_fault_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_guard_start),
        core::ptr::addr_of!(__user_guard_end),
    )
}

/// EL0 address of the guard-page store after copying its image to USER_CODE.
pub fn guard_fault_pc() -> u64 {
    fault_pc(
        core::ptr::addr_of!(__user_guard_start),
        core::ptr::addr_of!(__user_guard_pc),
    )
}

/// Run until lower-A64 timer IRQ handling terminates the process.
pub fn timeout_image() -> &'static [u8] {
    image(
        core::ptr::addr_of!(__user_timeout_start),
        core::ptr::addr_of!(__user_timeout_end),
    )
}

fn fault_pc(start: *const u8, instruction: *const u8) -> u64 {
    // The linker label is attached to the faulting instruction inside this
    // image. Relocating the bytes preserves its offset, not its kernel address.
    USER_CODE + (instruction as usize - start as usize) as u64
}

fn image(start: *const u8, end: *const u8) -> &'static [u8] {
    let len = end as usize - start as usize;
    // SAFETY: Each pair delimits immutable linker-owned bytes in .rodata.user.
    // The caller checks the copied image against its one-page code capacity;
    // these position-independent symbols live for the kernel's lifetime.
    unsafe { core::slice::from_raw_parts(start, len) }
}
