//! Owned physical translation tables and the opt-in M2 boot path.
//!
//! Construction is offline. On any pre-enable failure every table page is
//! returned; after activation, tables are frozen and retained for kernel life.

use core::ptr;

use super::fdt::MemoryLayout;
use super::mapping::{KernelSections, MappingError, MappingPlan};
use super::page::{PAGE_SIZE, PhysicalPages};
use super::paging::{MappingAttrs, PageTables, PagingError, TableMemory};
use super::regions::{Region, RegionSet};
use super::runtime::Cpu0Cell;
use crate::arch::mmu::{self as cpu, MmuError};
use crate::drivers::mailbox::{Mailbox, MailboxError};

const TABLE_CAPACITY: usize = 128;

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
    static __exception_vectors: u8;
}

#[derive(Debug)]
pub enum InitError {
    MemoryNotReady,
    AlreadyPrepared,
    Mapping(MappingError),
    Paging(PagingError),
    Cpu(MmuError),
    SoftwareWalk,
    HardwareProbe,
    MemorySelfTest,
    Firmware(MailboxError),
}

impl From<MappingError> for InitError {
    fn from(value: MappingError) -> Self {
        Self::Mapping(value)
    }
}
impl From<PagingError> for InitError {
    fn from(value: PagingError) -> Self {
        Self::Paging(value)
    }
}
impl From<MmuError> for InitError {
    fn from(value: MmuError) -> Self {
        Self::Cpu(value)
    }
}

pub(crate) struct PhysicalTableMemory {
    pages: [Option<PhysicalPages>; TABLE_CAPACITY],
    count: usize,
}

impl PhysicalTableMemory {
    pub(crate) const fn new() -> Self {
        Self {
            pages: [const { None }; TABLE_CAPACITY],
            count: 0,
        }
    }

    fn owns(&self, address: u64) -> bool {
        self.pages
            .iter()
            .flatten()
            .any(|pages| pages.start_address() as u64 == address)
    }

    /// Caller must first switch away and complete TLB invalidation.
    pub(crate) fn release_all(&mut self) {
        for slot in &mut self.pages {
            if let Some(pages) = slot.take() {
                // Tokens originate from the same stable global allocator. Never
                // invoke this after TTBR0 references the store.
                super::free_pages(pages).expect("owned translation page release");
            }
        }
        self.count = 0;
    }
}

impl TableMemory for PhysicalTableMemory {
    fn allocate_table(&mut self) -> Result<u64, PagingError> {
        let slot = self
            .pages
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(PagingError::OutOfMemory)?;
        let pages = super::allocate_zeroed_pages(1, 1).map_err(|_| PagingError::OutOfMemory)?;
        let address = pages.start_address() as u64;
        *slot = Some(pages);
        self.count += 1;
        Ok(address)
    }

    fn read_entry(&self, table: u64, index: usize) -> u64 {
        assert!(
            index < 512 && self.owns(table),
            "invalid owned translation table"
        );
        // SAFETY: an exclusive token owns this complete aligned RAM page. The
        // engine supplies a bounded slot; tables stay writable Normal NC RAM.
        unsafe { ptr::read_volatile((table as *const u64).add(index)) }
    }

    fn write_entry(&mut self, table: u64, index: usize, entry: u64) {
        assert!(
            index < 512 && self.owns(table),
            "invalid owned translation table"
        );
        unsafe {
            ptr::write_volatile((table as *mut u64).add(index), entry);
        }
    }

    fn release_table(&mut self, table: u64) {
        let slot = self
            .pages
            .iter_mut()
            .find(|slot| {
                slot.as_ref()
                    .is_some_and(|page| page.start_address() as u64 == table)
            })
            .expect("owned translation table");
        super::free_pages(slot.take().unwrap()).expect("owned translation page release");
        self.count -= 1;
    }
}

struct State {
    ram: RegionSet,
    unmapped: RegionSet,
    framebuffer: Option<Region>,
    ready: bool,
    active: bool,
    plan: MappingPlan,
    memory: PhysicalTableMemory,
    tables: Option<PageTables>,
}

// Static planning/storage avoids copying a large plan onto the 64 KiB stack.
static STATE: Cpu0Cell<State> = Cpu0Cell::new(State {
    ram: RegionSet::new(),
    unmapped: RegionSet::new(),
    framebuffer: None,
    ready: false,
    active: false,
    plan: MappingPlan::new(),
    memory: PhysicalTableMemory::new(),
    tables: None,
});

pub(super) fn remember_layout(layout: &MemoryLayout, framebuffer: Option<Region>) {
    STATE.with(|state| {
        state.ram.clone_from(&layout.ram);
        state.unmapped.clone_from(&layout.unmapped);
        state.framebuffer = framebuffer;
        state.ready = true;
    });
}

fn sections() -> KernelSections {
    KernelSections {
        text: Region {
            start: ptr::addr_of!(__text_start) as usize,
            end: ptr::addr_of!(__text_end) as usize,
        },
        rodata: Region {
            start: ptr::addr_of!(__rodata_start) as usize,
            end: ptr::addr_of!(__rodata_end) as usize,
        },
        data: Region {
            start: ptr::addr_of!(__data_start) as usize,
            end: ptr::addr_of!(__kernel_end) as usize,
        },
    }
}

pub(crate) fn kernel_root() -> Result<u64, InitError> {
    STATE.with(|state| {
        if !state.active {
            return Err(InitError::MemoryNotReady);
        }
        Ok(state
            .tables
            .as_ref()
            .ok_or(InitError::MemoryNotReady)?
            .root_address())
    })
}

/// Build a fresh offline root with the shared, EL1-only kernel mappings. The
/// caller adds user pages, validates it and owns it until switching away.
pub(crate) fn new_task_root(memory: &mut impl TableMemory) -> Result<PageTables, InitError> {
    STATE.with(|state| {
        if !state.active {
            return Err(InitError::MemoryNotReady);
        }
        let mut tables = PageTables::new(memory)?;
        for mapping in state.plan.as_slice() {
            let start = mapping.region.start as u64;
            tables.map_range(
                memory,
                start,
                start,
                (mapping.region.end - mapping.region.start) as u64,
                mapping.attrs,
            )?;
        }
        Ok(tables)
    })
}

/// Build and validate the identity map, then turn on EL1 translation only.
///
/// # Safety
/// Run once on CPU0 at EL1h after successful memory initialization and before
/// unmasking IRQ/FIQ. Every preexisting pointer must refer to the planned RAM or
/// explicit device mappings; firmware handoff must keep MMU/cache disabled.
pub unsafe fn init() -> Result<(), InitError> {
    STATE.with(|state| -> Result<(), InitError> {
        if !state.ready {
            return Err(InitError::MemoryNotReady);
        }
        if state.tables.is_some() || state.active {
            return Err(InitError::AlreadyPrepared);
        }
        let result = prepare(state);
        if let Err(error) = result {
            state.memory.release_all();
            return Err(error);
        }
        let root = state.tables.as_ref().unwrap().root_address();
        crate::println!(
            "[INFO] MMU ROOT: {root:#018x}, TABLE PAGES: {}",
            state.memory.count
        );
        let registers = match unsafe { cpu::enable(root) } {
            Ok(registers) => registers,
            Err(error) => {
                state.tables = None;
                state.memory.release_all();
                return Err(error.into());
            }
        };
        state.active = true;
        // These diagnostics execute through the newly installed mappings.
        crate::println!("[ OK ] EL1 MMU ENABLED (C/I OFF)");
        crate::println!("SCTLR_EL1: {:#018x}", registers.sctlr);
        crate::println!("TCR_EL1  : {:#018x}", registers.tcr);
        crate::println!("MAIR_EL1 : {:#018x}", registers.mair);
        crate::println!("TTBR0_EL1: {:#018x}", registers.ttbr0);
        hardware_probes(state)?;
        Ok(())
    })?;
    // Actual allocation/zeroing and firmware roundtrip after M=1, using the
    // same identity addresses and non-cacheable mailbox buffer as before.
    super::runtime::heap_self_test().map_err(|_| InitError::MemorySelfTest)?;
    let pages = super::allocate_zeroed_pages(1, 1).map_err(|_| InitError::MemorySelfTest)?;
    let good = (0..PAGE_SIZE / 8).all(|index| unsafe {
        ptr::read_volatile((pages.start_address() as *const u64).add(index)) == 0
    });
    super::free_pages(pages).map_err(|_| InitError::MemorySelfTest)?;
    if !good {
        return Err(InitError::MemorySelfTest);
    }
    let revision = Mailbox::new()
        .firmware_revision()
        .map_err(InitError::Firmware)?;
    crate::println!("[ OK ] MMU MEMORY/MAILBOX SELF-TEST: {revision:#010x}");
    Ok(())
}

fn prepare(state: &mut State) -> Result<(), InitError> {
    let sections = sections();
    state.plan.build_in_place(
        state.ram.as_slice(),
        state.unmapped.as_slice(),
        sections,
        state.framebuffer,
    )?;
    let mut tables = PageTables::new(&mut state.memory)?;
    for mapping in state.plan.as_slice() {
        let start = mapping.region.start as u64;
        let size = (mapping.region.end - mapping.region.start) as u64;
        tables.map_range(&mut state.memory, start, start, size, mapping.attrs)?;
    }
    // Walk each plan edge and all live table pages before switching TTBR0.
    for mapping in state.plan.as_slice() {
        check_translation(&tables, &state.memory, mapping.region.start, mapping.attrs)?;
        check_translation(
            &tables,
            &state.memory,
            mapping.region.end - 1,
            mapping.attrs,
        )?;
    }
    for page in state.memory.pages.iter().flatten() {
        let address = page.start_address();
        let translation = tables
            .lookup(&state.memory, address as u64)
            .ok_or(InitError::SoftwareWalk)?;
        if translation.physical_address != address as u64
            || !translation.attrs.writable
            || translation.attrs.executable
        {
            return Err(InitError::SoftwareWalk);
        }
    }
    let stack: usize;
    unsafe {
        core::arch::asm!("mov {stack}, sp", stack = out(reg) stack, options(nomem, nostack, preserves_flags));
    }
    check_translation(
        &tables,
        &state.memory,
        stack,
        super::paging::MappingAttrs {
            memory: super::paging::MemoryType::NormalNonCacheable,
            writable: true,
            executable: false,
        },
    )?;
    let text_attrs = MappingAttrs {
        memory: super::paging::MemoryType::NormalNonCacheable,
        writable: false,
        executable: true,
    };
    check_translation(
        &tables,
        &state.memory,
        cpu::enable as *const () as usize,
        text_attrs,
    )?;
    check_translation(
        &tables,
        &state.memory,
        ptr::addr_of!(__exception_vectors) as usize,
        text_attrs,
    )?;
    let device_attrs = MappingAttrs {
        memory: super::paging::MemoryType::Device,
        writable: true,
        executable: false,
    };
    // A DT no-map span can remove a device page. Reject that conflict BEFORE
    // enabling: UART must remain available even to report a later exception.
    for address in [0xFE21_5040, 0xFE00_B880, 0xFF84_1000, 0xFF84_2000] {
        check_translation(&tables, &state.memory, address, device_attrs)?;
    }
    if tables.lookup(&state.memory, 0).is_some() {
        return Err(InitError::SoftwareWalk);
    }
    state.tables = Some(tables);
    Ok(())
}

fn check_translation(
    tables: &PageTables,
    memory: &PhysicalTableMemory,
    address: usize,
    attrs: MappingAttrs,
) -> Result<(), InitError> {
    let value = tables
        .lookup(memory, address as u64)
        .ok_or(InitError::SoftwareWalk)?;
    if value.physical_address != address as u64 || value.attrs != attrs {
        return Err(InitError::SoftwareWalk);
    }
    Ok(())
}

fn hardware_probes(state: &State) -> Result<(), InitError> {
    let sections = sections();
    cpu::probe(sections.text.start, false)?;
    cpu::probe(ptr::addr_of!(__exception_vectors) as usize, false)?;
    cpu::probe(sections.rodata.start, false)?;
    cpu::probe(sections.data.start, true)?;
    cpu::probe(0xFE21_5040, true)?; // Mini UART AUX_MU_IO_REG
    cpu::probe(0xFE00_B880, true)?; // Property mailbox
    cpu::probe(0xFF84_2000, true)?; // GIC-400 CPU interface
    if let Some(framebuffer) = state.framebuffer {
        cpu::probe(framebuffer.start, true)?;
    }
    if !matches!(
        cpu::probe(sections.text.start, true),
        Err(MmuError::TranslationFault(_))
    ) || !matches!(
        cpu::probe(sections.rodata.start, true),
        Err(MmuError::TranslationFault(_))
    ) || !matches!(cpu::probe(0, false), Err(MmuError::TranslationFault(_)))
    {
        return Err(InitError::HardwareProbe);
    }
    crate::println!("[ OK ] MMU AT PROBES: IDENTITY, RO TEXT/RODATA, NULL FAULT");
    Ok(())
}
