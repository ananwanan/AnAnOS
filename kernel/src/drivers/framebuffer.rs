use core::ptr::{read_volatile, write_volatile};

use crate::drivers::mailbox::{Mailbox, MailboxError, REQUEST_CODE};

use crate::graphics::font;

const TAG_SET_PHYSICAL_SIZE: u32 = 0x0004_8003;
const TAG_SET_VIRTUAL_SIZE: u32 = 0x0004_8004;
const TAG_SET_VIRTUAL_OFFSET: u32 = 0x0004_8009;
const TAG_SET_DEPTH: u32 = 0x0004_8005;
const TAG_SET_PIXEL_ORDER: u32 = 0x0004_8006;
const TAG_ALLOCATE_BUFFER: u32 = 0x0004_0001;
const TAG_GET_PITCH: u32 = 0x0004_0008;
const END_TAG: u32 = 0;

const TAG_RESPONSE_BIT: u32 = 0x8000_0000;

/// VideoCore 返回的 framebuffer 地址可能带有总线地址别名。
const VC_ADDRESS_MASK: u32 = 0x3FFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameBufferError {
    Mailbox(MailboxError),
    InvalidResponse,
    InvalidDimensions,
    InvalidPitch,
    InvalidAddress,
    UnsupportedDepth(u32),
    NullAddress,
    BufferTooSmall,
}

impl From<MailboxError> for FrameBufferError {
    fn from(error: MailboxError) -> Self {
        Self::Mailbox(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelOrder {
    Bgr,
    Rgb,
}

pub struct FrameBuffer {
    address: *mut u8,
    width: u32,
    height: u32,
    pitch: u32,
    depth: u32,
    size: u32,
    pixel_order: PixelOrder,
}

impl FrameBuffer {
    /// 通过树莓派固件申请一个 32 位 framebuffer。
    pub fn new(requested_width: u32, requested_height: u32) -> Result<Self, FrameBufferError> {
        /*
         * Property Mailbox 消息必须按 16 字节对齐。
         *
         * 使用固定数组可以精确控制每一个 u32 的位置，避免结构体
         * 尾部填充或字段布局错误。
         */
        #[repr(C, align(16))]
        struct AlignedMessage {
            data: [u32; 36],
        }

        let mut message = AlignedMessage { data: [0; 36] };
        let data = &mut message.data;

        /*
         * 消息布局：
         *
         *  0  消息总长度
         *  1  请求/响应码
         *
         *  2  设置物理尺寸
         *  7  设置虚拟尺寸
         * 12  设置虚拟偏移
         * 17  设置色深
         * 21  设置像素顺序
         * 25  申请 framebuffer
         * 30  获取 pitch
         * 34  结束标签
         */

        data[0] = (35 * core::mem::size_of::<u32>()) as u32;
        data[1] = REQUEST_CODE;

        // 设置物理尺寸。
        data[2] = TAG_SET_PHYSICAL_SIZE;
        data[3] = 8;
        data[4] = 8;
        data[5] = requested_width;
        data[6] = requested_height;

        // 设置虚拟尺寸。
        data[7] = TAG_SET_VIRTUAL_SIZE;
        data[8] = 8;
        data[9] = 8;
        data[10] = requested_width;
        data[11] = requested_height;

        // 设置虚拟偏移。
        data[12] = TAG_SET_VIRTUAL_OFFSET;
        data[13] = 8;
        data[14] = 8;
        data[15] = 0;
        data[16] = 0;

        // 请求每像素 32 位。
        data[17] = TAG_SET_DEPTH;
        data[18] = 4;
        data[19] = 4;
        data[20] = 32;

        /*
         * 请求 RGB 像素顺序。
         *
         * 0 = BGR
         * 1 = RGB
         */
        data[21] = TAG_SET_PIXEL_ORDER;
        data[22] = 4;
        data[23] = 4;
        data[24] = 1;

        /*
         * 申请 framebuffer。
         *
         * 输入：
         * data[28] = 对齐要求
         *
         * 输出：
         * data[28] = framebuffer 总线地址
         * data[29] = framebuffer 大小
         */
        data[25] = TAG_ALLOCATE_BUFFER;
        data[26] = 8;
        data[27] = 4;
        data[28] = 4096;
        data[29] = 0;

        // 获取每行字节数。
        data[30] = TAG_GET_PITCH;
        data[31] = 4;
        data[32] = 0;
        data[33] = 0;

        data[34] = END_TAG;

        Mailbox::new().property_call(data.as_mut_ptr())?;

        validate_tag_response(data[4], 8)?;
        validate_tag_response(data[9], 8)?;
        validate_tag_response(data[14], 8)?;
        validate_tag_response(data[19], 4)?;
        validate_tag_response(data[23], 4)?;
        validate_tag_response(data[27], 8)?;
        validate_tag_response(data[32], 4)?;

        let width = data[5];
        let height = data[6];
        let depth = data[20];
        let pixel_order = match data[24] {
            0 => PixelOrder::Bgr,
            1 => PixelOrder::Rgb,
            _ => return Err(FrameBufferError::InvalidResponse),
        };

        let framebuffer_bus_address = data[28];
        let size = data[29];
        let pitch = data[33];

        if width == 0 || height == 0 || pitch == 0 {
            return Err(FrameBufferError::InvalidDimensions);
        }

        if depth != 32 {
            return Err(FrameBufferError::UnsupportedDepth(depth));
        }

        let row_bytes = width.checked_mul(4).ok_or(FrameBufferError::InvalidPitch)?;
        if pitch < row_bytes || pitch & 3 != 0 {
            return Err(FrameBufferError::InvalidPitch);
        }

        if framebuffer_bus_address == 0 {
            return Err(FrameBufferError::NullAddress);
        }

        let minimum_size = pitch
            .checked_mul(height)
            .ok_or(FrameBufferError::BufferTooSmall)?;

        if size < minimum_size {
            return Err(FrameBufferError::BufferTooSmall);
        }

        /*
         * VideoCore 返回的是总线地址，需要去掉顶部的地址别名位，
         * 得到 ARM 当前可访问的物理地址。
         */
        let arm_address = (framebuffer_bus_address & VC_ADDRESS_MASK) as usize;
        if arm_address == 0
            || arm_address & 3 != 0
            || arm_address
                .checked_add(size as usize)
                .is_none_or(|end| end > 0x4000_0000)
        {
            return Err(FrameBufferError::InvalidAddress);
        }

        Ok(Self {
            address: arm_address as *mut u8,
            width,
            height,
            pitch,
            depth,
            size,
            pixel_order,
        })
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn pitch(&self) -> u32 {
        self.pitch
    }

    pub const fn depth(&self) -> u32 {
        self.depth
    }

    pub const fn size(&self) -> u32 {
        self.size
    }

    pub fn address(&self) -> usize {
        self.address as usize
    }

    pub const fn pixel_order(&self) -> PixelOrder {
        self.pixel_order
    }

    /// 向指定位置写入一个 RGB 像素。
    pub fn put_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }

        let offset = y as usize * self.pitch as usize + x as usize * 4;

        let red = ((color >> 16) & 0xFF) as u8;
        let green = ((color >> 8) & 0xFF) as u8;
        let blue = (color & 0xFF) as u8;

        let pixel = match self.pixel_order {
            /*
             * 内存是小端序。
             *
             * 写入 0x00RRGGBB 时，内存字节顺序为：
             * BB GG RR 00
             */
            PixelOrder::Rgb => ((red as u32) << 16) | ((green as u32) << 8) | blue as u32,

            PixelOrder::Bgr => ((blue as u32) << 16) | ((green as u32) << 8) | red as u32,
        };

        unsafe {
            write_volatile(self.address.add(offset).cast::<u32>(), pixel);
        }
    }

    /// 使用同一种颜色填满屏幕。
    pub fn clear(&mut self, color: u32) {
        let pixel = self.encode_color(color);

        for y in 0..self.height as usize {
            let row = unsafe { self.address.add(y * self.pitch as usize).cast::<u32>() };

            for x in 0..self.width as usize {
                unsafe {
                    write_volatile(row.add(x), pixel);
                }
            }
        }
    }

    pub fn draw_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }

        let end_x = x.saturating_add(width).min(self.width);
        let end_y = y.saturating_add(height).min(self.height);
        let pixel = self.encode_color(color);

        for py in y as usize..end_y as usize {
            let row = unsafe { self.address.add(py * self.pitch as usize).cast::<u32>() };

            for px in x as usize..end_x as usize {
                unsafe {
                    write_volatile(row.add(px), pixel);
                }
            }
        }
    }

    /// 绘制一个经过缩放的 8×8 字符。
    ///
    /// `foreground` 和 `background` 使用 0x00RRGGBB 格式。
    pub fn draw_char_scaled(
        &mut self,
        x: u32,
        y: u32,
        character: char,
        foreground: u32,
        background: u32,
        scale: u32,
    ) {
        if scale == 0 {
            return;
        }

        let glyph = font::glyph(character);

        for (row, bits) in glyph.iter().copied().enumerate() {
            for column in 0..8u32 {
                let mask = 1u8 << (7 - column);
                let color = if bits & mask != 0 {
                    foreground
                } else {
                    background
                };

                let pixel_x = x.saturating_add(column.saturating_mul(scale));
                let pixel_y = y.saturating_add((row as u32).saturating_mul(scale));

                self.draw_rect(pixel_x, pixel_y, scale, scale, color);
            }
        }
    }

    /// 绘制字符串。
    ///
    /// 支持：
    /// - `\n` 换行
    /// - `\r` 回到行首
    /// - 自动忽略当前字体不支持的非 ASCII 字符
    pub fn draw_string_scaled(
        &mut self,
        x: u32,
        y: u32,
        text: &str,
        foreground: u32,
        background: u32,
        scale: u32,
    ) {
        if scale == 0 {
            return;
        }

        let origin_x = x;
        let mut cursor_x = x;
        let mut cursor_y = y;

        let character_width = 8u32.saturating_mul(scale);
        let character_height = 8u32.saturating_mul(scale);

        // 额外留一个像素宽度作为字符间距。
        let horizontal_advance = character_width.saturating_add(scale);

        let vertical_advance = character_height.saturating_add(scale.saturating_mul(2));

        for character in text.chars() {
            match character {
                '\n' => {
                    cursor_x = origin_x;
                    cursor_y = cursor_y.saturating_add(vertical_advance);
                }

                '\r' => {
                    cursor_x = origin_x;
                }

                '\t' => {
                    cursor_x = cursor_x.saturating_add(horizontal_advance.saturating_mul(4));
                }

                _ => {
                    /*
                     * 简单自动换行。
                     */
                    if cursor_x.saturating_add(character_width) > self.width {
                        cursor_x = origin_x;
                        cursor_y = cursor_y.saturating_add(vertical_advance);
                    }

                    /*
                     * 超出屏幕底部后停止绘制。
                     * 后面实现图形控制台时再加入滚屏。
                     */
                    if cursor_y.saturating_add(character_height) > self.height {
                        break;
                    }

                    self.draw_char_scaled(
                        cursor_x, cursor_y, character, foreground, background, scale,
                    );

                    cursor_x = cursor_x.saturating_add(horizontal_advance);
                }
            }
        }
    }

    /// 将 framebuffer 内容向上滚动指定像素行。
    pub fn scroll_up(&mut self, rows: u32, background: u32) {
        if rows == 0 {
            return;
        }

        if rows >= self.height {
            self.clear(background);
            return;
        }

        let bytes_per_row = self.pitch as usize;
        let copy_rows = (self.height - rows) as usize;
        let source_offset = rows as usize * bytes_per_row;

        /*
         * 源区域地址高于目标区域，因此从前向后复制是安全的。
         * 这里使用 volatile，确保编译器不会优化掉显存访问。
         */
        for row in 0..copy_rows {
            let source_row = source_offset + row * bytes_per_row;
            let destination_row = row * bytes_per_row;

            for byte_offset in (0..bytes_per_row).step_by(4) {
                unsafe {
                    let pixel =
                        read_volatile(self.address.add(source_row + byte_offset).cast::<u32>());

                    write_volatile(
                        self.address
                            .add(destination_row + byte_offset)
                            .cast::<u32>(),
                        pixel,
                    );
                }
            }
        }

        // 清空滚动后底部留下的区域。
        self.draw_rect(0, self.height - rows, self.width, rows, background);
    }

    #[inline(always)]
    fn encode_color(&self, color: u32) -> u32 {
        let red = (color >> 16) & 0xFF;
        let green = (color >> 8) & 0xFF;
        let blue = color & 0xFF;

        match self.pixel_order {
            PixelOrder::Rgb => (red << 16) | (green << 8) | blue,
            PixelOrder::Bgr => (blue << 16) | (green << 8) | red,
        }
    }
}

fn validate_tag_response(response_field: u32, minimum_length: u32) -> Result<(), FrameBufferError> {
    if response_field & TAG_RESPONSE_BIT == 0 {
        return Err(FrameBufferError::InvalidResponse);
    }

    let response_length = response_field & !TAG_RESPONSE_BIT;

    if response_length < minimum_length {
        return Err(FrameBufferError::InvalidResponse);
    }

    Ok(())
}
