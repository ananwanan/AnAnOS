//! CPU0 EL0 bring-up tasks. No scheduler, ELF loader or hosted runtime yet.
//!
//! Every task has private pages and an offline-built root. No mutable state
//! borrow spans `eret`; exceptions borrow the state only after EL0 has stopped.

use core::cell::UnsafeCell;
use core::ptr;

use super::abi::{self, Errno, SyscallRequest};
use super::space::{self, UserAccess};
use crate::arch::{exception, interrupt, mmu as cpu, user};
use crate::memory::mmu::{self, PhysicalTableMemory};
use crate::memory::page::{PageError, PhysicalPages};
use crate::memory::paging::{PageTables, PagingError};

const PAGE_SIZE: usize = 4096;
const MAX_TASK_TICKS: u64 = 3;

#[repr(C, align(16))]
pub struct UserExceptionFrame {
    pub context: exception::ExceptionContext,
    pub simd: [[u64; 2]; 32],
    pub fpcr: u64,
    pub fpsr: u64,
    pub sp_el0: u64,
    pub far_el1: u64,
}
const _: () = assert!(core::mem::size_of::<UserExceptionFrame>() == 816);
const _: () = assert!(core::mem::offset_of!(UserExceptionFrame, sp_el0) == 800);
const _: () = assert!(core::mem::offset_of!(UserExceptionFrame, far_el1) == 808);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pending,
    Exit(i32),
    Fault {
        esr: u64,
        pc: u64,
        address: u64,
        stack: u64,
    },
    Timeout,
}

#[derive(Debug)]
pub enum UserError {
    Memory(crate::memory::mmu::InitError),
    Cpu(cpu::MmuError),
    Pages(PageError),
    Paging(PagingError),
    InvalidImage,
    Busy,
    InvalidMapping,
    ProbeFailed(u64),
    UnexpectedOutcome,
    LeakedPages,
}
impl From<PageError> for UserError {
    fn from(value: PageError) -> Self {
        Self::Pages(value)
    }
}
impl From<PagingError> for UserError {
    fn from(value: PagingError) -> Self {
        Self::Paging(value)
    }
}
impl From<cpu::MmuError> for UserError {
    fn from(value: cpu::MmuError) -> Self {
        Self::Cpu(value)
    }
}

struct State {
    memory: PhysicalTableMemory,
    tables: Option<PageTables>,
    code: Option<PhysicalPages>,
    data: Option<PhysicalPages>,
    stack: Option<PhysicalPages>,
    running: bool,
    outcome: Outcome,
    irq_count: u64,
    writes: u64,
    rejected_pointers: u64,
    unknown_syscalls: u64,
    yields: u64,
}

impl State {
    const fn new() -> Self {
        Self {
            memory: PhysicalTableMemory::new(),
            tables: None,
            code: None,
            data: None,
            stack: None,
            running: false,
            outcome: Outcome::Pending,
            irq_count: 0,
            writes: 0,
            rejected_pointers: 0,
            unknown_syscalls: 0,
            yields: 0,
        }
    }

    fn owns_chunk(&self, address: u64, length: usize) -> bool {
        let Some(end) = address.checked_add(length as u64) else {
            return false;
        };
        [&self.code, &self.data, &self.stack]
            .into_iter()
            .flatten()
            .any(|pages| {
                let start = pages.start_address() as u64;
                address >= start && end <= start + pages.byte_len() as u64
            })
    }

    /// Only after switching back to the kernel root and completing full TLBI.
    fn reclaim(&mut self) -> Result<(), UserError> {
        self.tables = None;
        self.memory.release_all();
        for slot in [&mut self.code, &mut self.data, &mut self.stack] {
            if let Some(pages) = slot.take() {
                crate::memory::free_pages(pages)?;
            }
        }
        self.running = false;
        Ok(())
    }
}

struct TaskCell(UnsafeCell<State>);
// SAFETY: only CPU0 runs. Every short borrow masks IRQ; none spans entry to EL0.
unsafe impl Sync for TaskCell {}
impl TaskCell {
    fn with<R>(&self, function: impl FnOnce(&mut State) -> R) -> R {
        interrupt::with_irq_masked(|| unsafe { function(&mut *self.0.get()) })
    }
}
static TASK: TaskCell = TaskCell(UnsafeCell::new(State::new()));

#[derive(Clone, Copy)]
enum Case {
    Hello,
    KernelFault,
    ReadOnlyFault,
    GuardFault,
    Timeout,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Hello => "HELLO/SVC",
            Self::KernelFault => "KERNEL ISOLATION",
            Self::ReadOnlyFault => "READ-ONLY CODE",
            Self::GuardFault => "STACK GUARD",
            Self::Timeout => "EL0 TIMER TIMEOUT",
        }
    }
    fn image(self) -> &'static [u8] {
        match self {
            Self::Hello => user::demo_image(),
            Self::KernelFault => user::kernel_fault_image(),
            Self::ReadOnlyFault => user::readonly_fault_image(),
            Self::GuardFault => user::guard_fault_image(),
            Self::Timeout => user::timeout_image(),
        }
    }
}

pub fn run_demos() -> Result<(), UserError> {
    if exception::irq_is_masked() || !cpu::is_enabled() {
        return Err(UserError::InvalidMapping);
    }
    let before = crate::memory::page_stats();
    let heap_before = crate::memory::heap_free_bytes();
    for case in [
        Case::Hello,
        Case::KernelFault,
        Case::ReadOnlyFault,
        Case::GuardFault,
        Case::Timeout,
    ] {
        run_case(case)?;
        if crate::memory::page_stats() != before || crate::memory::heap_free_bytes() != heap_before
        {
            return Err(UserError::LeakedPages);
        }
    }
    crate::println!("[ OK ] EL0 ADDRESS-SPACE/PAGE RECLAIM");
    Ok(())
}

fn run_case(case: Case) -> Result<(), UserError> {
    crate::println!("[INFO] EL0 TASK: {}", case.name());
    let kernel_root = mmu::kernel_root().map_err(UserError::Memory)?;
    // IRQ was enabled by GIC setup. This closure restores that mask on return;
    // user::run therefore permits EL0 IRQs, without holding the cell borrow.
    let preparation = TASK.with(|state| prepare(state, case.image()));
    if let Err(error) = preparation {
        TASK.with(State::reclaim)?;
        return Err(error);
    }
    let switching = TASK.with(|state| -> Result<(), UserError> {
        let root = state
            .tables
            .as_ref()
            .ok_or(UserError::InvalidMapping)?
            .root_address();
        // All kernel mappings are identical. Both old/new stores remain owned.
        unsafe { cpu::switch_root(root) }?;
        if let Err(error) = verify_user_hardware(state) {
            // Never return to a caller that might reclaim while this root lives.
            unsafe { cpu::switch_root(kernel_root) }.expect("restore validated kernel root");
            return Err(error);
        }
        state.running = true;
        Ok(())
    });
    if let Err(error) = switching {
        TASK.with(State::reclaim)?;
        return Err(error);
    }
    // SAFETY: mappings/entry/SP were validated and the root owns private pages.
    // No borrow of TASK is live here. The handler records exit/fault/timeout.
    unsafe {
        user::run(space::USER_CODE, space::USER_STACK_TOP, 0x20_0000);
    }
    let (outcome, irq_count, valid_calls) = TASK.with(|state| -> Result<_, UserError> {
        // The runner has restored EL1's stack/FP state. Switch and flush BEFORE
        // releasing any page, even after a terminating synchronous exception.
        unsafe { cpu::switch_root(kernel_root) }.expect("restore validated kernel root");
        let result = (
            state.outcome,
            state.irq_count,
            state.writes != 0
                && state.rejected_pointers != 0
                && state.unknown_syscalls != 0
                && state.yields != 0,
        );
        state.reclaim()?;
        Ok(result)
    })?;
    let passed = match (case, outcome) {
        (Case::Hello, Outcome::Exit(0)) => valid_calls,
        (
            Case::KernelFault,
            Outcome::Fault {
                esr,
                address: 0x20_0000,
                ..
            },
        ) => (esr >> 26) & 0x3F == 0x24,
        (Case::ReadOnlyFault, Outcome::Fault { esr, address, .. }) => {
            (esr >> 26) & 0x3F == 0x24 && address == space::USER_CODE
        }
        (Case::GuardFault, Outcome::Fault { esr, address, .. }) => {
            (esr >> 26) & 0x3F == 0x24 && address == space::USER_STACK_BASE - 8
        }
        (Case::Timeout, Outcome::Timeout) => irq_count >= MAX_TASK_TICKS,
        _ => false,
    };
    if let Outcome::Fault {
        esr,
        pc,
        address,
        stack,
    } = outcome
    {
        crate::println!("EL0 FAULT: ESR={esr:#018x}, ELR={pc:#018x}");
        crate::println!("           FAR={address:#018x}, SP={stack:#018x}");
    } else {
        crate::println!("EL0 RESULT: {outcome:?}; IRQ COUNT: {irq_count}");
    }
    if !passed {
        return Err(UserError::UnexpectedOutcome);
    }
    crate::println!("[ OK ] EL0 {}", case.name());
    Ok(())
}

fn prepare(state: &mut State, image: &[u8]) -> Result<(), UserError> {
    if state.running || state.tables.is_some() || state.code.is_some() {
        return Err(UserError::Busy);
    }
    if image.is_empty() || image.len() > PAGE_SIZE {
        return Err(UserError::InvalidImage);
    }
    state.outcome = Outcome::Pending;
    state.irq_count = 0;
    state.writes = 0;
    state.rejected_pointers = 0;
    state.unknown_syscalls = 0;
    state.yields = 0;
    state.code = Some(crate::memory::allocate_zeroed_pages(1, 1)?);
    state.data = Some(crate::memory::allocate_zeroed_pages(1, 1)?);
    state.stack = Some(crate::memory::allocate_zeroed_pages(
        space::USER_STACK_PAGES as usize,
        1,
    )?);
    let code = state.code.as_ref().unwrap().start_address();
    for (offset, &byte) in image.iter().enumerate() {
        // SAFETY: this token exclusively owns the initialized physical page.
        unsafe {
            ptr::write_volatile((code as *mut u8).add(offset), byte);
        }
    }
    let mut tables = mmu::new_task_root(&mut state.memory).map_err(UserError::Memory)?;
    tables.map_user_range(
        &mut state.memory,
        space::USER_CODE,
        code as u64,
        PAGE_SIZE as u64,
        false,
        true,
    )?;
    tables.map_user_range(
        &mut state.memory,
        space::USER_DATA,
        state.data.as_ref().unwrap().start_address() as u64,
        PAGE_SIZE as u64,
        true,
        false,
    )?;
    tables.map_user_range(
        &mut state.memory,
        space::USER_STACK_BASE,
        state.stack.as_ref().unwrap().start_address() as u64,
        space::USER_STACK_PAGES * PAGE_SIZE as u64,
        true,
        false,
    )?;
    space::validate_user_range(
        &tables,
        &state.memory,
        space::USER_CODE,
        image.len(),
        UserAccess::Execute,
    )
    .map_err(|_| UserError::InvalidMapping)?;
    space::validate_user_range(
        &tables,
        &state.memory,
        space::USER_STACK_BASE,
        space::USER_STACK_PAGES as usize * PAGE_SIZE,
        UserAccess::Write,
    )
    .map_err(|_| UserError::InvalidMapping)?;
    for address in [
        0,
        0x20_0000,
        0xFE21_5040,
        space::USER_STACK_GUARD,
        space::USER_STACK_TOP,
    ] {
        if space::validate_user_range(&tables, &state.memory, address, 1, UserAccess::Read).is_ok()
        {
            return Err(UserError::InvalidMapping);
        }
    }
    state.tables = Some(tables);
    cpu::publish_user_code();
    Ok(())
}

fn verify_user_hardware(state: &State) -> Result<(), UserError> {
    let read = cpu::probe_user(space::USER_CODE, false);
    let physical = (read & 0x0000_00FF_FFFF_F000) | (space::USER_CODE & 0xFFF);
    if read & 1 != 0 || physical != state.code.as_ref().unwrap().start_address() as u64 {
        return Err(UserError::ProbeFailed(read));
    }
    for address in [space::USER_DATA, space::USER_STACK_TOP - 8] {
        let par = cpu::probe_user(address, true);
        if par & 1 != 0 {
            return Err(UserError::ProbeFailed(par));
        }
    }
    for (address, write) in [
        (space::USER_CODE, true),
        (0x20_0000, false),
        (0xFE21_5040, false),
        (space::USER_STACK_GUARD, false),
        (space::USER_STACK_TOP, false),
    ] {
        let par = cpu::probe_user(address, write);
        if par & 1 == 0 {
            return Err(UserError::ProbeFailed(par));
        }
    }
    Ok(())
}

fn syscall_write(state: &mut State, address: u64, length: usize) -> Result<u64, Errno> {
    if length == 0 {
        return Ok(0);
    }
    let tables = state.tables.as_ref().ok_or(Errno::Fault)?;
    space::validate_user_range(tables, &state.memory, address, length, UserAccess::Read)
        .map_err(|_| Errno::Fault)?;
    // Validate ownership for the ENTIRE buffer before emitting any output.
    let mut offset = 0;
    while offset < length {
        let chunk = space::user_chunk(
            tables,
            &state.memory,
            address + offset as u64,
            length - offset,
            UserAccess::Read,
        )
        .map_err(|_| Errno::Fault)?;
        if !state.owns_chunk(chunk.physical_address, chunk.length) {
            return Err(Errno::Fault);
        }
        offset += chunk.length;
    }
    offset = 0;
    let mut buffer = [0u8; 128];
    while offset < length {
        let chunk = space::user_chunk(
            tables,
            &state.memory,
            address + offset as u64,
            (length - offset).min(buffer.len()),
            UserAccess::Read,
        )
        .map_err(|_| Errno::Fault)?;
        for (index, byte) in buffer[..chunk.length].iter_mut().enumerate() {
            // SAFETY: validated Normal NC user-owned bytes, accessed through
            // the existing privileged physical identity alias, not an EL0 VA.
            *byte = unsafe { ptr::read_volatile((chunk.physical_address as *const u8).add(index)) };
        }
        crate::console::write_bytes(&buffer[..chunk.length]);
        offset += chunk.length;
    }
    state.writes += 1;
    Ok(length as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_user_sync_exception(frame: &mut UserExceptionFrame) -> u64 {
    TASK.with(|state| {
        if !state.running || !abi::frame_from_el0(frame.context.spsr_el1) {
            panic!("unexpected lower-EL exception origin");
        }
        let esr = frame.context.esr_el1;
        if (esr >> 26) & 0x3F != 0x15 {
            state.outcome = Outcome::Fault {
                esr,
                pc: frame.context.elr_el1,
                address: frame.far_el1,
                stack: frame.sp_el0,
            };
            return 1;
        }
        // SVC ELR already names the next instruction; never add 4 here.
        if esr & 0xFFFF != 0 {
            frame.context.registers[0] = Errno::InvalidArgument.result();
            return 0;
        }
        let registers = &mut frame.context.registers;
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
        match request {
            Ok(SyscallRequest::Write {
                address, length, ..
            }) => {
                registers[0] = match syscall_write(state, address, length) {
                    Ok(result) => result,
                    Err(error) => {
                        state.rejected_pointers += 1;
                        error.result()
                    }
                };
            }
            Ok(SyscallRequest::Exit { status }) => {
                state.outcome = Outcome::Exit(status);
                return 1;
            }
            Ok(SyscallRequest::Yield) => {
                state.yields += 1;
                registers[0] = 0;
            }
            Err(error) => {
                if error == Errno::NotImplemented {
                    state.unknown_syscalls += 1;
                }
                registers[0] = error.result();
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_user_irq_exception(frame: &mut UserExceptionFrame) -> u64 {
    let previous_tick = exception::timer_ticks();
    exception::rust_irq_exception(&mut frame.context);
    let timer_events = exception::timer_ticks().wrapping_sub(previous_tick);
    TASK.with(|state| {
        if !state.running || !abi::frame_from_el0(frame.context.spsr_el1) {
            panic!("unexpected lower-EL IRQ origin");
        }
        // Count actual timer IRQs taken from EL0. A pending tick may have run at
        // EL1 between root publication and ERET, and must not shorten this test.
        state.irq_count += timer_events;
        if state.irq_count >= MAX_TASK_TICKS {
            state.outcome = Outcome::Timeout;
            return 1;
        }
        0
    })
}
