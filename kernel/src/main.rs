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
        Ok(mut framebuffer) => {
            println!("[ OK ] Framebuffer");
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

            const BACKGROUND: u32 = 0x0018_2430;
            const TITLE: u32 = 0x0060_A5FA;
            const TEXT: u32 = 0x00F0_F4F8;
            const SUCCESS: u32 = 0x0050_FA7B;
            const PANEL: u32 = 0x0022_3040;

            framebuffer.clear(BACKGROUND);

            framebuffer.draw_rect(60, 60, framebuffer.width().saturating_sub(120), 720, PANEL);

            framebuffer.draw_string_scaled(110, 100, "ANANOS", TITLE, PANEL, 5);

            framebuffer.draw_string_scaled(
                110,
                180,
                "RUST BARE METAL OPERATING SYSTEM",
                TEXT,
                PANEL,
                2,
            );

            framebuffer.draw_string_scaled(
                110,
                270,
                "[OK] BOOT ASSEMBLY\n\
             [OK] KERNEL STACK\n\
             [OK] MINI UART\n\
             [OK] PROPERTY MAILBOX\n\
             [OK] FRAMEBUFFER",
                SUCCESS,
                PANEL,
                3,
            );

            framebuffer.draw_string_scaled(
                110,
                570,
                "RASPBERRY PI 4B\n\
             RESOLUTION: 1920X1080\n\
             DEPTH: 32 BIT\n\
             PITCH: 7680 BYTES",
                TEXT,
                PANEL,
                2,
            );

            println!("[ OK ] Text rendered");
        }

        Err(error) => {
            println!("[FAIL] Framebuffer initialization");
            println!("Error: {error:?}");
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
