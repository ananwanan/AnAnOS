use core::arch::asm;

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
    crate::println!("[IRQ] UNHANDLED");
}
