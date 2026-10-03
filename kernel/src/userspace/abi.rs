//! Provisional M3 AArch64 syscall contract, shared with embedded EL0 programs.
//!
//! `svc #0`, x8 = number, x0..x5 = arguments, x0 = signed result. Successful
//! results are nonnegative; errors are `-errno`. This is a bootstrap ABI, not a
//! published libc or Linux ABI. Process entry and syscall numbers are documented
//! in docs/abi/; kernel-private Rust structures are never passed to userspace.

pub const SVC_IMMEDIATE: u16 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_EXIT: u64 = 2;
pub const SYS_YIELD: u64 = 3;
pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;
pub const MAX_WRITE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Errno {
    BadFileDescriptor = 9,
    Fault = 14,
    InvalidArgument = 22,
    NotImplemented = 38,
}

impl Errno {
    pub const fn result(self) -> u64 {
        (-(self as i64)) as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyscallRequest {
    Write {
        fd: u64,
        address: u64,
        length: usize,
    },
    Exit {
        status: i32,
    },
    Yield,
}

/// Decode fixed-width register arguments. User-pointer permissions and mapping
/// validity are checked separately, before the kernel accesses a physical page.
pub fn decode_syscall(number: u64, arguments: [u64; 6]) -> Result<SyscallRequest, Errno> {
    match number {
        SYS_WRITE => {
            if arguments[0] != STDOUT && arguments[0] != STDERR {
                return Err(Errno::BadFileDescriptor);
            }
            if arguments[2] > MAX_WRITE_BYTES as u64 {
                return Err(Errno::InvalidArgument);
            }
            Ok(SyscallRequest::Write {
                fd: arguments[0],
                address: arguments[1],
                length: arguments[2] as usize,
            })
        }
        SYS_EXIT => Ok(SyscallRequest::Exit {
            status: arguments[0] as u32 as i32,
        }),
        SYS_YIELD => Ok(SyscallRequest::Yield),
        _ => Err(Errno::NotImplemented),
    }
}

/// AArch64 EL0t is the only lower-EL mode supported by the M3 entry/return path.
/// nRW is part of the low-five-bit mode field, so AArch32 is excluded too.
pub const fn frame_from_el0(saved_pstate: u64) -> bool {
    saved_pstate & 0b1_1111 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_write_and_exit_arguments_decode() {
        for fd in [STDOUT, STDERR] {
            assert_eq!(
                decode_syscall(SYS_WRITE, [fd, 0x40_0000_1234, 4096, 0, 0, 0]),
                Ok(SyscallRequest::Write {
                    fd,
                    address: 0x40_0000_1234,
                    length: 4096,
                })
            );
        }
        assert_eq!(
            decode_syscall(SYS_EXIT, [u64::MAX, 0, 0, 0, 0, 0]),
            Ok(SyscallRequest::Exit { status: -1 })
        );
    }

    #[test]
    fn invalid_syscall_arguments_and_unknown_numbers_return_errno() {
        assert_eq!(decode_syscall(999, [0; 6]), Err(Errno::NotImplemented));
        assert_eq!(
            decode_syscall(SYS_WRITE, [0, 0, 1, 0, 0, 0]),
            Err(Errno::BadFileDescriptor)
        );
        for length in [4097, u64::MAX] {
            assert_eq!(
                decode_syscall(SYS_WRITE, [STDOUT, 0, length, 0, 0, 0]),
                Err(Errno::InvalidArgument)
            );
        }
        assert_eq!(Errno::Fault.result() as i64, -14);
        assert_eq!(Errno::NotImplemented.result() as i64, -38);
    }

    #[test]
    fn saved_pstate_origin_requires_aarch64_el0t() {
        assert!(frame_from_el0(0));
        assert!(frame_from_el0(0x3c0 | 0xf000_0000));
        for mode in [4, 5, 8, 9, 12, 13, 16, 0x1f] {
            assert!(!frame_from_el0(mode));
        }
    }
}
