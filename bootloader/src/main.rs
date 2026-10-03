#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

global_asm!(include_str!("../boot.S"));

const KERNEL_LOAD_ADDRESS: usize = 0x200000;
const MAX_KERNEL_SIZE: u32 = 64 * 1024 * 1024;
const MAGIC: [u8; 4] = *b"ANAN";

const PERIPHERAL_BASE: usize = 0xFE00_0000;
const GPIO_BASE: usize = PERIPHERAL_BASE + 0x0020_0000;
const AUX_BASE: usize = PERIPHERAL_BASE + 0x0021_5000;

const GPFSEL1: *mut u32 = (GPIO_BASE + 0x04) as *mut u32;
const GPPUPPDN0: *mut u32 = (GPIO_BASE + 0xE4) as *mut u32;
const AUX_ENABLES: *mut u32 = (AUX_BASE + 0x04) as *mut u32;
const AUX_MU_IO_REG: *mut u32 = (AUX_BASE + 0x40) as *mut u32;
const AUX_MU_IER_REG: *mut u32 = (AUX_BASE + 0x44) as *mut u32;
const AUX_MU_IIR_REG: *mut u32 = (AUX_BASE + 0x48) as *mut u32;
const AUX_MU_LCR_REG: *mut u32 = (AUX_BASE + 0x4C) as *mut u32;
const AUX_MU_MCR_REG: *mut u32 = (AUX_BASE + 0x50) as *mut u32;
const AUX_MU_LSR_REG: *mut u32 = (AUX_BASE + 0x54) as *mut u32;
const AUX_MU_CNTL_REG: *mut u32 = (AUX_BASE + 0x60) as *mut u32;
const AUX_MU_BAUD_REG: *mut u32 = (AUX_BASE + 0x68) as *mut u32;

struct MiniUart;

impl MiniUart {
    fn init(&self) {
        unsafe {
            let enables = read_volatile(AUX_ENABLES);
            write_volatile(AUX_ENABLES, enables | 1);
            write_volatile(AUX_MU_CNTL_REG, 0);
            write_volatile(AUX_MU_IER_REG, 0);
            write_volatile(AUX_MU_LCR_REG, 3);
            write_volatile(AUX_MU_MCR_REG, 0);
            write_volatile(AUX_MU_IIR_REG, 0xC6);
            write_volatile(AUX_MU_BAUD_REG, 270);

            let mut selector = read_volatile(GPFSEL1);
            selector &= !((0b111 << 12) | (0b111 << 15));
            selector |= (0b010 << 12) | (0b010 << 15);
            write_volatile(GPFSEL1, selector);

            let mut pulls = read_volatile(GPPUPPDN0);
            pulls &= !((0b11 << 28) | (0b11 << 30));
            write_volatile(GPPUPPDN0, pulls);

            write_volatile(AUX_MU_CNTL_REG, 0b11);
        }
    }

    fn write_byte(&self, byte: u8) {
        unsafe {
            while read_volatile(AUX_MU_LSR_REG) & (1 << 5) == 0 {
                core::hint::spin_loop();
            }
            write_volatile(AUX_MU_IO_REG, byte as u32);
        }
    }

    fn read_byte(&self) -> u8 {
        unsafe {
            while read_volatile(AUX_MU_LSR_REG) & 1 == 0 {
                core::hint::spin_loop();
            }
            read_volatile(AUX_MU_IO_REG) as u8
        }
    }

    fn write_string(&self, text: &str) {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn bootloader_main(dtb_address: usize) -> ! {
    let uart = MiniUart;
    uart.init();
    uart.write_string("\nANANOS UART BOOTLOADER READY\n");
    if dtb_address == 0 {
        uart.write_string("WARN DTB MISSING_OR_INVALID\n");
    }

    loop {
        match receive_kernel(&uart) {
            Ok(size) => {
                uart.write_string("OK ");
                write_hex(&uart, size);
                uart.write_string("\n");
                jump_to_kernel(dtb_address);
            }
            Err(message) => {
                uart.write_string("ERR ");
                uart.write_string(message);
                uart.write_string("\nREADY\n");
            }
        }
    }
}

fn receive_kernel(uart: &MiniUart) -> Result<u32, &'static str> {
    for expected in MAGIC {
        if uart.read_byte() != expected {
            return Err("BAD_MAGIC");
        }
    }

    let size = read_u32_le(uart);
    if size == 0 || size > MAX_KERNEL_SIZE {
        return Err("BAD_SIZE");
    }

    let kernel = KERNEL_LOAD_ADDRESS as *mut u8;
    let mut checksum = 0u32;

    for offset in 0..size {
        let byte = uart.read_byte();
        checksum = checksum.wrapping_add(byte as u32);
        unsafe {
            write_volatile(kernel.add(offset as usize), byte);
        }
    }

    let expected_checksum = read_u32_le(uart);
    if checksum != expected_checksum {
        return Err("BAD_CHECKSUM");
    }

    Ok(size)
}

fn read_u32_le(uart: &MiniUart) -> u32 {
    let b0 = uart.read_byte() as u32;
    let b1 = uart.read_byte() as u32;
    let b2 = uart.read_byte() as u32;
    let b3 = uart.read_byte() as u32;
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
}

fn write_hex(uart: &MiniUart, value: u32) {
    uart.write_string("0x");
    for shift in (0..8).rev() {
        let digit = ((value >> (shift * 4)) & 0xf) as u8;
        let byte = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        };
        uart.write_byte(byte);
    }
}

fn jump_to_kernel(dtb_address: usize) -> ! {
    unsafe {
        asm!(
            "dsb sy",
            "ic iallu",
            "dsb sy",
            "isb",
            "mov x0, {dtb}",
            "br {entry}",
            dtb = in(reg) dtb_address,
            entry = in(reg) KERNEL_LOAD_ADDRESS,
            options(noreturn)
        );
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
