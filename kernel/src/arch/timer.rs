use core::arch::asm;

/// AArch64 Generic Timer。
/// 这里使用的是 CPU 自带的通用系统计数器，不依赖 BCM2711 的 MMIO 外设计时器，因此以后移植到其他 ARMv8-A 板子时也更方便
pub struct Timer;

pub const TIMER_INTERVAL_SECONDS: u64 = 1;

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

/// 安排下一次物理定时器中断。
pub fn schedule_next_interrupt() {
    let frequency: u64;
    let counter: u64;

    unsafe {
        asm!(
            "mrs {frequency}, cntfrq_el0",
            "mrs {counter}, cntpct_el0",
            frequency = out(reg) frequency,
            counter = out(reg) counter,
            options(nomem, nostack, preserves_flags),
        );
    }

    let deadline = counter.wrapping_add(frequency.saturating_mul(TIMER_INTERVAL_SECONDS));

    unsafe {
        /**
         * 使用 CNTP_CVAL_EL0 设置绝对截止时间。
         * 定时器到期后不会自动安排下一次，所以 IRQ handler 必须重新设置下一次截止时间。
         */
        asm!(
            "msr cntp_cval_el0, {deadline}",
            "mov {control}, #1",
            "msr cntp_ctl_el0, {control}",
            "isb",
            deadline = in(reg) deadline,
            control = out(reg) _,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// 禁用物理定时器。
pub fn disable_interrupt() {
    unsafe {
        asm!(
            "msr cntp_ctl_el0, xzr",
            "isb",
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// 当前物理定时器是否产生了中断条件。
pub fn interrupt_pending() -> bool {
    let control: u64;

    unsafe {
        asm!(
            "mrs {control}, cntp_ctl_el0",
            control = out(reg) control,
            options(nomem, nostack, preserves_flags),
        );
    }

    /*
     * CNTP_CTL_EL0:
     * bit 0 ENABLE
     * bit 1 IMASK
     * bit 2 ISTATUS
     */
    control & (1 << 2) != 0
}

pub fn control() -> u64 {
    let value: u64;

    unsafe {
        asm!(
            "mrs {value}, cntp_ctl_el0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

pub fn compare_value() -> u64 {
    let value: u64;

    unsafe {
        asm!(
            "mrs {value}, cntp_cval_el0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

pub fn current_counter() -> u64 {
    let value: u64;

    unsafe {
        asm!(
            "isb",
            "mrs {value}, cntpct_el0",
            value = out(reg) value,
            // No nomem: preflight samples IRQ memory state before this timestamp.
            // The compiler barrier and ISB preserve that measurement order.
            options(nostack, preserves_flags),
        );
    }

    value
}
