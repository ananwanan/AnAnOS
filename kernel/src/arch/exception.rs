use crate::arch::timer;
use crate::drivers::gic::{GENERIC_PHYSICAL_TIMER_IRQ, Gic, SPURIOUS_IRQ};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
pub struct ExceptionContext {
    pub registers: [u64; 31],
    pub elr_el1: u64,
    pub spsr_el1: u64,
    pub esr_el1: u64,
}

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

    /*
     * IAR 的低 10 位是中断 ID。
     * 其余位包含 CPU 来源等信息，EOIR 时要完整写回。
     */
    let acknowledge_value = gic.acknowledge();
    let interrupt_id = acknowledge_value & 0x3ff;

    match interrupt_id {
        GENERIC_PHYSICAL_TIMER_IRQ => {
            /*
             * 先安排下一次中断。
             * 写入新的 CVAL 后，当前定时器条件便被解除。
             */
            timer::schedule_next_interrupt();

            let tick = TIMER_TICKS.fetch_add(1, Ordering::Relaxed) + 1;

            /*
             * 当前测试阶段每秒仅产生一次 IRQ，且主循环不打印，
             * 所以暂时可以直接输出。
             *
             * 后面提高 tick 频率后，IRQ 中不要直接 println!。
             */
            crate::println!("[IRQ] GENERIC TIMER TICK: {}", tick,);
        }

        SPURIOUS_IRQ => {
            /*
             * 1023 表示没有可处理的中断。
             * 对 spurious interrupt 不写 EOIR。
             */
            return;
        }

        _ => {
            crate::println!("[IRQ] UNHANDLED INTERRUPT: {}", interrupt_id,);
        }
    }

    gic.end_interrupt(acknowledge_value);
}

pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

/// 解除 PSTATE 中的 IRQ 屏蔽位。
pub fn enable_irq() {
    unsafe {
        /**
         * daifclr #2 中的立即数是四位掩码：
         * bit 0: D
         * bit 1: A
         * bit 2: I
         * bit 3: F
         */
        core::arch::asm!(
            "msr daifclr, #2",
            "isb",
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// 设置 PSTATE 中的 IRQ 屏蔽位。
pub fn disable_irq() {
    unsafe {
        core::arch::asm!(
            "msr daifset, #2",
            "isb",
            options(nomem, nostack, preserves_flags),
        );
    }
}

pub fn irq_is_masked() -> bool {
    let daif: u64;

    unsafe {
        core::arch::asm!(
            "mrs {daif}, daif",
            daif = out(reg) daif,
            options(nomem, nostack, preserves_flags),
        );
    }

    /*
     * DAIF.I 是 bit 7。
     */
    daif & (1 << 7) != 0
}
