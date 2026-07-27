use core::fmt::{self, Write};

use crate::drivers::uart::MiniUart;

/// 将格式化参数输出到当前内核控制台。
///
/// 现阶段控制台后端只有 Mini UART。
/// 后面可以同时转发到 UART、Framebuffer 和日志缓冲区。
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    let mut uart = MiniUart::new();

    // UART writer 当前不会主动返回错误。
    let _ = uart.write_fmt(args);
}