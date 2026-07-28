#![allow(unused)]
#![no_std]
#![no_main]

mod console;
mod drivers;
mod graphics;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

use drivers::framebuffer::FrameBuffer;
use drivers::mailbox::Mailbox;
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

    init_mailbox();

    init_framebuffer();

    screen_test();

    println!("DTB address : {dtb_address:#018x}");
    println!();
    println!("Welcome to AnanOS!");

    loop {
        unsafe {
            asm!("wfe");
        }
    }
}

/// 初始化 Mailbox 并获取固件修订号。
fn init_mailbox() {
    let mailbox = Mailbox::new();
    match mailbox.firmware_revision() {
        Ok(revision) => {
            println!("[ OK ] Property Mailbox");
            println!("Firmware revision: {revision:#010x}");
        }
        Err(error) => {
            println!("[FAIL] Property Mailbox");
            println!("Mailbox error: {error:?}");
        }
    }
}

/// 初始化 Framebuffer 并绘制测试图案。
fn init_framebuffer() {
    println!();
    println!("Initializing framebuffer...");

    match FrameBuffer::new(1920, 1080) {
        Ok(framebuffer) => {
            // 安装前，这些日志只输出到 UART。
            println!("[ OK ] Framebuffer allocated");
            println!("Address    : {:#018x}", framebuffer.address());
            println!(
                "Resolution : {}x{}",
                framebuffer.width(),
                framebuffer.height()
            );
            println!("Pitch      : {} bytes", framebuffer.pitch());
            println!("Depth      : {} bits", framebuffer.depth());
            println!("Size       : {} bytes", framebuffer.size());
            println!("Pixel order: {:?}", framebuffer.pixel_order());

            /*
             * 安装时会清空屏幕。
             * 从这一行之后，println! 同时输出到 UART 和 HDMI。
             */
            console::install_framebuffer(framebuffer);

            println!("========================================");
            println!("               ANANOS");
            println!("========================================");
            println!();
            println!("[ OK ] BOOT ASSEMBLY");
            println!("[ OK ] KERNEL STACK");
            println!("[ OK ] BSS INITIALIZATION");
            println!("[ OK ] MINI UART");
            println!("[ OK ] PROPERTY MAILBOX");
            println!("[ OK ] FRAMEBUFFER");
            println!("[ OK ] SCREEN CONSOLE");
            println!();
            println!("BOARD       : RASPBERRY PI 4B");
            println!("ARCHITECTURE: AARCH64");
            println!("RESOLUTION  : 1920X1080");
            println!("DEPTH       : 32 BIT");
            println!("PITCH       : 7680 BYTES");
            println!();
            println!("WELCOME TO ANANOS!");
        }

        Err(error) => {
            println!("[FAIL] Framebuffer initialization");
            println!("Error: {error:?}");
        }
    }
}

fn screen_test() {
    const WHITE: u32 = 0x00F0_F4F8;
    const GREEN: u32 = 0x0050_FA7B;
    const BLUE: u32 = 0x0060_A5FA;
    const YELLOW: u32 = 0x00FF_D866;

    console::set_screen_foreground(BLUE);
    println!("ANANOS KERNEL");

    console::set_screen_foreground(GREEN);
    println!("[ OK ] FRAMEBUFFER");
    println!("[ OK ] SCREEN CONSOLE");

    console::set_screen_foreground(YELLOW);
    println!("[INFO] RUNNING ON CPU0");

    console::set_screen_foreground(WHITE);
    println!("WELCOME TO ANANOS!");

    for index in 0..60 {
        println!("LOG LINE {:02}: SCREEN SCROLL TEST", index);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!();
    println!("==============================");
    println!("        KERNEL PANIC");
    println!("==============================");

    if let Some(location) = info.location() {
        println!("File  : {}", location.file());
        println!("Line  : {}", location.line());
        println!("Column: {}", location.column());
    }

    println!("Message: {}", info.message());

    loop {
        unsafe {
            core::arch::asm!("wfe");
        }
    }
}
