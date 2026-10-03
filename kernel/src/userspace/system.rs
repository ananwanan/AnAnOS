//! CPU0 process execution, offline ELF roots and syscall/VFS integration.
//! No mutable session borrow spans ERET. Only the runner installs roots; SVC
//! stages exec in a spare slot, then switches away before releasing old pages.
use super::{
    abi::{self, Errno},
    copy::{self, Arguments, UserMemory},
    elf,
    files::{FileServices, Terminal, fs_errno},
    process::{MAX_PROCESSES, ProcessError, ProcessTable},
    space::{self, UserAccess},
    syscall::{self, Action},
};
use crate::{
    arch::{
        context::{ExceptionContext, ExceptionFrame},
        interrupt, mmu as cpu, timer, user,
    },
    drivers::uart::MiniUart,
    fs::{FileSystem, ProcessFs},
    memory::{
        self,
        mmu::{self, PhysicalTableMemory},
        page::PhysicalPages,
        paging::PageTables,
    },
};
use core::{cell::UnsafeCell, ptr};

const STAGING: usize = MAX_PROCESSES;
use super::syscall::Services as _;
const PAGE: usize = 4096;
const MAX_TICKS: u64 = 30;
const INITRAMFS: &[u8] = include_bytes!("../../../userspace/images/initramfs.tar");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    None,
    Yield,
    Wait,
    Exec,
    Exit(i32),
    Fault {
        esr: u64,
        pc: u64,
        address: u64,
        stack: u64,
    },
}
const EMPTY_FRAME: ExceptionFrame = ExceptionFrame {
    context: ExceptionContext {
        registers: [0; 31],
        elr_el1: 0,
        spsr_el1: 0x340,
        esr_el1: 0,
    },
    simd: [[0; 2]; 32],
    fpcr: 0,
    fpsr: 0,
    sp_el0: 0,
    far_el1: 0,
};
struct Task {
    memory: PhysicalTableMemory,
    tables: Option<PageTables>,
    segments: [Option<PhysicalPages>; elf::MAX_LOAD_SEGMENTS],
    stack: Option<PhysicalPages>,
    frame: ExceptionFrame,
    ticks: u64,
    entered: bool,
    event: Event,
}
impl Task {
    const fn new() -> Self {
        Self {
            memory: PhysicalTableMemory::new(),
            tables: None,
            segments: [const { None }; elf::MAX_LOAD_SEGMENTS],
            stack: None,
            frame: EMPTY_FRAME,
            ticks: 0,
            entered: false,
            event: Event::None,
        }
    }
    fn owns(&self, address: u64, length: usize) -> bool {
        let Some(end) = address.checked_add(length as u64) else {
            return false;
        };
        self.segments
            .iter()
            .chain(core::iter::once(&self.stack))
            .flatten()
            .any(|pages| {
                let start = pages.start_address() as u64;
                address >= start && end <= start + pages.byte_len() as u64
            })
    }
    /// Requires an inactive root, and full TLBI after its last installation.
    fn reclaim(&mut self) {
        self.tables = None;
        self.memory.release_all();
        for slot in self
            .segments
            .iter_mut()
            .chain(core::iter::once(&mut self.stack))
        {
            if let Some(pages) = slot.take() {
                memory::free_pages(pages).expect("owned user pages");
            }
        }
        self.event = Event::None;
    }
    fn prepare(&mut self, image: &[u8], argv: &[&[u8]], envp: &[&[u8]]) -> Result<(), Errno> {
        if self.tables.is_some() || self.stack.is_some() {
            return Err(Errno::Busy);
        }
        let plan = elf::ElfPlan::parse(image).map_err(|_| Errno::ExecFormat)?;
        let mut tables = mmu::new_task_root(&mut self.memory).map_err(|_| Errno::NoMemory)?;
        for (index, segment) in plan.segments().iter().enumerate() {
            let pages = memory::allocate_zeroed_pages(segment.page_count, 1)
                .map_err(|_| Errno::NoMemory)?;
            let physical = pages.start_address();
            self.segments[index] = Some(pages);
            for page in 0..segment.page_count {
                // SAFETY: this fresh inactive token owns the whole aligned page;
                // kernel RAM identity aliases are Normal NC and writable.
                let destination = unsafe { &mut *((physical + page * PAGE) as *mut [u8; PAGE]) };
                segment
                    .copy_page(image, page, destination)
                    .map_err(|_| Errno::ExecFormat)?;
            }
            tables
                .map_user_range(
                    &mut self.memory,
                    segment.page_start,
                    physical as u64,
                    (segment.page_count * PAGE) as u64,
                    segment.writable,
                    segment.executable,
                )
                .map_err(|_| Errno::NoMemory)?;
        }
        let stack = memory::allocate_zeroed_pages(space::USER_STACK_PAGES as usize, 1)
            .map_err(|_| Errno::NoMemory)?;
        let physical = stack.start_address();
        self.stack = Some(stack);
        // SAFETY: stack is privately owned, zeroed and not yet mapped/active.
        let buffer = unsafe {
            core::slice::from_raw_parts_mut(
                physical as *mut u8,
                space::USER_STACK_PAGES as usize * PAGE,
            )
        };
        let initial =
            elf::build_initial_stack(buffer, space::USER_STACK_BASE, plan.entry, argv, envp)
                .map_err(|_| Errno::InvalidArgument)?;
        tables
            .map_user_range(
                &mut self.memory,
                space::USER_STACK_BASE,
                physical as u64,
                space::USER_STACK_PAGES * PAGE as u64,
                true,
                false,
            )
            .map_err(|_| Errno::NoMemory)?;
        space::validate_owned_user_range(
            &tables,
            &self.memory,
            plan.entry,
            4,
            UserAccess::Execute,
            |address, length| self.owns(address, length),
        )
        .map_err(|_| Errno::ExecFormat)?;
        self.frame = EMPTY_FRAME;
        self.frame.context.elr_el1 = plan.entry;
        self.frame.sp_el0 = initial.stack_pointer;
        self.ticks = 0;
        self.entered = false;
        self.event = Event::None;
        self.tables = Some(tables);
        cpu::publish_user_code();
        Ok(())
    }
}
struct PhysicalMemory<'a>(&'a Task);
impl UserMemory for PhysicalMemory<'_> {
    fn validate(&self, address: u64, length: usize, access: UserAccess) -> Result<(), Errno> {
        let tables = self.0.tables.as_ref().ok_or(Errno::Fault)?;
        space::validate_owned_user_range(
            tables,
            &self.0.memory,
            address,
            length,
            access,
            |physical, count| self.0.owns(physical, count),
        )
        .map_err(|_| Errno::Fault)
    }
    fn read_validated(&self, address: u64, output: &mut [u8]) {
        transfer(
            self.0,
            address,
            output.len(),
            UserAccess::Read,
            |physical, offset, count| {
                for index in 0..count {
                    // SAFETY: whole range and private physical ownership validated
                    // under SESSION's IRQ guard before any side effect.
                    output[offset + index] =
                        unsafe { ptr::read_volatile((physical as *const u8).add(index)) };
                }
            },
        );
    }
    fn write_validated(&self, address: u64, input: &[u8]) {
        transfer(
            self.0,
            address,
            input.len(),
            UserAccess::Write,
            |physical, offset, count| {
                for index in 0..count {
                    unsafe {
                        ptr::write_volatile(
                            (physical as *mut u8).add(index),
                            input[offset + index],
                        );
                    }
                }
            },
        );
    }
}
fn transfer(
    task: &Task,
    address: u64,
    length: usize,
    access: UserAccess,
    mut action: impl FnMut(u64, usize, usize),
) {
    let mut offset = 0;
    while offset < length {
        let chunk = space::user_chunk(
            task.tables.as_ref().unwrap(),
            &task.memory,
            address + offset as u64,
            length - offset,
            access,
        )
        .expect("validated frozen user range");
        action(chunk.physical_address, offset, chunk.length);
        offset += chunk.length;
    }
}
struct Console;
impl Terminal for Console {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        let uart = MiniUart::new();
        let mut count = 0;
        while count < buffer.len() {
            let Some(byte) = uart.try_read_byte() else {
                break;
            };
            buffer[count] = byte;
            count += 1;
        }
        if count == 0 && !buffer.is_empty() {
            Err(Errno::Again)
        } else {
            Ok(count)
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        crate::console::write_bytes(bytes);
    }
}
#[derive(Clone, Copy)]
struct Wait {
    pid: Option<u32>,
    status: u64,
}
struct Session {
    fs: FileSystem,
    descriptors: [ProcessFs; MAX_PROCESSES],
    processes: ProcessTable,
    tasks: [Task; MAX_PROCESSES + 1],
    waiting: [Option<Wait>; MAX_PROCESSES],
    active: Option<usize>,
    initial_pid: u32,
    initial_status: Option<i32>,
    spawn_count: u64,
    exec_count: u64,
    wait_count: u64,
}
struct SessionCell(UnsafeCell<Session>);
// SAFETY: CPU0 only; every borrow masks IRQ, none spans a runner call or ERET.
unsafe impl Sync for SessionCell {}
impl SessionCell {
    fn with<R>(&self, function: impl FnOnce(&mut Session) -> R) -> R {
        interrupt::with_irq_masked(|| unsafe { function(&mut *self.0.get()) })
    }
}
static SESSION: SessionCell = SessionCell(UnsafeCell::new(Session {
    fs: FileSystem::new(),
    descriptors: [const { ProcessFs::new() }; MAX_PROCESSES],
    processes: ProcessTable::new(),
    tasks: [const { Task::new() }; MAX_PROCESSES + 1],
    waiting: [None; MAX_PROCESSES],
    active: None,
    initial_pid: 0,
    initial_status: None,
    spawn_count: 0,
    exec_count: 0,
    wait_count: 0,
}));
fn process_errno(error: ProcessError) -> Errno {
    match error {
        ProcessError::NoChild => Errno::NoChild,
        ProcessError::NoSlots | ProcessError::PidExhausted => Errno::Again,
        _ => Errno::InvalidArgument,
    }
}
fn inherit(
    fs: &mut FileSystem,
    descriptors: &mut [ProcessFs; MAX_PROCESSES],
    parent: usize,
    child: usize,
) -> Result<(), Errno> {
    let (low, high) = descriptors.split_at_mut(parent.max(child));
    if parent < child {
        fs.inherit_process(&low[parent], &mut high[0])
            .map_err(fs_errno)
    } else {
        fs.inherit_process(&high[0], &mut low[child])
            .map_err(fs_errno)
    }
}
fn launch(session: &mut Session, slot: usize, args: [u64; 6], exec: bool) -> Result<u64, Errno> {
    let mut path = [0; abi::MAX_PATH_BYTES];
    let mut arguments = Arguments::new();
    let user_memory = PhysicalMemory(&session.tasks[slot]);
    let length = copy::path_in(&user_memory, args[0], args[1], &mut path)?;
    arguments.from_user(&user_memory, args[2], args[3], args[4], args[5])?;
    // Reject missing/malformed files before reserving a PID or changing FDs.
    let image = session
        .fs
        .file_bytes(&session.descriptors[slot], &path[..length])
        .map_err(fs_errno)?;
    elf::ElfPlan::parse(image).map_err(|_| Errno::ExecFormat)?;
    let parent = session.processes.getpid(slot).map_err(process_errno)?;
    let (target, pid) = if exec {
        (STAGING, parent)
    } else {
        session
            .processes
            .reserve_spawn(Some(parent))
            .map_err(process_errno)?
    };
    let (argv, argc) = arguments.argv();
    let (envp, envc) = arguments.envp();
    let result = session.tasks[target].prepare(image, &argv[..argc], &envp[..envc]);
    if let Err(error) = result {
        session.tasks[target].reclaim();
        if !exec {
            session
                .processes
                .rollback_spawn(target)
                .expect("reserved child");
        }
        return Err(error);
    }
    if exec {
        session.tasks[slot].event = Event::Exec;
        session.exec_count += 1;
    } else {
        if let Err(error) = inherit(&mut session.fs, &mut session.descriptors, slot, target) {
            session.tasks[target].reclaim();
            session
                .processes
                .rollback_spawn(target)
                .expect("reserved child");
            return Err(error);
        }
        session
            .processes
            .commit_spawn(target)
            .expect("complete child");
        session.spawn_count += 1;
    }
    Ok(pid as u64)
}
fn wait_request(session: &mut Session, slot: usize, args: [u64; 6]) -> Result<u64, Errno> {
    let pid = if args[0] == u64::MAX {
        None
    } else {
        if args[0] == 0 || args[0] > u32::MAX as u64 {
            return Err(Errno::InvalidArgument);
        }
        Some(args[0] as u32)
    };
    if args[2] > 1 {
        return Err(Errno::InvalidArgument);
    }
    let parent = session.processes.getpid(slot).map_err(process_errno)?;
    let ready = session.processes.wait(parent, pid).map_err(process_errno)?;
    if args[1] != 0 {
        PhysicalMemory(&session.tasks[slot]).validate(args[1], 4, UserAccess::Write)?;
    }
    if let Some((child, status, child_slot)) = ready {
        if args[1] != 0 {
            copy::copy_out(
                &PhysicalMemory(&session.tasks[slot]),
                args[1],
                &status.to_le_bytes(),
            )?;
        }
        session
            .processes
            .commit_wait(parent, child_slot)
            .expect("completed child");
        session.wait_count += 1;
        Ok(child as u64)
    } else if args[2] == 1 {
        Ok(0)
    } else {
        session.waiting[slot] = Some(Wait {
            pid,
            status: args[1],
        });
        session
            .processes
            .arm_wait(parent, pid)
            .expect("live matching child");
        session.tasks[slot].event = Event::Wait;
        Ok(0)
    }
}
fn complete_wait(session: &mut Session, slot: usize) {
    let Some(wait) = session.waiting[slot].take() else {
        return;
    };
    let parent = session.processes.getpid(slot).expect("waiting parent");
    let result = match session.processes.wait(parent, wait.pid) {
        Ok(Some((pid, status, child))) => {
            let copied = if wait.status == 0 {
                Ok(())
            } else {
                copy::copy_out(
                    &PhysicalMemory(&session.tasks[slot]),
                    wait.status,
                    &status.to_le_bytes(),
                )
            };
            copied.map(|()| {
                session
                    .processes
                    .commit_wait(parent, child)
                    .expect("completed wait");
                session.wait_count += 1;
                pid as u64
            })
        }
        Ok(None) => Err(Errno::Again),
        Err(error) => Err(process_errno(error)),
    };
    session.tasks[slot].frame.context.registers[0] = result.unwrap_or_else(Errno::result);
}
struct Services<'a> {
    session: &'a mut Session,
    slot: usize,
}
impl syscall::Services for Services<'_> {
    fn validate_write(&self, fd: u64) -> Result<(), Errno> {
        let fd = usize::try_from(fd).map_err(|_| Errno::BadFileDescriptor)?;
        self.session
            .fs
            .validate_write(&self.session.descriptors[self.slot], fd)
            .map(|_| ())
            .map_err(fs_errno)
    }
    fn write(&mut self, fd: u64, address: u64, length: usize) -> Result<u64, Errno> {
        let mut console = Console;
        FileServices {
            fs: &mut self.session.fs,
            process: &mut self.session.descriptors[self.slot],
            memory: &PhysicalMemory(&self.session.tasks[self.slot]),
            terminal: &mut console,
        }
        .write(fd, address, length)
    }
    fn yield_now(&mut self) {
        self.session.tasks[self.slot].event = Event::Yield;
    }
    fn call(&mut self, number: u64, args: [u64; 6]) -> Result<u64, Errno> {
        match number {
            abi::SYS_GETPID => self
                .session
                .processes
                .getpid(self.slot)
                .map(u64::from)
                .map_err(process_errno),
            abi::SYS_SPAWN => launch(self.session, self.slot, args, false),
            abi::SYS_EXEC => launch(self.session, self.slot, args, true),
            abi::SYS_WAITPID => wait_request(self.session, self.slot, args),
            abi::SYS_CLOCK_GETTIME => {
                // Bootstrap CLOCK_MONOTONIC=1 only; no wall-clock source.
                if args[0] != 1 {
                    return Err(Errno::InvalidArgument);
                }
                let frequency = timer::Timer::new().frequency();
                if frequency == 0 {
                    return Err(Errno::Io);
                }
                let counter = timer::current_counter();
                let mut bytes = [0; 16];
                bytes[..8].copy_from_slice(&(counter / frequency).to_le_bytes());
                let nanos =
                    ((counter % frequency) as u128 * 1_000_000_000 / frequency as u128) as u64;
                bytes[8..].copy_from_slice(&nanos.to_le_bytes());
                copy::copy_out(
                    &PhysicalMemory(&self.session.tasks[self.slot]),
                    args[1],
                    &bytes,
                )?;
                Ok(0)
            }
            _ => {
                let mut console = Console;
                FileServices {
                    fs: &mut self.session.fs,
                    process: &mut self.session.descriptors[self.slot],
                    memory: &PhysicalMemory(&self.session.tasks[self.slot]),
                    terminal: &mut console,
                }
                .call(number, args)
            }
        }
    }
}

pub(super) fn handle_sync(frame: &mut ExceptionFrame) -> Option<u64> {
    SESSION.with(|session| {
        let slot = session.active?;
        assert!(
            abi::frame_from_el0(frame.context.spsr_el1),
            "invalid process exception origin"
        );
        if (frame.context.esr_el1 >> 26) & 0x3f != syscall::AARCH64_SVC_CLASS {
            session.tasks[slot].event = Event::Fault {
                esr: frame.context.esr_el1,
                pc: frame.context.elr_el1,
                address: frame.far_el1,
                stack: frame.sp_el0,
            };
            return Some(1);
        }
        let action = syscall::dispatch(&mut frame.context, &mut Services { session, slot })
            .expect("validated process SVC");
        if let Action::Exit(status) = action {
            session.tasks[slot].event = Event::Exit((status & 0xff) << 8);
        }
        let event = session.tasks[slot].event;
        if matches!(event, Event::Yield | Event::Wait) {
            session.tasks[slot].frame = *frame;
        }
        Some(u64::from(event != Event::None))
    })
}
pub(super) fn handle_irq(frame: &ExceptionFrame, events: u64) -> Option<u64> {
    SESSION.with(|session| {
        let slot = session.active?;
        assert!(
            abi::frame_from_el0(frame.context.spsr_el1),
            "invalid process IRQ origin"
        );
        if events == 0 {
            return Some(0);
        }
        let task = &mut session.tasks[slot];
        task.ticks = task.ticks.saturating_add(events);
        if task.ticks >= MAX_TICKS {
            task.event = Event::Exit(9);
        } else {
            task.frame = *frame;
            task.event = Event::Yield;
        }
        Some(1)
    })
}

fn verify(task: &Task) -> Result<(), Errno> {
    let tables = task.tables.as_ref().ok_or(Errno::Fault)?;
    if !abi::frame_from_el0(task.frame.context.spsr_el1) {
        return Err(Errno::Fault);
    }
    if !task.entered {
        for (address, write) in [
            (task.frame.context.elr_el1, false),
            (task.frame.sp_el0, true),
        ] {
            let par = cpu::probe_user(address, write);
            let translated = tables.lookup(&task.memory, address).ok_or(Errno::Fault)?;
            if par & 1 != 0
                || ((par & 0x0000_00FF_FFFF_F000) | (address & 0xfff))
                    != translated.physical_address
            {
                return Err(Errno::Fault);
            }
        }
    }
    // Saved PC/SP may be user-controlled and unmapped after a yield/IRQ. They
    // are never dereferenced by EL1: resume lets the resulting EL0 access fault
    // terminate just that process instead of aborting all other processes.
    for (address, write, allowed) in [
        (0x20_0000, false, false),
        (0xFE21_5040, false, false),
        (space::USER_STACK_GUARD, false, false),
        (space::USER_STACK_TOP, false, false),
    ] {
        let par = cpu::probe_user(address, write);
        if (par & 1 == 0) != allowed {
            return Err(Errno::Fault);
        }
        if allowed {
            let translated = tables.lookup(&task.memory, address).ok_or(Errno::Fault)?;
            if ((par & 0x0000_00FF_FFFF_F000) | (address & 0xfff)) != translated.physical_address {
                return Err(Errno::Fault);
            }
        }
    }
    Ok(())
}
fn cleanup(session: &mut Session) {
    session.active = None;
    for index in 0..MAX_PROCESSES {
        session.tasks[index].reclaim();
        session.fs.cleanup_process(&mut session.descriptors[index]);
    }
    session.tasks[STAGING].reclaim();
    session.processes.clear_after_reclaim();
    session.waiting.fill(None);
}
pub fn run_filesystem_demo() -> Result<(), Errno> {
    let kernel_root = mmu::kernel_root().map_err(|_| Errno::Io)?;
    let pages_before = memory::page_stats();
    let heap_before = memory::heap_free_bytes();
    crate::println!("[INFO] M4 STATIC ELF / PROCESS / FILESYSTEM");
    let result = run_session(kernel_root);
    // This also covers failed initial/spawn/exec construction and probe failure.
    SESSION.with(|session| {
        unsafe { cpu::switch_root(kernel_root) }.expect("validated kernel root");
        cleanup(session);
    });
    result?;
    if memory::page_stats() != pages_before || memory::heap_free_bytes() != heap_before {
        return Err(Errno::NoMemory);
    }
    crate::println!("[ OK ] M4 ADDRESS-SPACE/FD/PAGE RECLAIM");
    Ok(())
}
fn run_session(kernel_root: u64) -> Result<(), Errno> {
    SESSION.with(|session| -> Result<(), Errno> {
        if session.initial_pid != 0 {
            return Err(Errno::Busy);
        }
        session.fs.initialize().map_err(fs_errno)?;
        session
            .fs
            .mount_tar(INITRAMFS)
            .map_err(|_| Errno::ExecFormat)?;
        let (slot, pid) = session
            .processes
            .reserve_spawn(None)
            .map_err(process_errno)?;
        session
            .fs
            .attach_process(&mut session.descriptors[slot])
            .map_err(fs_errno)?;
        let image = session
            .fs
            .file_bytes(&session.descriptors[slot], b"/init/bin/init")
            .map_err(fs_errno)?;
        session.tasks[slot].prepare(image, &[b"init"], &[b"PATH=/init/bin"])?;
        session
            .processes
            .commit_spawn(slot)
            .map_err(process_errno)?;
        session.initial_pid = pid;
        Ok(())
    })?;
    loop {
        // Capture before SESSION.with temporarily masks IRQs; the temporary
        // critical-section DAIF must never become a process's saved PSTATE.
        let caller_irq = crate::arch::exception::daif() & 0x80;
        let selection = SESSION.with(
            |session| -> Result<Option<(usize, *const ExceptionFrame)>, Errno> {
                while let Some(slot) = session.processes.next_orphan_zombie() {
                    session.processes.reap_orphan(slot).map_err(process_errno)?;
                }
                let Some(slot) = session.processes.next_ready() else {
                    return Ok(None);
                };
                complete_wait(session, slot);
                let task = &mut session.tasks[slot];
                task.event = Event::None;
                // Initial and saved frames always retain the kernel's IRQ policy.
                task.frame.context.spsr_el1 = (task.frame.context.spsr_el1 & !0x80) | caller_irq;
                let root = task.tables.as_ref().ok_or(Errno::Fault)?.root_address();
                unsafe { cpu::switch_root(root) }.map_err(|_| Errno::Fault)?;
                if let Err(error) = verify(task) {
                    unsafe { cpu::switch_root(kernel_root) }.expect("restore kernel root");
                    return Err(error);
                }
                task.entered = true;
                session
                    .processes
                    .mark_running(slot)
                    .map_err(process_errno)?;
                session.active = Some(slot);
                Ok(Some((slot, ptr::addr_of!(task.frame))))
            },
        )?;
        let Some((slot, frame)) = selection else {
            break;
        };
        // SAFETY: root/probes validated; frame lives in static owned storage.
        // SESSION has no live reference/borrow across this runner call.
        unsafe {
            user::resume(frame);
        }
        let (pid, event) = SESSION.with(|session| -> Result<_, Errno> {
            unsafe { cpu::switch_root(kernel_root) }.expect("restore kernel root before reclaim");
            session.active = None;
            let pid = session.processes.getpid(slot).map_err(process_errno)?;
            let event = session.tasks[slot].event;
            match event {
                Event::Yield => session.processes.yield_ready(slot).map_err(process_errno)?,
                Event::Wait => {}
                Event::Exec => {
                    session.tasks[slot].reclaim();
                    session.tasks.swap(slot, STAGING);
                    session.processes.yield_ready(slot).map_err(process_errno)?;
                }
                Event::Exit(_) | Event::Fault { .. } => {
                    let status = if let Event::Exit(status) = event {
                        status
                    } else {
                        11
                    };
                    session.tasks[slot].reclaim();
                    session.fs.cleanup_process(&mut session.descriptors[slot]);
                    session.waiting[slot] = None;
                    session
                        .processes
                        .exit(slot, status)
                        .map_err(process_errno)?;
                    if pid == session.initial_pid {
                        session.initial_status = Some(status);
                    }
                }
                Event::None => return Err(Errno::Io),
            }
            Ok((pid, event))
        })?;
        if !matches!(event, Event::Yield | Event::Wait) {
            crate::println!("M4 PROCESS: pid={pid}, {event:?}");
        }
    }
    SESSION.with(|session| {
        crate::println!(
            "M4 PROCESS: spawn={}, exec={}, wait={}, init={:?}",
            session.spawn_count,
            session.exec_count,
            session.wait_count,
            session.initial_status
        );
        if session.initial_status != Some(0)
            || session.spawn_count != 1
            || session.exec_count != 1
            || session.wait_count != 1
            || (0..MAX_PROCESSES).any(|i| session.processes.process(i).is_some())
            || session.fs.has_process_resources()
        {
            return Err(Errno::Io);
        }
        Ok(())
    })
}
