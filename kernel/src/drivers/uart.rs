use core::fmt;
use core::ptr::{read_volatile, write_volatile};
/// 周围设备基址
const PERIPHERAL_BASE: usize = 0xFE00_0000;

/// GPIO 基址
const GPIO_BASE: usize = PERIPHERAL_BASE + 0x0020_0000;
/// AUX 基址
const AUX_BASE: usize = PERIPHERAL_BASE + 0x0021_5000;

/// GPIO 选择器 1
const GPFSEL1: *mut u32 = (GPIO_BASE + 0x04) as *mut u32;
/// GPIO 上下拉选择器 0
const GPPUPPDN0: *mut u32 = (GPIO_BASE + 0xE4) as *mut u32;

/// AUX 使能寄存器
const AUX_ENABLES: *mut u32 = (AUX_BASE + 0x04) as *mut u32;

/// AUX IO 寄存器
const AUX_MU_IO_REG: *mut u32 = (AUX_BASE + 0x40) as *mut u32;
/// AUX 中断使能寄存器
const AUX_MU_IER_REG: *mut u32 = (AUX_BASE + 0x44) as *mut u32;
/// AUX 中断标识寄存器
const AUX_MU_IIR_REG: *mut u32 = (AUX_BASE + 0x48) as *mut u32;
/// AUX 级联中断标识寄存器
const AUX_MU_LCR_REG: *mut u32 = (AUX_BASE + 0x4C) as *mut u32;
/// AUX 自动流控寄存器
const AUX_MU_MCR_REG: *mut u32 = (AUX_BASE + 0x50) as *mut u32;
/// AUX 状态寄存器
const AUX_MU_LSR_REG: *mut u32 = (AUX_BASE + 0x54) as *mut u32;
/// AUX 控制寄存器
const AUX_MU_CNTL_REG: *mut u32 = (AUX_BASE + 0x60) as *mut u32;
/// AUX 波特率寄存器
const AUX_MU_BAUD_REG: *mut u32 = (AUX_BASE + 0x68) as *mut u32;

/// AUX UART 驱动
/// 该驱动仅支持 8 位数据模式，不支持奇偶校验位，该驱动不支持中断。
pub struct MiniUart;

impl MiniUart {
    pub const fn new() -> Self {
        Self
    }

    pub fn init(&self) {
        unsafe {
            let enables = read_volatile(AUX_ENABLES);
            write_volatile(AUX_ENABLES, enables | 1);

            // 初始化期间关闭收发。
            write_volatile(AUX_MU_CNTL_REG, 0);

            // 暂时关闭中断。
            write_volatile(AUX_MU_IER_REG, 0);

            // 8 位数据模式。
            write_volatile(AUX_MU_LCR_REG, 3);

            // 禁用自动流控。
            write_volatile(AUX_MU_MCR_REG, 0);

            // 清空 FIFO。
            write_volatile(AUX_MU_IIR_REG, 0xC6);

            // core_freq = 250 MHz 时对应 115200 baud。
            write_volatile(AUX_MU_BAUD_REG, 270);

            // GPIO14、GPIO15 配置为 ALT5。
            let mut selector = read_volatile(GPFSEL1);
            selector &= !((0b111 << 12) | (0b111 << 15));
            selector |= (0b010 << 12) | (0b010 << 15);
            write_volatile(GPFSEL1, selector);

            // GPIO14、GPIO15 无上下拉。
            let mut pulls = read_volatile(GPPUPPDN0);
            pulls &= !((0b11 << 28) | (0b11 << 30));
            write_volatile(GPPUPPDN0, pulls);

            // 启用 RX 和 TX。
            write_volatile(AUX_MU_CNTL_REG, 0b11);
        }
    }

    /// 写入字节
    /// 等待发送缓冲区为空，将字节写入发送缓冲区。
    pub fn write_byte(&self, byte: u8) {
        unsafe {
            while read_volatile(AUX_MU_LSR_REG) & (1 << 5) == 0 {
                core::hint::spin_loop();
            }

            write_volatile(AUX_MU_IO_REG, byte as u32);
        }
    }

    /// 读取字节
    /// 等待接收缓冲区非空，将字节从接收缓冲区读取。
    /// 返回读取的字节。
    #[allow(unused)]
    pub fn read_byte(&self) -> u8 {
        unsafe {
            while read_volatile(AUX_MU_LSR_REG) & 1 == 0 {
                core::hint::spin_loop();
            }

            read_volatile(AUX_MU_IO_REG) as u8
        }
    }

    /// 写入字符串
    /// 等待发送缓冲区为空，将字符串写入发送缓冲区。
    /// 如果字符串包含换行符，会自动添加回车符。
    pub fn write_string(&self, text: &str) {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }

            self.write_byte(byte);
        }
    }
}

/// 默认实现
impl Default for MiniUart {
    fn default() -> Self {
        Self::new()
    }
}

/// 实现 fmt::Write
impl fmt::Write for MiniUart {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_string(text);
        Ok(())
    }
}
