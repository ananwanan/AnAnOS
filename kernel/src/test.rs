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

/// Opt-in hardware diagnostic after the EL1h vector table is installed.
///
/// Samples both 64-bit halves of q0/q1/q31 and FP control/status across the
/// normal BRK handler. The original FPCR/FPSR are restored before Rust resumes.
/// This is a runtime sample; the assembly layout/disassembly checks cover the
/// complete register bank. It does not verify asynchronous IRQ delivery.
pub fn test_exception_simd_context() -> bool {
    println!("BEFORE SIMD/FP CONTEXT BRK");
    let failures = crate::arch::interrupt::with_irq_masked(|| {
        let failures: u64;
        unsafe {
            core::arch::asm!(
                "mrs {old_fpcr}, fpcr",
                "mrs {old_fpsr}, fpsr",
                "movi v0.16b, #0x11",
                "movi v1.16b, #0x22",
                "movi v31.16b, #0xaa",
                // FZ and cumulative saturation are implemented on Cortex-A72;
                // integer-only diagnostics do not depend on FP rounding modes.
                "mov {scratch}, #(1 << 24)",
                "msr fpcr, {scratch}",
                "mov {scratch}, #(1 << 27)",
                "msr fpsr, {scratch}",
                "brk #0x5349",
                "mov {failures}, xzr",
                "umov {actual}, v0.d[0]",
                "eor {actual}, {actual}, {expected0}",
                "umov {scratch}, v0.d[1]",
                "eor {scratch}, {scratch}, {expected0}",
                "orr {actual}, {actual}, {scratch}",
                "cmp {actual}, #0",
                "cset {actual}, ne",
                "orr {failures}, {failures}, {actual}",
                "umov {actual}, v1.d[0]",
                "eor {actual}, {actual}, {expected1}",
                "umov {scratch}, v1.d[1]",
                "eor {scratch}, {scratch}, {expected1}",
                "orr {actual}, {actual}, {scratch}",
                "cmp {actual}, #0",
                "cset {actual}, ne",
                "orr {failures}, {failures}, {actual}, lsl #1",
                "umov {actual}, v31.d[0]",
                "eor {actual}, {actual}, {expected31}",
                "umov {scratch}, v31.d[1]",
                "eor {scratch}, {scratch}, {expected31}",
                "orr {actual}, {actual}, {scratch}",
                "cmp {actual}, #0",
                "cset {actual}, ne",
                "orr {failures}, {failures}, {actual}, lsl #2",
                "mrs {actual}, fpcr",
                "mov {scratch}, #(1 << 24)",
                "cmp {actual}, {scratch}",
                "cset {actual}, ne",
                "orr {failures}, {failures}, {actual}, lsl #3",
                "mrs {actual}, fpsr",
                "mov {scratch}, #(1 << 27)",
                "cmp {actual}, {scratch}",
                "cset {actual}, ne",
                "orr {failures}, {failures}, {actual}, lsl #4",
                "msr fpcr, {old_fpcr}",
                "msr fpsr, {old_fpsr}",
                old_fpcr = out(reg) _,
                old_fpsr = out(reg) _,
                actual = out(reg) _,
                scratch = out(reg) _,
                failures = out(reg) failures,
                expected0 = in(reg) 0x1111_1111_1111_1111u64,
                expected1 = in(reg) 0x2222_2222_2222_2222u64,
                expected31 = in(reg) 0xaaaa_aaaa_aaaa_aaaau64,
                // Tell Rust that these vectors are overwritten. Do not use
                // nomem/nostack/preserves_flags: BRK invokes a Rust handler.
                out("v0") _,
                out("v1") _,
                out("v31") _,
            );
        }
        failures
    });

    if failures == 0 {
        println!("[ OK ] SIMD/FP CONTEXT BRK RETURN");
    } else {
        // bits 0..4: q0, q1, q31, FPCR, FPSR respectively.
        println!("[FAIL] SIMD/FP CONTEXT MASK: {failures:#04x}");
    }
    failures == 0
}
