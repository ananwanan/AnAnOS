use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

const PERIPHERAL_BASE: usize = 0xFE00_0000;
const MAILBOX_BASE: usize = PERIPHERAL_BASE + 0x0000_B880;

const MAILBOX_READ: *const u32 = MAILBOX_BASE as *const u32;
const MAILBOX_STATUS: *const u32 = (MAILBOX_BASE + 0x18) as *const u32;
const MAILBOX_WRITE: *mut u32 = (MAILBOX_BASE + 0x20) as *mut u32;

const MAILBOX_STATUS_FULL: u32 = 1 << 31;
const MAILBOX_STATUS_EMPTY: u32 = 1 << 30;

pub const PROPERTY_CHANNEL: u32 = 8;

pub const REQUEST_CODE: u32 = 0x0000_0000;
pub const RESPONSE_SUCCESS: u32 = 0x8000_0000;
pub const RESPONSE_ERROR: u32 = 0x8000_0001;

pub const TAG_GET_FIRMWARE_REVISION: u32 = 0x0000_0001;
pub const TAG_GET_VC_MEMORY: u32 = 0x0001_0006;
pub const END_TAG: u32 = 0x0000_0000;

/// 将 ARM 物理地址转换成 VideoCore 可访问的总线地址。
///
/// 当前内核运行于 Raspberry Pi 4 的低地址内存区域，
/// 使用 0xC000_0000 alias 将其交给 VideoCore 固件。
const VC_BUS_ALIAS: usize = 0xC000_0000;

/// 防止硬件异常时永久卡死。
const SPIN_LIMIT: usize = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxError {
    /// 缓冲区地址没有按 16 字节对齐。
    UnalignedBuffer,

    /// 消息地址超出了 Mailbox 的 32 位地址范围。
    AddressTooHigh,

    /// Mailbox 长时间处于满状态。
    WriteTimeout,

    /// Mailbox 长时间没有返回结果。
    ReadTimeout,

    /// 固件返回的不是当前请求。
    InvalidResponse,

    /// Property Interface 报告请求失败。
    PropertyError,
}

pub struct Mailbox;

impl Mailbox {
    pub const fn new() -> Self {
        Self
    }

    /// 向指定 mailbox channel 提交一个消息缓冲区。
    ///
    /// 缓冲区必须：
    ///
    /// - 16 字节对齐
    /// - 位于 VideoCore 可访问的低地址内存
    /// - 第一个 u32 是消息总长度
    /// - 第二个 u32 是请求/响应状态码
    /// 原因:
    /// Mailbox 写入值的低四位用来保存 channel：
    /// 31                             4 3        0
    /// +-------------------------------+----------+
    /// |         消息地址高位          | channel  |
    /// +-------------------------------+----------+
    /// 因此地址低四位必须全是 0：
    /// address % 16 == 0
    /// 否则地址和 channel 会混在一起
    pub fn call(&self, channel: u32, buffer: *mut u32) -> Result<(), MailboxError> {
        let arm_address = buffer as usize;

        if arm_address & 0xF != 0 {
            return Err(MailboxError::UnalignedBuffer);
        }

        /*
         * Mailbox 寄存器只传递 32 位值。
         *
         * 当前 kernel、stack 和 mailbox buffer 都在低地址区域，
         * 因此可以映射到 VideoCore bus alias。
         */
        let bus_address = arm_address | VC_BUS_ALIAS;

        if bus_address > u32::MAX as usize {
            return Err(MailboxError::AddressTooHigh);
        }

        let message = (bus_address as u32 & !0xF) | (channel & 0xF);

        unsafe {
            /*
             * 确保消息内容已写入内存，然后才通知固件。
             *
             * 当前阶段 MMU/cache 尚未开启，但保留 barrier，
             * 后续开启缓存后仍然需要正确的内存顺序。
             */
            data_sync_barrier();

            let mut spins = 0;

            while read_volatile(MAILBOX_STATUS) & MAILBOX_STATUS_FULL != 0 {
                spins += 1;

                if spins >= SPIN_LIMIT {
                    return Err(MailboxError::WriteTimeout);
                }

                core::hint::spin_loop();
            }

            write_volatile(MAILBOX_WRITE, message);

            spins = 0;

            loop {
                while read_volatile(MAILBOX_STATUS) & MAILBOX_STATUS_EMPTY != 0 {
                    spins += 1;

                    if spins >= SPIN_LIMIT {
                        return Err(MailboxError::ReadTimeout);
                    }

                    core::hint::spin_loop();
                }

                let response = read_volatile(MAILBOX_READ);

                /*
                 * Mailbox 可能返回其他 channel 的消息。
                 * 只有返回值完全匹配当前请求时才算完成。
                 */
                if response == message {
                    break;
                }

                spins += 1;

                if spins >= SPIN_LIMIT {
                    return Err(MailboxError::InvalidResponse);
                }
            }

            data_sync_barrier();

            /*
             * Property Interface 把总响应码写回 buffer[1]。
             */
            match read_volatile(buffer.add(1)) {
                RESPONSE_SUCCESS => Ok(()),
                RESPONSE_ERROR => Err(MailboxError::PropertyError),
                _ => Err(MailboxError::InvalidResponse),
            }
        }
    }

    pub fn property_call(&self, buffer: *mut u32) -> Result<(), MailboxError> {
        self.call(PROPERTY_CHANNEL, buffer)
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

#[inline(always)]
unsafe fn data_sync_barrier() {
    unsafe {
        asm!("dsb sy", options(nostack, preserves_flags));
    }
}

#[repr(C, align(16))]
struct FirmwareRevisionMessage {
    size: u32,
    code: u32,

    tag: u32,
    value_buffer_size: u32,
    request_response_size: u32,
    revision: u32,

    end_tag: u32,
}

impl Mailbox {
    /// Firmware-owned physical memory. DT /memory is used separately for RAM
    /// discovery because this tag's 32-bit fields cannot describe high banks.
    pub fn vc_memory(&self) -> Result<(usize, usize), MailboxError> {
        #[repr(C, align(16))]
        struct Message([u32; 8]);

        let mut message = Message([32, REQUEST_CODE, TAG_GET_VC_MEMORY, 8, 0, 0, 0, END_TAG]);
        self.property_call(message.0.as_mut_ptr())?;
        if message.0[4] != 0x8000_0008 {
            return Err(MailboxError::InvalidResponse);
        }
        Ok((message.0[5] as usize, message.0[6] as usize))
    }

    /// 获取 VideoCore 固件版本号。
    pub fn firmware_revision(&self) -> Result<u32, MailboxError> {
        let mut message = FirmwareRevisionMessage {
            size: core::mem::size_of::<FirmwareRevisionMessage>() as u32,
            code: REQUEST_CODE,

            tag: TAG_GET_FIRMWARE_REVISION,
            value_buffer_size: 4,
            request_response_size: 0,
            revision: 0,

            end_tag: END_TAG,
        };

        self.property_call((&raw mut message).cast::<u32>())?;

        /*
         * Tag 响应字段的 bit 31 必须为 1。
         * 低 31 位表示固件实际返回的字节数。
         */
        if message.request_response_size & 0x8000_0000 == 0 {
            return Err(MailboxError::InvalidResponse);
        }

        let response_length = message.request_response_size & 0x7FFF_FFFF;

        if response_length < 4 {
            return Err(MailboxError::InvalidResponse);
        }

        Ok(message.revision)
    }
}
