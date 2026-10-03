//! Provisional M3/M4 AArch64 syscall contract, shared with EL0 programs.
//!
//! `svc #0`, x8 = number, x0..x5 = arguments, x0 = signed result. Successful
//! results are nonnegative; errors are `-errno`. This is a bootstrap ABI, not a
//! published libc or Linux ABI. Process entry and syscall numbers are documented
//! in docs/abi/; kernel-private Rust structures are never passed to userspace.

pub const SVC_IMMEDIATE: u16 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_EXIT: u64 = 2;
pub const SYS_YIELD: u64 = 3;
pub const SYS_READ: u64 = 4;
pub const SYS_OPEN: u64 = 5;
pub const SYS_CLOSE: u64 = 6;
pub const SYS_LSEEK: u64 = 7;
pub const SYS_FSTAT: u64 = 8;
pub const SYS_GETDENTS: u64 = 9;
pub const SYS_MKDIR: u64 = 10;
pub const SYS_UNLINK: u64 = 11;
pub const SYS_RENAME: u64 = 12;
pub const SYS_CHDIR: u64 = 13;
pub const SYS_GETCWD: u64 = 14;
pub const SYS_DUP: u64 = 15;
pub const SYS_DUP2: u64 = 16;
pub const SYS_GETPID: u64 = 17;
pub const SYS_SPAWN: u64 = 18;
pub const SYS_EXEC: u64 = 19;
pub const SYS_WAITPID: u64 = 20;
pub const SYS_CLOCK_GETTIME: u64 = 21;
pub const MAX_PATH_BYTES: usize = 256;
pub const MAX_ARGUMENTS: usize = 8;
pub const MAX_ARGUMENT_BYTES: usize = 256;
pub const STAT_BYTES: usize = 32;
pub const DIRENT_BYTES: usize = 80;
pub const O_RDONLY: u64 = 0;
pub const O_WRONLY: u64 = 1;
pub const O_RDWR: u64 = 2;
pub const O_CREAT: u64 = 0x40;
pub const O_EXCL: u64 = 0x80;
pub const O_TRUNC: u64 = 0x200;
pub const O_APPEND: u64 = 0x400;
pub const O_DIRECTORY: u64 = 0x1_0000;
pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;
pub const MAX_WRITE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Errno {
    NoEntry = 2,
    Io = 5,
    ExecFormat = 8,
    BadFileDescriptor = 9,
    NoChild = 10,
    Again = 11,
    NoMemory = 12,
    Permission = 13,
    Fault = 14,
    Busy = 16,
    Exists = 17,
    NotDirectory = 20,
    IsDirectory = 21,
    InvalidArgument = 22,
    TooManyFiles = 24,
    FileTooLarge = 27,
    NoSpace = 28,
    IllegalSeek = 29,
    ReadOnly = 30,
    Range = 34,
    NameTooLong = 36,
    NotImplemented = 38,
    NotEmpty = 39,
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
    Extension {
        number: u64,
        arguments: [u64; 6],
    },
}

/// Decode fixed-width register arguments. User-pointer permissions and mapping
/// validity are checked separately, before the kernel accesses a physical page.
pub fn decode_syscall(number: u64, arguments: [u64; 6]) -> Result<SyscallRequest, Errno> {
    match number {
        SYS_WRITE => {
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
        SYS_READ..=SYS_CLOCK_GETTIME => Ok(SyscallRequest::Extension { number, arguments }),
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
        // Descriptor existence/access is a per-process service check.
        assert!(decode_syscall(SYS_WRITE, [0, 0, 1, 0, 0, 0]).is_ok());
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
