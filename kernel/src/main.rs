#![no_std]
#![no_main]

mod console;
mod drivers;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

use drivers::uart::MiniUart;

global_asm!(include_str!("../../boot/boot.S"));

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::console::_print(core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };

    ($($arg:tt)*) => {
        $crate::print!("{}\n", core::format_args!($($arg)*))
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb_address: usize) -> ! {
    let uart = MiniUart::new();
    uart.init();

    println!();
    println!("================================");
    println!("            AnanOS");
    println!("================================");

    println!("[ OK ] Boot assembly");
    println!("[ OK ] Kernel stack");
    println!("[ OK ] BSS initialization");
    println!("[ OK ] Mini UART");
    println!();

    println!("Board       : Raspberry Pi 4B");
    println!("Architecture: AArch64");
    println!("DTB address : {dtb_address:#018x}");
    println!();
    println!("Welcome to AnanOS!");

    loop {
        unsafe {
            asm!("wfe");
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!();
    println!("================================");
    println!("         KERNEL PANIC");
    println!("================================");
    println!("{info}");

    loop {
        unsafe {
            asm!("wfe");
        }
    }
}