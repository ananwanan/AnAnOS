use core::arch::asm;

/// CPU0-only critical section. Secondary cores must remain parked.
///
/// This avoids exclusive-load/store locks while the MMU and caches are disabled.
/// Nested sections preserve the caller's IRQ mask and leave D/A/F unchanged.
pub fn with_irq_masked<R>(function: impl FnOnce() -> R) -> R {
    struct RestoreIrq(u64);
    impl Drop for RestoreIrq {
        fn drop(&mut self) {
            if self.0 & (1 << 7) == 0 {
                unsafe {
                    // Deliberately no `nomem`: accesses in the section must not
                    // be moved past the compiler memory barrier at IRQ restore.
                    asm!("msr daifclr, #2", "isb", options(nostack, preserves_flags));
                }
            }
        }
    }

    let previous: u64;
    unsafe {
        asm!(
            "mrs {previous}, daif",
            "msr daifset, #2",
            "isb",
            previous = out(reg) previous,
            options(nostack, preserves_flags),
        );
    }
    let restore = RestoreIrq(previous);
    let result = function();
    drop(restore);
    result
}
