use core::fmt;

use crate::drivers::framebuffer::FrameBuffer;
use crate::graphics::font;

pub const DEFAULT_FOREGROUND: u32 = 0x00F0_F4F8;
pub const DEFAULT_BACKGROUND: u32 = 0x0018_2430;

/// 屏幕控制台:这是一个简单的文本控制台，用于在屏幕上显示文本
pub struct ScreenConsole {
    framebuffer: FrameBuffer,

    cursor_x: u32,
    cursor_y: u32,

    margin_x: u32,
    margin_y: u32,

    scale: u32,

    foreground: u32,
    background: u32,
}

impl ScreenConsole {
    pub fn new(mut framebuffer: FrameBuffer) -> Self {
        framebuffer.clear(DEFAULT_BACKGROUND);

        Self {
            framebuffer,

            cursor_x: 24,
            cursor_y: 24,

            margin_x: 24,
            margin_y: 24,

            // 1920×1080 下，scale=2 比较合适。
            scale: 2,

            foreground: DEFAULT_FOREGROUND,
            background: DEFAULT_BACKGROUND,
        }
    }

    pub fn framebuffer(&self) -> &FrameBuffer {
        &self.framebuffer
    }

    pub fn framebuffer_mut(&mut self) -> &mut FrameBuffer {
        &mut self.framebuffer
    }

    pub fn set_colors(&mut self, foreground: u32, background: u32) {
        self.foreground = foreground;
        self.background = background;
    }

    pub fn set_foreground(&mut self, color: u32) {
        self.foreground = color;
    }

    pub fn set_background(&mut self, color: u32) {
        self.background = color;
    }

    pub fn clear(&mut self) {
        self.framebuffer.clear(self.background);
        self.cursor_x = self.margin_x;
        self.cursor_y = self.margin_y;
    }

    fn character_width(&self) -> u32 {
        8 * self.scale
    }

    fn character_height(&self) -> u32 {
        8 * self.scale
    }

    fn horizontal_advance(&self) -> u32 {
        self.character_width() + self.scale
    }

    fn line_height(&self) -> u32 {
        self.character_height() + self.scale * 2
    }

    fn carriage_return(&mut self) {
        self.cursor_x = self.margin_x;
    }

    fn newline(&mut self) {
        self.cursor_x = self.margin_x;
        self.cursor_y = self.cursor_y.saturating_add(self.line_height());

        self.ensure_cursor_visible();
    }

    fn ensure_cursor_visible(&mut self) {
        let bottom_limit = self.framebuffer.height().saturating_sub(self.margin_y);

        if self.cursor_y + self.character_height() <= bottom_limit {
            return;
        }

        let scroll_rows = self.line_height();

        self.framebuffer.scroll_up(scroll_rows, self.background);

        self.cursor_y = self.cursor_y.saturating_sub(scroll_rows);
    }

    fn write_character(&mut self, character: char) {
        match character {
            '\n' => {
                self.newline();
                return;
            }

            '\r' => {
                self.carriage_return();
                return;
            }

            '\t' => {
                for _ in 0..4 {
                    self.write_character(' ');
                }
                return;
            }

            // 暂时把不可显示的控制字符忽略掉。
            character if character.is_control() => {
                return;
            }

            _ => {}
        }

        let right_limit = self.framebuffer.width().saturating_sub(self.margin_x);

        if self.cursor_x + self.character_width() > right_limit {
            self.newline();
        }

        self.ensure_cursor_visible();

        let glyph = font::glyph(character);
        for (row, bits) in glyph.iter().copied().enumerate() {
            for column in 0..8u32 {
                let mask = 1u8 << (7 - column);

                if bits & mask == 0 {
                    continue;
                }

                let x = self.cursor_x + column * self.scale;
                let y = self.cursor_y + row as u32 * self.scale;

                self.framebuffer
                    .draw_rect(x, y, self.scale, self.scale, self.foreground);
            }
        }

        self.cursor_x += self.horizontal_advance();
    }
}

impl fmt::Write for ScreenConsole {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            self.write_character(character);
        }

        Ok(())
    }
}
