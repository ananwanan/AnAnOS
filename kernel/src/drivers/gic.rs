use core::ptr::{read_volatile, write_volatile};

/*
 * Raspberry Pi 4 / BCM2711 的 GIC-400 地址。
 *
 * Distributor : 0xFF84_1000
 * CPU Interface: 0xFF84_2000
 *
 * 当前 MMU 尚未开启，直接使用物理地址。
 * 
 * BCM2711 的 GIC Distributor 和 CPU Interface 窗口分别位于 0xFF841000 和 0xFF842000。
 * Pi 4 上的 GIC 是 GICv2，而不是 GICv3。
 */
const GICD_BASE: usize = 0xFF84_1000;
const GICC_BASE: usize = 0xFF84_2000;

// Distributor 寄存器
const GICD_CTLR: *mut u32 = (GICD_BASE + 0x000) as *mut u32;
const GICD_IGROUPR0: *mut u32 = (GICD_BASE + 0x080) as *mut u32;
const GICD_ISENABLER0: *mut u32 = (GICD_BASE + 0x100) as *mut u32;
const GICD_ICENABLER0: *mut u32 = (GICD_BASE + 0x180) as *mut u32;
const GICD_ICPENDR0: *mut u32 = (GICD_BASE + 0x280) as *mut u32;
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400;

// CPU Interface 寄存器
const GICC_CTLR: *mut u32 = (GICC_BASE + 0x000) as *mut u32;
const GICC_PMR: *mut u32 = (GICC_BASE + 0x004) as *mut u32;
const GICC_BPR: *mut u32 = (GICC_BASE + 0x008) as *mut u32;
const GICC_IAR: *const u32 = (GICC_BASE + 0x00C) as *const u32;
const GICC_EOIR: *mut u32 = (GICC_BASE + 0x010) as *mut u32;

pub const GENERIC_PHYSICAL_TIMER_IRQ: u32 = 30;
pub const SPURIOUS_IRQ: u32 = 1023;

pub struct Gic;

impl Gic {
    pub const fn new() -> Self {
        Self
    }

    pub fn init(&self) {
        unsafe {
            /*
             * 先关闭 CPU Interface 和 Distributor。
             */
            write_volatile(GICC_CTLR, 0);
            write_volatile(GICD_CTLR, 0);

            /*
             * 先禁用并清除 SGI/PPI。
             *
             * GICD_ISENABLER0 等寄存器对于 PPI 是每 CPU banked，
             * 因此这里配置的是当前 CPU0。
             */
            write_volatile(GICD_ICENABLER0, 0xFFFF_FFFF);
            write_volatile(GICD_ICPENDR0, 0xFFFF_FFFF);

            /*
             * 将物理定时器 PPI 30 配置为 Group 1，
             * 供当前 Non-secure EL1 内核处理。
             */
            let mut group = read_volatile(GICD_IGROUPR0);
            group |= 1 << GENERIC_PHYSICAL_TIMER_IRQ;
            write_volatile(GICD_IGROUPR0, group);

            /*
             * 设置 IRQ 30 的优先级。
             *
             * 每个中断占一个字节。
             * 数字越小，优先级越高。
             */
            write_volatile(
                (GICD_IPRIORITYR + GENERIC_PHYSICAL_TIMER_IRQ as usize) as *mut u8,
                0x80,
            );

            /*
             * CPU 接受所有优先级不低于 0xFF 的中断。
             */
            write_volatile(GICC_PMR, 0xFF);

            /*
             * 暂时不使用复杂的优先级分组。
             */
            write_volatile(GICC_BPR, 0);

            /*
             * 启用物理定时器 PPI。
             */
            write_volatile(GICD_ISENABLER0, 1 << GENERIC_PHYSICAL_TIMER_IRQ);

            /*
             * 启用 Distributor 和 CPU Interface。
             *
             * 对 Non-secure GICv2 访问而言，bit 0 控制 Group 1。
             */
            write_volatile(GICD_CTLR, 1);
            write_volatile(GICC_CTLR, 1);

            core::arch::asm!("dsb sy", "isb");
        }
    }

    /// 确认并取得当前最高优先级中断。
    pub fn acknowledge(&self) -> u32 {
        unsafe { read_volatile(GICC_IAR) }
    }

    /// 通知 GIC 当前中断已经处理完成。
    ///
    /// 必须写回从 IAR 读取的完整值，而不仅是中断号。
    pub fn end_interrupt(&self, acknowledge_value: u32) {
        unsafe {
            write_volatile(GICC_EOIR, acknowledge_value);
            core::arch::asm!("dsb sy", "isb");
        }
    }
}

impl Default for Gic {
    fn default() -> Self {
        Self::new()
    }
}
