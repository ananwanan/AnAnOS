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
pub extern "C" fn rust_sync_exception(context: &mut ExceptionContext) -> ! {
    let esr = context.esr_el1;
    let exception_class = (esr >> 26) & 0x3f;
    let iss = esr & 0x01ff_ffff;

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
    crate::println!("FAR_EL1   : {far:#018x}");

    loop {
        unsafe {
            asm!("wfe");
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_irq_exception(_context: &mut ExceptionContext) {
    crate::println!("[IRQ] UNHANDLED");
}
