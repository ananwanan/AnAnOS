use crate::arch::timer::Timer;

use crate::println;

/// 测试定时器
pub fn time_test() {
    let timer = Timer::new();

    println!();
    println!("[INFO] GENERIC TIMER");
    println!("FREQUENCY : {} HZ", timer.frequency());
    println!("COUNTER   : {}", timer.counter());
    println!("UPTIME    : {} MS", timer.uptime_millis());

    println!();
    println!("DELAY TEST START");

    for second in 1..=3 {
        let start = timer.counter();

        timer.delay_millis(1_000);

        let end = timer.counter();
        let elapsed = timer.ticks_to_micros(end.wrapping_sub(start));

        println!(
            "{} SECOND, DELAY = {} US, UPTIME = {} MS",
            second,
            elapsed,
            timer.uptime_millis(),
        );
    }

    println!("DELAY TEST FINISHED");
}

/// 测试当前异常级别
pub fn current_exception_level() {
    let current_el: u64;

    unsafe {
        core::arch::asm!(
            "mrs {value}, CurrentEL",
            value = out(reg) current_el,
            options(nomem, nostack, preserves_flags),
        );
    }

    /// 树莓派固件可能让内核从 EL2 启动，而操作系统内核通常希望运行在 EL1。EL0 一般留给未来的用户程序，EL2 则主要用于虚拟化管理。
    /// 如果是 EL2，我们下一步先在 boot.S 中安全下降到 EL1；
    /// 如果已经是 EL1，就直接写 vectors.S 和 VBAR_EL1。
    let level: u64 = current_el >> 2;

    /// 我的树莓派固件把内核放在 EL2 启动。
    /// 因此要在 boot.S 中切换到 EL1h，以后内核、异常向量、页表和驱动都统一运行在 EL1；EL2 留给未来的虚拟机管理器。
    /// Arm 的异常级模型中，操作系统通常运行在 EL1，EL2主要用于虚拟化。
    println!("Current exception level: {}", level);
}

/// 测试异常向量，触发一个异常，检查是否能正确处理。
/// # 注意
/// 这个测试需要在 EL1 下运行，否则会触发 EL0 异常。    
pub fn test_exception() {
    println!("BEFORE BRK");

    unsafe {
        core::arch::asm!("brk #0x1234");
    }

    println!("AFTER BRK");
    println!("EXCEPTION RETURN SUCCESSFUL");
}
