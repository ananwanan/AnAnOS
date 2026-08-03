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
const GICD_TYPER: *const u32 = (GICD_BASE + 0x004) as *const u32;
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

const GICD_ISPENDR0: *const u32 = (GICD_BASE + 0x200) as *const u32;
const GICD_ISENABLER0_READ: *const u32 = (GICD_BASE + 0x100) as *const u32;
const GICD_IGROUPR0_READ: *const u32 = (GICD_BASE + 0x080) as *const u32;
const GICC_HPPIR: *const u32 = (GICC_BASE + 0x018) as *const u32;
const GICC_RPR: *const u32 = (GICC_BASE + 0x014) as *const u32;

pub const GENERIC_PHYSICAL_TIMER_IRQ: u32 = 30;
pub const SPURIOUS_IRQ: u32 = 1023;

const GICC_AHPPIR: *const u32 = (GICC_BASE + 0x028) as *const u32;

pub struct Gic;

impl Gic {
    pub const fn new() -> Self {
        Self
    }

    pub fn init(&self) {
        unsafe {
            // 当前 CPU Interface 先关闭。
            write_volatile(GICC_CTLR, 0);

            /*
             * 不建议在这里只为初始化一个 PPI 就关闭整个 Distributor。
             * Distributor 是整个 GIC 共享的，而 PPI 配置是当前 CPU 私有的。
             *
             * 树莓派固件可能已经对 Distributor 做过基础设置。
             */

            // 禁用当前 CPU 的 PPI 30。
            write_volatile(GICD_ICENABLER0, 1u32 << GENERIC_PHYSICAL_TIMER_IRQ);

            // 清除可能遗留的 pending 状态。
            write_volatile(GICD_ICPENDR0, 1u32 << GENERIC_PHYSICAL_TIMER_IRQ);

            /*
             * IRQ 30 的优先级字节。
             */
            write_volatile(
                (GICD_IPRIORITYR + GENERIC_PHYSICAL_TIMER_IRQ as usize) as *mut u8,
                0x80,
            );

            write_volatile(GICC_PMR, 0xff);
            write_volatile(GICC_BPR, 0);

            // 启用当前 CPU 的 PPI 30。
            write_volatile(GICD_ISENABLER0, 1u32 << GENERIC_PHYSICAL_TIMER_IRQ);

            /*
             * 确保 Distributor 已启用。
             * 保留已有位，不要直接覆盖成 1。
             */
            let distributor_control = read_volatile(GICD_CTLR);
            write_volatile(GICD_CTLR, distributor_control | 1);

            /*
             * 启用当前 CPU Interface。
             */
            write_volatile(GICC_CTLR, 1);

            core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags),);
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
            core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags),);
        }
    }
    pub fn distributor_control(&self) -> u32 {
        unsafe { read_volatile(GICD_CTLR) }
    }

    pub fn distributor_type(&self) -> u32 {
        unsafe { read_volatile(GICD_TYPER) }
    }

    pub fn cpu_interface_control(&self) -> u32 {
        unsafe { read_volatile(GICC_CTLR) }
    }

    pub fn priority_mask(&self) -> u32 {
        unsafe { read_volatile(GICC_PMR) }
    }

    pub fn enabled_private_interrupts(&self) -> u32 {
        unsafe { read_volatile(GICD_ISENABLER0_READ) }
    }

    pub fn pending_private_interrupts(&self) -> u32 {
        unsafe { read_volatile(GICD_ISPENDR0) }
    }

    pub fn private_interrupt_groups(&self) -> u32 {
        unsafe { read_volatile(GICD_IGROUPR0_READ) }
    }

    pub fn highest_pending_interrupt(&self) -> u32 {
        unsafe { read_volatile(GICC_HPPIR) & 0x3ff }
    }

    pub fn running_priority(&self) -> u32 {
        unsafe { read_volatile(GICC_RPR) }
    }

    pub fn group1_highest_pending_interrupt(&self) -> u32 {
        unsafe { read_volatile(GICC_AHPPIR) & 0x3ff }
    }
}

impl Default for Gic {
    fn default() -> Self {
        Self::new()
    }
}
