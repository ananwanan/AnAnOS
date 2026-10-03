//! EL0 SVC dispatch and bounded console copies, independent of task scheduling.
//!
//! The hardware runner and host tests use this same path. Only the physical
//! byte reader and console sink differ; no user VA is directly dereferenced.

use super::abi::{self, Errno, SyscallRequest};
use super::space::{self, UserAccess, UserChunk};
use crate::arch::context::ExceptionContext;
use crate::memory::paging::{PageTables, TableMemory};

pub const AARCH64_SVC_CLASS: u64 = 0x15;
const COPY_CHUNK_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Resume,
    Exit(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapError {
    InvalidOrigin,
    NotSvc,
}

/// The current kernel primitives exposed to the provisional syscall ABI.
/// `write` is called after stdout/stderr descriptor and length validation;
/// the service must validate user memory before accessing any bytes.
pub trait Services {
    fn write(&mut self, address: u64, length: usize) -> Result<u64, Errno>;
    fn yield_now(&mut self);
}

/// Handle an AArch64 EL0 `svc`. Returning calls change only x0; ELR already
/// points after SVC and must not be advanced. Exit records a status for the
/// caller, which must leave EL0 and switch roots before reclaiming its pages.
/// Invalid origins/classes leave the frame untouched and invoke no service.
pub fn dispatch(
    context: &mut ExceptionContext,
    services: &mut impl Services,
) -> Result<Action, TrapError> {
    if !abi::frame_from_el0(context.spsr_el1) {
        return Err(TrapError::InvalidOrigin);
    }
    if (context.esr_el1 >> 26) & 0x3f != AARCH64_SVC_CLASS {
        return Err(TrapError::NotSvc);
    }
    if context.esr_el1 & 0xffff != abi::SVC_IMMEDIATE as u64 {
        context.registers[0] = Errno::InvalidArgument.result();
        return Ok(Action::Resume);
    }
    let registers = &mut context.registers;
    let request = abi::decode_syscall(
        registers[8],
        [
            registers[0],
            registers[1],
            registers[2],
            registers[3],
            registers[4],
            registers[5],
        ],
    );
    let result = match request {
        Ok(SyscallRequest::Write {
            address, length, ..
        }) => services.write(address, length),
        Ok(SyscallRequest::Exit { status }) => return Ok(Action::Exit(status)),
        Ok(SyscallRequest::Yield) => {
            services.yield_now();
            Ok(0)
        }
        Err(error) => Err(error),
    };
    registers[0] = result.unwrap_or_else(Errno::result);
    Ok(Action::Resume)
}

/// Validate the complete buffer (including physical ownership), then stream
/// it using a small kernel scratch buffer. An invalid later page causes no
/// physical read or output. Empty writes never examine the pointer.
///
/// The caller must keep mappings and owned pages stable throughout this call.
/// `read_physical` fills the supplied slice from exactly the validated chunk;
/// the bare-metal service uses volatile reads of its privileged identity alias.
/// No allocation, active-table mutation or user-controlled Rust slice is used.
pub fn write_user_buffer(
    tables: &PageTables,
    memory: &impl TableMemory,
    address: u64,
    length: usize,
    owns: impl Fn(u64, usize) -> bool,
    mut read_physical: impl FnMut(UserChunk, &mut [u8]),
    mut output: impl FnMut(&[u8]),
) -> Result<u64, Errno> {
    if length > abi::MAX_WRITE_BYTES {
        return Err(Errno::InvalidArgument);
    }
    if length == 0 {
        return Ok(0);
    }
    space::validate_owned_user_range(tables, memory, address, length, UserAccess::Read, owns)
        .map_err(|_| Errno::Fault)?;
    let mut offset = 0;
    let mut buffer = [0u8; COPY_CHUNK_BYTES];
    while offset < length {
        let chunk = space::user_chunk(
            tables,
            memory,
            address + offset as u64,
            (length - offset).min(buffer.len()),
            UserAccess::Read,
        )
        .map_err(|_| Errno::Fault)?;
        read_physical(chunk, &mut buffer[..chunk.length]);
        output(&buffer[..chunk.length]);
        offset += chunk.length;
    }
    Ok(length as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::context::ExceptionFrame;
    use std::vec::Vec;

    #[derive(Default)]
    struct Model {
        writes: Vec<(u64, usize)>,
        yields: usize,
        write_error: Option<Errno>,
    }
    impl Services for Model {
        fn write(&mut self, address: u64, length: usize) -> Result<u64, Errno> {
            self.writes.push((address, length));
            self.write_error.map_or(Ok(length as u64), Err)
        }
        fn yield_now(&mut self) {
            self.yields += 1;
        }
    }

    fn frame(number: u64, arguments: [u64; 6]) -> ExceptionFrame {
        let mut registers = core::array::from_fn(|index| 0xabcd_0000 + index as u64);
        registers[..6].copy_from_slice(&arguments);
        registers[8] = number;
        ExceptionFrame {
            context: ExceptionContext {
                registers,
                elr_el1: space::USER_CODE + 4,
                spsr_el1: 0xa000_0340,
                esr_el1: (AARCH64_SVC_CLASS << 26) | (1 << 25),
            },
            simd: core::array::from_fn(|i| [i as u64, !(i as u64)]),
            fpcr: 1 << 24,
            fpsr: 1 << 27,
            sp_el0: space::USER_STACK_TOP - 16,
            far_el1: 0xdead_beef,
        }
    }

    #[test]
    fn returning_calls_only_change_x0_and_never_skip_the_post_svc_instruction() {
        for (number, arguments, expected, error) in [
            (
                abi::SYS_WRITE,
                [abi::STDOUT, space::USER_DATA, 17, 8, 9, 10],
                17,
                None,
            ),
            (
                abi::SYS_WRITE,
                [abi::STDERR, 0, 1, 0, 0, 0],
                Errno::Fault.result(),
                Some(Errno::Fault),
            ),
            (abi::SYS_YIELD, [0; 6], 0, None),
            (0xffff, [0; 6], Errno::NotImplemented.result(), None),
        ] {
            let mut services = Model {
                write_error: error,
                ..Model::default()
            };
            let mut saved = frame(number, arguments);
            let mut expected_frame = saved;
            expected_frame.context.registers[0] = expected;
            assert_eq!(
                dispatch(&mut saved.context, &mut services),
                Ok(Action::Resume)
            );
            assert_eq!(saved, expected_frame);
            assert_eq!(services.yields, usize::from(number == abi::SYS_YIELD));
            assert_eq!(services.writes.len(), usize::from(number == abi::SYS_WRITE));
            if number == abi::SYS_WRITE {
                assert_eq!(services.writes, [(arguments[1], arguments[2] as usize)]);
            }
        }
    }

    #[test]
    fn exit_decodes_low_signed_32_bits_without_returning_or_invoking_io() {
        for (argument, status) in [(0, 0), (u64::MAX, -1), (0x1234_5678_8000_0000, i32::MIN)] {
            let mut services = Model::default();
            let mut saved = frame(abi::SYS_EXIT, [argument, 0, 0, 0, 0, 0]);
            let before = saved;
            assert_eq!(
                dispatch(&mut saved.context, &mut services),
                Ok(Action::Exit(status))
            );
            assert_eq!(saved, before);
            assert!(services.writes.is_empty());
            assert_eq!(services.yields, 0);
        }
    }

    #[test]
    fn malformed_requests_return_errno_without_accessing_memory_or_exiting() {
        for (number, arguments, immediate, expected) in [
            (
                abi::SYS_WRITE,
                [0, 0, 0, 0, 0, 0],
                0,
                Errno::BadFileDescriptor,
            ),
            (
                abi::SYS_WRITE,
                [abi::STDOUT, 0, 4097, 0, 0, 0],
                0,
                Errno::InvalidArgument,
            ),
            (
                abi::SYS_WRITE,
                [abi::STDOUT, 0, u64::MAX, 0, 0, 0],
                0,
                Errno::InvalidArgument,
            ),
            (
                abi::SYS_EXIT,
                [99, 0, 0, 0, 0, 0],
                1,
                Errno::InvalidArgument,
            ),
        ] {
            let mut services = Model::default();
            let mut saved = frame(number, arguments);
            saved.context.esr_el1 |= immediate;
            let mut expected_frame = saved;
            expected_frame.context.registers[0] = expected.result();
            assert_eq!(
                dispatch(&mut saved.context, &mut services),
                Ok(Action::Resume)
            );
            assert_eq!(saved, expected_frame);
            assert!(services.writes.is_empty());
            assert_eq!(services.yields, 0);
        }
    }

    #[test]
    fn non_el0_or_non_svc_traps_leave_state_and_services_untouched() {
        for (mode, class, error) in [
            (5, AARCH64_SVC_CLASS, TrapError::InvalidOrigin),
            (0x10, AARCH64_SVC_CLASS, TrapError::InvalidOrigin),
            (0, 0x24, TrapError::NotSvc),
            (0, 0x11, TrapError::NotSvc),
        ] {
            let mut saved = frame(abi::SYS_EXIT, [0; 6]);
            saved.context.spsr_el1 = mode;
            saved.context.esr_el1 = class << 26;
            let before = saved;
            let mut services = Model::default();
            assert_eq!(dispatch(&mut saved.context, &mut services), Err(error));
            assert_eq!(saved, before);
            assert!(services.writes.is_empty());
            assert_eq!(services.yields, 0);
        }
    }
}
