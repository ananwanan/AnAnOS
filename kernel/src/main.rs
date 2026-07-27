#![no_std]
#![no_main]

mod uart;

use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::panic::PanicInfo;

use uart::MiniUart;

global_asm!(include_str!("../../boot/boot.S"));

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb_address: usize) -> ! {
    let mut uart = MiniUart::new();
    uart.init();

    uart.write_string("\n");
    uart.write_string("================================\n");
    uart.write_string("           AnanOS\n");
    uart.write_string("================================\n");
    uart.write_string("Boot successful!\n");
    uart.write_string("CPU: Raspberry Pi 4B / BCM2711\n");

    writeln!(uart, "Device Tree: {dtb_address:#018x}").ok();

    uart.write_string("\nHello from Rust kernel!\n");

    loop {
        unsafe {
            asm!("wfe");
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut uart = MiniUart::new();

    // 如果 panic 出现在 UART 初始化之后，可以打印错误。
    writeln!(uart, "\n[KERNEL PANIC]").ok();
    writeln!(uart, "{info}").ok();

    loop {
        unsafe {
            asm!("wfe");
        }
    }
}