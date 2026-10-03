//! CPU0 EL0 bring-up tasks. No scheduler, ELF loader or hosted runtime yet.
//!
//! Every task has private pages and an offline-built root. No mutable state
//! borrow spans `eret`; exceptions borrow the state only after EL0 has stopped.

use core::cell::UnsafeCell;
use core::ptr;

use super::abi::{self, Errno};
use super::fault::{AbortKind, DataAbort, ExpectedFault};
use super::preflight::{self, PreflightError, TimerSource};
use super::space::{self, UserAccess};
use super::syscall::{self, Action};
use crate::arch::context::ExceptionFrame as UserExceptionFrame;
use crate::arch::{exception, interrupt, mmu as cpu, timer, user};
use crate::drivers::gic::Gic;
use crate::memory::mmu::{self, PhysicalTableMemory};
use crate::memory::page::{PageError, PhysicalPages};
use crate::memory::paging::{PageTables, PagingError};

const PAGE_SIZE: usize = 4096;
const MAX_TASK_TICKS: u64 = 3;

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
    IrqPreflight(PreflightError),
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
    svc_calls: u64,
    bad_descriptors: u64,
    invalid_requests: u64,
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
            svc_calls: 0,
            bad_descriptors: 0,
            invalid_requests: 0,
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

    fn expected_fault(self, kernel_text_level: u8) -> Option<ExpectedFault> {
        let (kind, level, write, address, pc) = match self {
            Self::KernelFault => (
                AbortKind::Permission,
                kernel_text_level,
                false,
                0x20_0000,
                user::kernel_fault_pc(),
            ),
            Self::ReadOnlyFault => (
                AbortKind::Permission,
                3,
                true,
                space::USER_CODE,
                user::readonly_fault_pc(),
            ),
            Self::GuardFault => (
                AbortKind::Translation,
                3,
                true,
                space::USER_STACK_BASE - 8,
                user::guard_fault_pc(),
            ),
            _ => return None,
        };
        Some(ExpectedFault {
            kind,
            level,
            write,
            address,
            pc,
            stack: space::USER_STACK_TOP,
        })
    }
}

struct KernelTimer;
impl TimerSource for KernelTimer {
    fn frequency(&mut self) -> u64 {
        timer::Timer::new().frequency()
    }
    fn counter(&mut self) -> u64 {
        timer::current_counter()
    }
    fn timer_ticks(&mut self) -> u64 {
        exception::timer_ticks()
    }
    fn irq_masked(&mut self) -> bool {
        exception::irq_is_masked()
    }
}

fn verify_timer_irq() -> Result<(), UserError> {
    crate::println!("[INFO] EL0 IRQ PREFLIGHT: WAITING FOR TWO EL1 TIMER EVENTS");
    match preflight::wait_for_timer_irq(&mut KernelTimer) {
        Ok(report) => {
            crate::println!(
                "[ OK ] EL0 IRQ PREFLIGHT: events={}, counter ticks={}",
                report.timer_events,
                report.elapsed_counter_ticks
            );
            Ok(())
        }
        Err(error) => {
            print_irq_preflight_failure(error);
            Err(UserError::IrqPreflight(error))
        }
    }
}

fn print_irq_preflight_failure(error: PreflightError) {
    struct Snapshot {
        daif: u64,
        frequency: u64,
        counter: u64,
        deadline: u64,
        control: u64,
        ticks: u64,
        distributor: u32,
        interface: u32,
        priority_mask: u32,
        enabled: u32,
        pending: u32,
        groups: u32,
        highest: u32,
        alternate_highest: u32,
        running_priority: u32,
    }
    // Capture the original mask, not the temporary mask used for the snapshot.
    let original_daif = exception::daif();
    let snapshot = interrupt::with_irq_masked(|| {
        let gic = Gic::new();
        Snapshot {
            daif: original_daif,
            frequency: timer::Timer::new().frequency(),
            counter: timer::current_counter(),
            deadline: timer::compare_value(),
            control: timer::control(),
            ticks: exception::timer_ticks(),
            distributor: gic.distributor_control(),
            interface: gic.cpu_interface_control(),
            priority_mask: gic.priority_mask(),
            enabled: gic.enabled_private_interrupts(),
            pending: gic.pending_private_interrupts(),
            groups: gic.private_interrupt_groups(),
            highest: gic.highest_pending_interrupt(),
            alternate_highest: gic.group1_highest_pending_interrupt(),
            running_priority: gic.running_priority(),
        }
    });
    // No IAR read: diagnostics must not acknowledge or alter pending IRQs.
    crate::println!("[FAIL] EL0 IRQ PREFLIGHT: {error:?}; USER TASKS NOT ENTERED");
    crate::println!("DAIF          : {:#018x}", snapshot.daif);
    crate::println!("CNTFRQ        : {}", snapshot.frequency);
    crate::println!("CNTPCT        : {:#018x}", snapshot.counter);
    crate::println!("CNTP_CVAL     : {:#018x}", snapshot.deadline);
    crate::println!("TIMER CONTROL : {:#010x}", snapshot.control);
    crate::println!("TIMER PENDING : {}", snapshot.control & (1 << 2) != 0);
    crate::println!("TIMER TICKS   : {}", snapshot.ticks);
    crate::println!("GICD_CTLR     : {:#010x}", snapshot.distributor);
    crate::println!("GICC_CTLR     : {:#010x}", snapshot.interface);
    crate::println!("GICC_PMR      : {:#010x}", snapshot.priority_mask);
    crate::println!("ISENABLER0    : {:#010x}", snapshot.enabled);
    crate::println!("ISPENDR0      : {:#010x}", snapshot.pending);
    crate::println!("IGROUPR0 (NS) : {:#010x}", snapshot.groups);
    crate::println!("HPPIR         : {}", snapshot.highest);
    crate::println!("AHPPIR        : {}", snapshot.alternate_highest);
    crate::println!("GICC_RPR      : {:#010x}", snapshot.running_priority);
}

pub fn run_demos() -> Result<(), UserError> {
    if !cpu::is_enabled() {
        return Err(UserError::InvalidMapping);
    }
    // No task-cell borrow or allocated user pages while IRQ is being observed.
    // This proves EL1 timer/rearm delivery, not the later lower-EL vector path.
    verify_timer_irq()?;
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
    let preparation = TASK.with(|state| -> Result<_, UserError> {
        prepare(state, case.image())?;
        // Kernel text can use a page or block as the image grows. Derive its
        // expected permission-fault level from the actual frozen task mapping.
        let kernel_text_level = state
            .tables
            .as_ref()
            .and_then(|tables| tables.lookup(&state.memory, 0x20_0000))
            .ok_or(UserError::InvalidMapping)?
            .level;
        Ok(case.expected_fault(kernel_text_level))
    });
    let expected_fault = match preparation {
        Ok(expected) => expected,
        Err(error) => {
            // A Busy result belongs to an existing runner. Never free its live
            // root/pages as if they were this invocation's partial construction.
            if !matches!(error, UserError::Busy) {
                TASK.with(State::reclaim)?;
            }
            return Err(error);
        }
    };
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
    let (outcome, irq_count, calls) = TASK.with(|state| -> Result<_, UserError> {
        // The runner has restored EL1's stack/FP state. Switch and flush BEFORE
        // releasing any page, even after a terminating synchronous exception.
        unsafe { cpu::switch_root(kernel_root) }.expect("restore validated kernel root");
        let result = (
            state.outcome,
            state.irq_count,
            [
                state.svc_calls,
                state.writes,
                state.rejected_pointers,
                state.bad_descriptors,
                state.invalid_requests,
                state.unknown_syscalls,
                state.yields,
            ],
        );
        state.reclaim()?;
        Ok(result)
    })?;
    let passed = match (case, outcome) {
        (Case::Hello, Outcome::Exit(0)) => calls == [12, 3, 3, 1, 2, 1, 1],
        (
            _,
            Outcome::Fault {
                esr,
                pc,
                address,
                stack,
            },
        ) => expected_fault.is_some_and(|expected| expected.matches(esr, pc, address, stack)),
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
        if let Some(abort) = DataAbort::decode(esr) {
            crate::println!(
                "EL0 ABORT: kind={:?}, level={:?}, DFSC={:#04x}, write={}",
                abort.kind,
                abort.level,
                abort.dfsc,
                abort.write
            );
            crate::println!(
                "           IL={}, FAR valid={}, S1PTW={}, CM={}, EA={}",
                abort.instruction_32bit,
                abort.far_valid,
                abort.stage1_walk,
                abort.cache_maintenance,
                abort.external_abort_type
            );
        }
        if let Some(expected) = expected_fault {
            crate::println!(
                "EL0 EXPECT: kind={:?}, level={}, write={}, match={passed}",
                expected.kind,
                expected.level,
                expected.write
            );
            if !passed {
                crate::println!(
                    "            ELR={:#018x}, FAR={:#018x}, SP={:#018x}",
                    expected.pc,
                    expected.address,
                    expected.stack
                );
            }
        }
    } else {
        crate::println!("EL0 RESULT: {outcome:?}; IRQ COUNT: {irq_count}");
    }
    if matches!(case, Case::Hello) {
        // Print only after leaving the exception and restoring the kernel root.
        crate::println!(
            "EL0 SVC: calls={}, writes={}, EFAULT={}, EBADF={}, EINVAL={}, ENOSYS={}, yield={}",
            calls[0],
            calls[1],
            calls[2],
            calls[3],
            calls[4],
            calls[5],
            calls[6]
        );
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
    state.svc_calls = 0;
    state.bad_descriptors = 0;
    state.invalid_requests = 0;
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
    let tables = state.tables.as_ref().ok_or(Errno::Fault)?;
    let result = syscall::write_user_buffer(
        tables,
        &state.memory,
        address,
        length,
        |physical, count| state.owns_chunk(physical, count),
        |chunk, buffer| {
            for (index, byte) in buffer.iter_mut().enumerate() {
                // SAFETY: the shared copy path validated the entire range and
                // ownership. TASK's IRQ guard keeps pages/mappings stable;
                // read only the privileged Normal NC physical identity alias.
                *byte =
                    unsafe { ptr::read_volatile((chunk.physical_address as *const u8).add(index)) };
            }
        },
        crate::console::write_bytes,
    );
    if result.is_ok() {
        state.writes += 1;
    }
    result
}

impl syscall::Services for State {
    fn write(&mut self, _fd: u64, address: u64, length: usize) -> Result<u64, Errno> {
        syscall_write(self, address, length)
    }

    fn yield_now(&mut self) {
        // There is one synchronous task. This remains a hint until scheduling
        // exists; it must return to the current EL0 context without losing it.
        self.yields += 1;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_user_sync_exception(frame: &mut UserExceptionFrame) -> u64 {
    #[cfg(feature = "filesystem")]
    if let Some(action) = super::system::handle_sync(frame) {
        return action;
    }
    TASK.with(|state| {
        if !state.running || !abi::frame_from_el0(frame.context.spsr_el1) {
            panic!("unexpected lower-EL exception origin");
        }
        let esr = frame.context.esr_el1;
        if (esr >> 26) & 0x3F != syscall::AARCH64_SVC_CLASS {
            state.outcome = Outcome::Fault {
                esr,
                pc: frame.context.elr_el1,
                address: frame.far_el1,
                stack: frame.sp_el0,
            };
            return 1;
        }
        state.svc_calls += 1;
        match syscall::dispatch(&mut frame.context, state).expect("validated EL0 SVC origin") {
            Action::Exit(status) => {
                state.outcome = Outcome::Exit(status);
                // Vector action 1 discards its frame and resumes the EL1h
                // runner. It restores the kernel root before page reclamation.
                return 1;
            }
            Action::Resume => {
                let result = frame.context.registers[0];
                if result == Errno::Fault.result() {
                    state.rejected_pointers += 1;
                } else if result == Errno::BadFileDescriptor.result() {
                    state.bad_descriptors += 1;
                } else if result == Errno::InvalidArgument.result() {
                    state.invalid_requests += 1;
                } else if result == Errno::NotImplemented.result() {
                    state.unknown_syscalls += 1;
                }
            }
        }
        // Vector action 0 restores every saved register and ERETs to EL0.
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_user_irq_exception(frame: &mut UserExceptionFrame) -> u64 {
    let previous_tick = exception::timer_ticks();
    exception::rust_irq_exception(&mut frame.context);
    let timer_events = exception::timer_ticks().wrapping_sub(previous_tick);
    #[cfg(feature = "filesystem")]
    if let Some(action) = super::system::handle_irq(frame, timer_events) {
        return action;
    }
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
