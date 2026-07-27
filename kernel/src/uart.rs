use core::fmt;
use core::ptr::{read_volatile, write_volatile};

const PERIPHERAL_BASE: usize = 0xFE00_0000;

const GPIO_BASE: usize = PERIPHERAL_BASE + 0x0020_0000;
const AUX_BASE: usize = PERIPHERAL_BASE + 0x0021_5000;

// GPIO 寄存器
const GPFSEL1: *mut u32 = (GPIO_BASE + 0x04) as *mut u32;
const GPPUPPDN0: *mut u32 = (GPIO_BASE + 0xE4) as *mut u32;

// Auxiliary 外设寄存器
const AUX_ENABLES: *mut u32 = (AUX_BASE + 0x04) as *mut u32;

// Mini UART 寄存器
const AUX_MU_IO_REG: *mut u32 = (AUX_BASE + 0x40) as *mut u32;
const AUX_MU_IER_REG: *mut u32 = (AUX_BASE + 0x44) as *mut u32;
const AUX_MU_IIR_REG: *mut u32 = (AUX_BASE + 0x48) as *mut u32;
const AUX_MU_LCR_REG: *mut u32 = (AUX_BASE + 0x4C) as *mut u32;
const AUX_MU_MCR_REG: *mut u32 = (AUX_BASE + 0x50) as *mut u32;
const AUX_MU_LSR_REG: *mut u32 = (AUX_BASE + 0x54) as *mut u32;
const AUX_MU_CNTL_REG: *mut u32 = (AUX_BASE + 0x60) as *mut u32;
const AUX_MU_BAUD_REG: *mut u32 = (AUX_BASE + 0x68) as *mut u32;

pub struct MiniUart;

impl MiniUart {
    pub const fn new() -> Self {
        Self
    }

    /// 初始化 Raspberry Pi 4 Mini UART。
    ///
    /// 配置：
    /// - GPIO14：TXD1，ALT5
    /// - GPIO15：RXD1，ALT5
    /// - 8 数据位
    /// - 无校验位
    /// - 1 停止位
    /// - 115200 baud
    pub fn init(&self) {
        unsafe {
            // AUX_ENABLES bit 0：启用 Mini UART。
            let mut enables = read_volatile(AUX_ENABLES);
            enables |= 1;
            write_volatile(AUX_ENABLES, enables);

            // 配置期间先关闭接收和发送。
            write_volatile(AUX_MU_CNTL_REG, 0);

            // 暂时禁用 Mini UART 中断。
            write_volatile(AUX_MU_IER_REG, 0);

            // 8 位数据模式。
            write_volatile(AUX_MU_LCR_REG, 3);

            // 不使用 RTS。
            write_volatile(AUX_MU_MCR_REG, 0);

            // 清空收发 FIFO。
            write_volatile(AUX_MU_IIR_REG, 0xC6);

            /*
             * Mini UART 波特率公式：
             *
             * baud_reg = core_clock / (8 × baud_rate) - 1
             *
             * core_clock = 250_000_000 Hz
             * baud_rate  = 115_200
             *
             * 结果约为 270。
             */
            write_volatile(AUX_MU_BAUD_REG, 270);

            /*
             * GPIO14、GPIO15 位于 GPFSEL1。
             *
             * GPIO14 对应 bit 12..14
             * GPIO15 对应 bit 15..17
             *
             * ALT5 的编码为 010。
             */
            let mut selector = read_volatile(GPFSEL1);

            selector &= !((0b111 << 12) | (0b111 << 15));
            selector |= (0b010 << 12) | (0b010 << 15);

            write_volatile(GPFSEL1, selector);

            /*
             * BCM2711 使用 GPPUPPDN 寄存器设置上下拉。
             * GPIO14、GPIO15 都设置为无上下拉：00。
             */
            let mut pulls = read_volatile(GPPUPPDN0);

            pulls &= !((0b11 << 28) | (0b11 << 30));

            write_volatile(GPPUPPDN0, pulls);

            // AUX_MU_CNTL：
            // bit 0 = RX enable
            // bit 1 = TX enable
            write_volatile(AUX_MU_CNTL_REG, 0b11);
        }
    }

    pub fn write_byte(&self, byte: u8) {
        unsafe {
            // LSR bit 5：发送寄存器为空，可以写入下一个字符。
            while read_volatile(AUX_MU_LSR_REG) & (1 << 5) == 0 {
                core::hint::spin_loop();
            }

            write_volatile(AUX_MU_IO_REG, byte as u32);
        }
    }

    #[allow(unused)]
    pub fn read_byte(&self) -> u8 {
        unsafe {
            // LSR bit 0：接收 FIFO 中存在数据。
            while read_volatile(AUX_MU_LSR_REG) & 1 == 0 {
                core::hint::spin_loop();
            }

            read_volatile(AUX_MU_IO_REG) as u8
        }
    }

    pub fn write_string(&self, text: &str) {
        for byte in text.bytes() {
            // 大多数串口终端换行需要 CRLF。
            if byte == b'\n' {
                self.write_byte(b'\r');
            }

            self.write_byte(byte);
        }
    }
}

impl fmt::Write for MiniUart {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_string(text);
        Ok(())
    }
}