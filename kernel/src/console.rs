use core::cell::UnsafeCell;
use core::fmt::{self, Write};

use crate::drivers::framebuffer::FrameBuffer;
use crate::drivers::uart::MiniUart;
use crate::graphics::console::ScreenConsole;

struct KernelConsole {
    screen: Option<ScreenConsole>,
}

impl KernelConsole {
    const fn new() -> Self {
        Self { screen: None }
    }

    fn install_framebuffer(&mut self, framebuffer: FrameBuffer) {
        self.screen = Some(ScreenConsole::new(framebuffer));
    }

    fn write_arguments(&mut self, arguments: fmt::Arguments<'_>) {
        /*
         * fmt::Arguments 不能被消费两次，所以分别调用
         * fmt::write，并使用 arguments.clone()。
         */
        let mut uart = MiniUart::new();
        let _ = uart.write_fmt(arguments.clone());

        if let Some(screen) = self.screen.as_mut() {
            let _ = screen.write_fmt(arguments);
        }
    }

    fn set_screen_foreground(&mut self, color: u32) {
        if let Some(screen) = self.screen.as_mut() {
            screen.set_foreground(color);
        }
    }

    fn set_screen_colors(&mut self, foreground: u32, background: u32) {
        if let Some(screen) = self.screen.as_mut() {
            screen.set_colors(foreground, background);
        }
    }

    fn clear_screen(&mut self) {
        if let Some(screen) = self.screen.as_mut() {
            screen.clear();
        }
    }
}

/// 为全局可变控制台提供内部可变性。
///
/// 当前约束：
/// - 只运行 CPU0
/// - 尚未开启中断
/// - 所有日志同步执行
///
/// 开启中断或多核前必须替换成自旋锁。
struct ConsoleCell {
    inner: UnsafeCell<KernelConsole>,
}

impl ConsoleCell {
    const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(KernelConsole::new()),
        }
    }

    fn with<R>(&self, function: impl FnOnce(&mut KernelConsole) -> R) -> R {
        unsafe { function(&mut *self.inner.get()) }
    }
}

/*
 * 我们手动保证现阶段不会并发访问。
 * 开启中断或多核之后，这个保证将不再成立。
 */
unsafe impl Sync for ConsoleCell {}

static CONSOLE: ConsoleCell = ConsoleCell::new();

pub fn install_framebuffer(framebuffer: FrameBuffer) {
    CONSOLE.with(|console| {
        console.install_framebuffer(framebuffer);
    });
}

#[doc(hidden)]
pub fn _print(arguments: fmt::Arguments<'_>) {
    CONSOLE.with(|console| {
        console.write_arguments(arguments);
    });
}

pub fn set_screen_foreground(color: u32) {
    CONSOLE.with(|console| {
        console.set_screen_foreground(color);
    });
}

pub fn set_screen_colors(foreground: u32, background: u32) {
    CONSOLE.with(|console| {
        console.set_screen_colors(foreground, background);
    });
}

pub fn clear_screen() {
    CONSOLE.with(|console| {
        console.clear_screen();
    });
}
