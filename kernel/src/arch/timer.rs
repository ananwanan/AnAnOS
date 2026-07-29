use core::arch::asm;

/// AArch64 Generic Timer。
/// 这里使用的是 CPU 自带的通用系统计数器，不依赖 BCM2711 的 MMIO 外设计时器，因此以后移植到其他 ARMv8-A 板子时也更方便
pub struct Timer;

impl Timer {
    pub const fn new() -> Self {
        Self
    }

    /// 获取系统计数器频率，单位 Hz。
    #[inline]
    pub fn frequency(&self) -> u64 {
        let frequency: u64;

        unsafe {
            asm!(
                "mrs {value}, cntfrq_el0",
                value = out(reg) frequency,
                options(nomem, nostack, preserves_flags),
            );
        }

        frequency
    }

    /// 读取当前物理计数器。
    #[inline]
    pub fn counter(&self) -> u64 {
        let counter: u64;

        unsafe {
            /*
             * ISB 确保前面的指令完成后再读取计数器，
             * 避免测量短代码时出现乱序影响。
             */
            asm!(
                "isb",
                "mrs {value}, cntpct_el0",
                value = out(reg) counter,
                options(nomem, nostack, preserves_flags),
            );
        }

        counter
    }

    /// 系统启动至今的微秒数。
    pub fn uptime_micros(&self) -> u64 {
        let frequency = self.frequency();

        if frequency == 0 {
            return 0;
        }

        let counter = self.counter();

        /*
         * 避免 counter * 1_000_000 发生 u64 溢出。
         */
        let seconds = counter / frequency;
        let remainder = counter % frequency;

        seconds
            .saturating_mul(1_000_000)
            .saturating_add(remainder.saturating_mul(1_000_000) / frequency)
    }

    /// 系统启动至今的毫秒数。
    pub fn uptime_millis(&self) -> u64 {
        self.uptime_micros() / 1_000
    }

    /// 系统启动至今的秒数。
    pub fn uptime_seconds(&self) -> u64 {
        let frequency = self.frequency();

        if frequency == 0 {
            return 0;
        }

        self.counter() / frequency
    }

    /// 忙等待指定微秒。
    pub fn delay_micros(&self, micros: u64) {
        if micros == 0 {
            return;
        }

        let frequency = self.frequency();

        if frequency == 0 {
            return;
        }

        /*
         * 向上取整，避免实际等待时间短于请求值。
         */
        let ticks =
            ((frequency as u128 * micros as u128).saturating_add(999_999) / 1_000_000) as u64;

        let start = self.counter();

        /*
         * wrapping_sub 可以正确处理 u64 计数器回绕。
         */
        while self.counter().wrapping_sub(start) < ticks {
            core::hint::spin_loop();
        }
    }

    /// 忙等待指定毫秒。
    pub fn delay_millis(&self, millis: u64) {
        /*
         * 分块等待，防止 millis * 1000 溢出。
         */
        let seconds = millis / 1_000;
        let remainder_millis = millis % 1_000;

        for _ in 0..seconds {
            self.delay_micros(1_000_000);
        }

        self.delay_micros(remainder_millis * 1_000);
    }
    
    pub fn ticks_to_micros(&self, ticks: u64) -> u64 {
        let frequency = self.frequency();

        if frequency == 0 {
            return 0;
        }

        let seconds = ticks / frequency;
        let remainder = ticks % frequency;

        seconds
            .saturating_mul(1_000_000)
            .saturating_add(remainder.saturating_mul(1_000_000) / frequency)
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}
