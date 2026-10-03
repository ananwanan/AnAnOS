use crate::arch::timer;
use crate::drivers::gic::{GENERIC_PHYSICAL_TIMER_IRQ, Gic, SPURIOUS_IRQ};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);

pub use super::context::ExceptionContext;

unsafe extern "C" {
    static __exception_vectors: u8;
}

pub fn init() {
    let vector_address = core::ptr::addr_of!(__exception_vectors) as u64;

    unsafe {
        asm!(
            "msr vbar_el1, {address}",
            "isb",
            address = in(reg) vector_address,
            options(nostack, preserves_flags),
        );
    }

    crate::println!("[ OK ] EXCEPTION VECTOR TABLE: {vector_address:#018x}");
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_sync_exception(context: &mut ExceptionContext) {
    let esr = context.esr_el1;
    let exception_class = (esr >> 26) & 0x3f;
    /// BRK 的立即数只需要低 16 位，高 14 位为 0。
    let iss = esr & 0xffff;
    let brk_comment = iss & 0xffff;

    let far: u64;

    unsafe {
        asm!(
            "mrs {value}, far_el1",
            value = out(reg) far,
            options(nomem, nostack, preserves_flags),
        );
    }

    crate::println!();
    crate::println!("================================");
    crate::println!("       SYNC EXCEPTION");
    crate::println!("================================");
    crate::println!("ESR_EL1  : {esr:#018x}");
    crate::println!("EC        : {exception_class:#04x}");
    crate::println!("ISS       : {iss:#010x}");
    crate::println!("ELR_EL1   : {:#018x}", context.elr_el1);
    crate::println!("SPSR_EL1  : {:#018x}", context.spsr_el1);

    match exception_class {
        /*
         * BRK instruction executed in AArch64.
         */
        0x3c => {
            crate::println!("TYPE      : AARCH64 BRK");
            crate::println!("COMMENT   : {:#06x}", iss & 0xffff);

            /*
             * AArch64 指令固定为 4 字节。
             * 跳过触发异常的 BRK 指令。
             * BRK 被执行时，ELR_EL1 指向触发异常的那条指令。
             * 如果不加 4，eret 后会再次执行同一条 BRK，形成无限异常循环。
             */
            context.elr_el1 = context.elr_el1.wrapping_add(4);

            crate::println!("ACTION    : SKIP BRK AND CONTINUE");
        }

        _ => {
            crate::println!("TYPE      : UNHANDLED");

            loop {
                unsafe {
                    core::arch::asm!("wfe");
                }
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_irq_exception(_context: &mut ExceptionContext) {
    let gic = Gic::new();

    let acknowledge = gic.acknowledge();
    let interrupt_id = acknowledge & 0x3ff;

    match interrupt_id {
        GENERIC_PHYSICAL_TIMER_IRQ => {
            // 先设置下一次截止时间，解除当前电平中断条件。
            timer::schedule_next_interrupt();

            /*
             * MMU 关闭时，真机不保证 fetch_add 使用的 LDXR/STXR 能在
             * 当前内存属性下成功。这里只有 CPU0 的 IRQ handler 写入。
             */
            let ticks = TIMER_TICKS.load(Ordering::Relaxed);
            TIMER_TICKS.store(ticks.wrapping_add(1), Ordering::Relaxed);
        }

        SPURIOUS_IRQ => return,

        _ => {
            // 目前只统计，暂时不要在 IRQ 内打印。
        }
    }

    gic.end_interrupt(acknowledge);
}

pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

/// 解除 PSTATE 中的 IRQ 屏蔽位。
pub fn enable_irq() {
    unsafe {
        /**
         * daifclr #2 中的立即数是四位掩码：
         * bit 3: D
         * bit 2: A
         * bit 1: I
         * bit 0: F
         */
        core::arch::asm!("msr daifclr, #2", "isb", options(nostack, preserves_flags),);
    }
}

/// 设置 PSTATE 中的 IRQ 屏蔽位。
pub fn disable_irq() {
    unsafe {
        core::arch::asm!("msr daifset, #2", "isb", options(nostack, preserves_flags),);
    }
}

/// Read the caller's original DAIF before diagnostic snapshots mask IRQ.
pub fn daif() -> u64 {
    let daif: u64;

    unsafe {
        core::arch::asm!(
            "mrs {daif}, daif",
            daif = out(reg) daif,
            options(nomem, nostack, preserves_flags),
        );
    }

    daif
}

pub fn irq_is_masked() -> bool {
    /*
     * DAIF.I 是 bit 7。
     */
    daif() & (1 << 7) != 0
}
