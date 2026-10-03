use core::cell::{Cell, UnsafeCell};
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
/// CPU0 accesses are IRQ-masked; synchronous exception/panic reentry falls
/// back to UART without borrowing the framebuffer again. Not an SMP lock.
struct ConsoleCell {
    inner: UnsafeCell<KernelConsole>,
    busy: Cell<bool>,
}

impl ConsoleCell {
    const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(KernelConsole::new()),
            busy: Cell::new(false),
        }
    }

    fn with<R>(&self, function: impl FnOnce(&mut KernelConsole) -> R) -> Option<R> {
        crate::arch::interrupt::with_irq_masked(|| {
            if self.busy.replace(true) {
                return None;
            }
            struct Release<'a>(&'a Cell<bool>);
            impl Drop for Release<'_> {
                fn drop(&mut self) {
                    self.0.set(false);
                }
            }
            let _release = Release(&self.busy);
            // CPU0 owns this cell, IRQ is masked, and reentry was excluded.
            Some(unsafe { function(&mut *self.inner.get()) })
        })
    }
}

/*
 * Secondary cores are parked. The IRQ guard and busy flag serialize CPU0.
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
    if CONSOLE
        .with(|console| {
            console.write_arguments(arguments);
        })
        .is_none()
    {
        let _ = MiniUart::new().write_fmt(arguments);
    }
}

/// Byte-oriented early stdout/stderr. UART receives bytes (with the existing
/// CR/LF convention); the ASCII framebuffer sink substitutes unsupported glyphs.
pub fn write_bytes(bytes: &[u8]) {
    fn uart_bytes(bytes: &[u8]) {
        let uart = MiniUart::new();
        for &byte in bytes {
            if byte == b'\n' {
                uart.write_byte(b'\r');
            }
            uart.write_byte(byte);
        }
    }
    if CONSOLE
        .with(|console| {
            uart_bytes(bytes);
            if let Some(screen) = console.screen.as_mut() {
                for &byte in bytes {
                    let character = if byte.is_ascii() { byte as char } else { '?' };
                    let _ = screen.write_char(character);
                }
            }
        })
        .is_none()
    {
        uart_bytes(bytes);
    }
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
