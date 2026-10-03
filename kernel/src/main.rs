#![allow(unused)]
#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod console;
mod drivers;
mod graphics;
mod memory;
mod test;

use arch::timer::Timer;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

use drivers::framebuffer::FrameBuffer;
use drivers::gic::Gic;
use drivers::mailbox::Mailbox;
use drivers::uart::MiniUart;

global_asm!(include_str!("../../boot/boot.S"));
global_asm!(include_str!("../../boot/vectors.S"));

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

    let framebuffer_region = init_framebuffer();

    // 初始化异常向量
    arch::exception::init();

    println!("DTB address : {dtb_address:#018x}");
    // Firmware/bootloader provides the readable DTB; memory init excludes all
    // firmware/kernel buffers before any page or heap allocation is possible.
    match unsafe { memory::init(dtb_address, framebuffer_region) } {
        Ok(stats) => {
            println!("[ OK ] PHYSICAL PAGE ALLOCATOR");
            println!("PAGES TOTAL   : {}", stats.total_pages);
            println!("PAGES RESERVED: {}", stats.reserved_pages);
            println!("PAGES USED    : {}", stats.allocated_pages);
            println!("PAGES FREE    : {}", stats.free_pages);
        }
        Err(error) => {
            println!("[FAIL] MEMORY INITIALIZATION: {error:?}");
            println!("[INFO] Continuing UART/IRQ diagnostics");
        }
    }

    {
        // 测试定时器
        // test::time_test();
        // 测试当前异常级别
        test::current_exception_level();
        // 测试异常向量
        // test::test_exception();
    }

    println!("DTB address : {dtb_address:#018x}");
    println!();

    println!();
    println!("INITIALIZING INTERRUPTS...");

    /*
     * 诊断期间始终屏蔽 CPU IRQ。
     */
    arch::exception::disable_irq();

    let gic = Gic::new();
    gic.init();

    println!("[ OK ] GIC INITIALIZED");

    arch::timer::schedule_next_interrupt();
    println!("[ OK ] TIMER ARMED");

    /*
     * 定时器设定为一秒，等待 1.5 秒让它确定到期。
     * IRQ 仍然屏蔽，所以不会进入 handler。
     */
    Timer::new().delay_millis(1_500);

    let enabled = gic.enabled_private_interrupts();
    let pending = gic.pending_private_interrupts();
    let groups = gic.private_interrupt_groups();

    println!();
    println!("TIMER CONTROL : {:#010x}", arch::timer::control());
    println!("TIMER PENDING : {}", arch::timer::interrupt_pending());

    println!("GICD_CTLR     : {:#010x}", gic.distributor_control());
    let distributor_type = gic.distributor_type();
    println!("GICD_TYPER    : {distributor_type:#010x}");
    println!("SECURITY EXT  : {}", distributor_type & (1 << 10) != 0,);
    println!("GICC_CTLR     : {:#010x}", gic.cpu_interface_control());
    println!("GICC_PMR      : {:#010x}", gic.priority_mask());
    println!("GICC_RPR      : {:#010x}", gic.running_priority());

    println!("ISENABLER0    : {enabled:#010x}");
    println!("ISPENDR0      : {pending:#010x}");
    println!("IGROUPR0 (NS) : {groups:#010x}");
    println!("HPPIR         : {}", gic.highest_pending_interrupt());
    println!("AHPPIR        : {}", gic.group1_highest_pending_interrupt());

    println!("IRQ30 ENABLED : {}", enabled & (1u32 << 30) != 0);
    println!("IRQ30 PENDING : {}", pending & (1u32 << 30) != 0);
    println!();
    println!("ENABLING CPU IRQ...");

    arch::exception::enable_irq();

    /*
     * 防止下一条 Rust 代码掩盖问题。
     */
    unsafe {
        core::arch::asm!(
            "nop",
            "nop",
            "nop",
            options(nomem, nostack, preserves_flags),
        );
    }

    println!("IRQ ENABLE RETURNED");
    println!("WAITING FOR TIMER INTERRUPT...");

    let mut previous_tick = 0;

    loop {
        let tick = arch::exception::timer_ticks();

        if tick != previous_tick {
            println!("TIMER TICK: {}", tick);
            previous_tick = tick;
        }

        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags),);
        }
    }

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
fn init_framebuffer() -> Option<memory::regions::Region> {
    println!();
    println!("Initializing framebuffer...");

    match FrameBuffer::new(1920, 1080) {
        Ok(framebuffer) => {
            // Returned physical address and allocation size are authoritative.
            let region = memory::regions::Region {
                start: framebuffer.address(),
                end: framebuffer.address() + framebuffer.size() as usize,
            };
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
            Some(region)
        }

        Err(error) => {
            println!("[FAIL] Framebuffer initialization");
            println!("Error: {error:?}");
            None
        }
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
