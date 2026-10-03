//! Host integration checks for EL0 descriptor permissions and page ownership.
//! The CPU model walks raw descriptors independently of `PageTables::lookup`.
//! No host process dereferences a simulated physical or virtual address.

use super::abi::{self, Errno, SyscallRequest};
use super::space::{self, UserAccess};
use crate::page::{PAGE_SIZE, PageAllocator, PageError, PhysicalPages};
use crate::paging::{MappingAttrs, MemoryType, PageTables, PagingError, TableMemory};
use crate::regions::Region;
use std::boxed::Box;
use std::collections::BTreeMap;
use std::vec::Vec;

const POOL_START: usize = 0x0080_0000;
const POOL_PAGES: usize = 128;
const KERNEL_TEXT: u64 = 0x0020_0000;
const KERNEL_DATA: u64 = KERNEL_TEXT + PAGE_SIZE as u64;
const UART: u64 = 0xFE21_5040;
const GIC: u64 = 0xFF84_1000;

struct HostTable {
    owner: PhysicalPages,
    entries: Box<[u64; 512]>,
}

struct HostData {
    owner: PhysicalPages,
    bytes: Vec<u8>,
}

struct Store {
    // Allocator tokens record the owner's address, which boxing keeps fixed.
    allocator: Box<PageAllocator<2>>,
    tables: BTreeMap<u64, HostTable>,
    data: BTreeMap<u64, HostData>,
    allocation_limit: usize,
}

impl Store {
    fn new() -> Self {
        let mut allocator = Box::new(PageAllocator::<2>::new());
        allocator
            .init(
                &[Region {
                    start: POOL_START,
                    end: POOL_START + POOL_PAGES * PAGE_SIZE,
                }],
                &[],
            )
            .unwrap();
        Self {
            allocator,
            tables: BTreeMap::new(),
            data: BTreeMap::new(),
            allocation_limit: POOL_PAGES,
        }
    }

    fn allocate(&mut self, count: usize) -> Result<PhysicalPages, PagingError> {
        if self.allocator.stats().allocated_pages + count > self.allocation_limit {
            return Err(PagingError::OutOfMemory);
        }
        match self.allocator.alloc_pages(count, 1) {
            Ok(owner) => Ok(owner),
            Err(PageError::OutOfMemory) => Err(PagingError::OutOfMemory),
            Err(error) => panic!("unexpected physical allocator error: {error:?}"),
        }
    }

    fn allocate_data(&mut self, count: usize) -> Result<u64, PagingError> {
        let owner = self.allocate(count)?;
        let address = owner.start_address() as u64;
        let data = HostData {
            bytes: std::vec![0; owner.byte_len()],
            owner,
        };
        assert!(self.data.insert(address, data).is_none());
        self.assert_accounting();
        Ok(address)
    }

    fn release_data(&mut self, address: u64) {
        let data = self.data.remove(&address).unwrap();
        self.allocator.free_pages(data.owner).unwrap();
        self.assert_accounting();
    }

    fn data_range(&self, address: u64, length: usize) -> Option<(u64, usize)> {
        let (&base, data) = self.data.range(..=address).next_back()?;
        let offset = usize::try_from(address.checked_sub(base)?).ok()?;
        if offset.checked_add(length)? > data.bytes.len() {
            return None;
        }
        Some((base, offset))
    }

    fn bytes(&self, address: u64, length: usize) -> &[u8] {
        let (base, offset) = self.data_range(address, length).unwrap();
        &self.data[&base].bytes[offset..offset + length]
    }

    fn bytes_mut(&mut self, address: u64, length: usize) -> &mut [u8] {
        let (base, offset) = self.data_range(address, length).unwrap();
        &mut self.data.get_mut(&base).unwrap().bytes[offset..offset + length]
    }

    fn assert_accounting(&self) {
        let stats = self.allocator.stats();
        let data_pages: usize = self.data.values().map(|data| data.owner.page_count()).sum();
        assert_eq!(stats.allocated_pages, self.tables.len() + data_pages);
        assert_eq!(stats.free_pages + stats.allocated_pages, POOL_PAGES);
        assert_eq!(stats.reserved_pages, 0);
    }

    fn assert_empty(&self) {
        self.assert_accounting();
        assert!(self.tables.is_empty() && self.data.is_empty());
        assert_eq!(self.allocator.stats().free_pages, POOL_PAGES);
    }
}

impl TableMemory for Store {
    fn allocate_table(&mut self) -> Result<u64, PagingError> {
        let owner = self.allocate(1)?;
        let address = owner.start_address() as u64;
        assert!(
            self.tables
                .insert(
                    address,
                    HostTable {
                        owner,
                        entries: Box::new([u64::MAX; 512]),
                    },
                )
                .is_none()
        );
        self.assert_accounting();
        Ok(address)
    }

    fn read_entry(&self, table: u64, index: usize) -> u64 {
        self.tables[&table].entries[index]
    }

    fn write_entry(&mut self, table: u64, index: usize, entry: u64) {
        self.tables.get_mut(&table).unwrap().entries[index] = entry;
    }

    fn release_table(&mut self, table: u64) {
        let table = self.tables.remove(&table).unwrap();
        self.allocator.free_pages(table.owner).unwrap();
        self.assert_accounting();
    }
}

fn kernel_mappings(tables: &mut PageTables, store: &mut Store) -> Result<(), PagingError> {
    for (address, length, memory, writable, executable) in [
        (
            KERNEL_TEXT,
            PAGE_SIZE as u64,
            MemoryType::NormalNonCacheable,
            false,
            true,
        ),
        (
            KERNEL_DATA,
            PAGE_SIZE as u64,
            MemoryType::NormalNonCacheable,
            true,
            false,
        ),
        (
            POOL_START as u64,
            (POOL_PAGES * PAGE_SIZE) as u64,
            MemoryType::NormalNonCacheable,
            true,
            false,
        ),
        (0xFC00_0000, 0x0400_0000, MemoryType::Device, true, false),
    ] {
        tables.map_range(
            store,
            address,
            address,
            length,
            MappingAttrs {
                memory,
                writable,
                executable,
            },
        )?;
    }
    Ok(())
}

struct Task {
    tables: PageTables,
    owned_data: Vec<u64>,
    code: u64,
    first_data: u64,
    second_data: u64,
    stack: u64,
}

impl Task {
    fn create(store: &mut Store) -> Result<Self, PagingError> {
        let mut owned_data = Vec::new();
        let mut tables = None;
        let result = (|| {
            let code = store.allocate_data(1)?;
            owned_data.push(code);
            let first_data = store.allocate_data(1)?;
            owned_data.push(first_data);
            let stack = store.allocate_data(space::USER_STACK_PAGES as usize)?;
            owned_data.push(stack);
            // Place the next virtual data page after a physical stack run. This
            // forces the copy layer to handle physically discontiguous pages.
            let second_data = store.allocate_data(1)?;
            owned_data.push(second_data);
            tables = Some(PageTables::new(store)?);
            let root = tables.as_mut().unwrap();
            kernel_mappings(root, store)?;
            for (va, pa, length, writable, executable) in [
                (space::USER_CODE, code, PAGE_SIZE as u64, false, true),
                (space::USER_DATA, first_data, PAGE_SIZE as u64, true, false),
                (
                    space::USER_DATA + PAGE_SIZE as u64,
                    second_data,
                    PAGE_SIZE as u64,
                    true,
                    false,
                ),
                (
                    space::USER_STACK_BASE,
                    stack,
                    space::USER_STACK_PAGES * PAGE_SIZE as u64,
                    true,
                    false,
                ),
            ] {
                root.map_user_range(store, va, pa, length, writable, executable)?;
            }
            Ok((code, first_data, second_data, stack))
        })();
        match result {
            Ok((code, first_data, second_data, stack)) => Ok(Self {
                tables: tables.take().unwrap(),
                owned_data,
                code,
                first_data,
                second_data,
                stack,
            }),
            Err(error) => {
                if let Some(root) = tables {
                    root.destroy(store);
                }
                for address in owned_data {
                    store.release_data(address);
                }
                Err(error)
            }
        }
    }

    fn owns(&self, store: &Store, address: u64, length: usize) -> bool {
        store
            .data_range(address, length)
            .is_some_and(|(base, _)| self.owned_data.contains(&base))
    }

    fn reclaim(self, store: &mut Store) {
        self.tables.destroy(store);
        for address in self.owned_data {
            store.release_data(address);
        }
    }
}

#[derive(Clone, Copy)]
enum Privilege {
    El0,
    El1,
}

#[derive(Debug, PartialEq, Eq)]
enum CpuFault {
    Translation,
    Permission,
}

#[derive(Debug, PartialEq, Eq)]
struct CpuTranslation {
    physical_address: u64,
    attribute_index: u8,
    global: bool,
}

/// A stage-1 access check using architectural AP/PXN/UXN fields directly, not
/// the software walker's interpreted permissions. Cache/TLB behavior is outside
/// this host model and still needs the real-board probes and faulting programs.
fn cpu_translate(
    store: &Store,
    root: u64,
    address: u64,
    privilege: Privilege,
    access: UserAccess,
) -> Result<CpuTranslation, CpuFault> {
    if address >= 1 << 39 {
        return Err(CpuFault::Translation);
    }
    let mut table = root;
    let mut no_user = false;
    let mut read_only = false;
    let mut privileged_xn = false;
    let mut user_xn = false;
    for (level, shift) in [(1, 30), (2, 21), (3, 12)] {
        let descriptor = store.read_entry(table, ((address >> shift) & 0x1FF) as usize);
        if descriptor & 1 == 0 {
            return Err(CpuFault::Translation);
        }
        if level < 3 && descriptor & 2 != 0 {
            no_user |= descriptor & (1 << 61) != 0;
            read_only |= descriptor & (1 << 62) != 0;
            privileged_xn |= descriptor & (1 << 59) != 0;
            user_xn |= descriptor & (1 << 60) != 0;
            table = descriptor & 0x0000_00FF_FFFF_F000;
            continue;
        }
        if level == 3 && descriptor & 2 == 0 {
            return Err(CpuFault::Translation);
        }
        let user = matches!(privilege, Privilege::El0);
        let writable = descriptor & (1 << 7) == 0 && !read_only;
        let execute_never = if user {
            user_xn || descriptor & (1 << 54) != 0
        } else {
            privileged_xn || descriptor & (1 << 53) != 0
        };
        if (user && (no_user || descriptor & (1 << 6) == 0))
            || (access == UserAccess::Write && !writable)
            || (access == UserAccess::Execute && execute_never)
        {
            return Err(CpuFault::Permission);
        }
        return Ok(CpuTranslation {
            physical_address: (descriptor & 0x0000_00FF_FFFF_F000) | (address & ((1 << shift) - 1)),
            attribute_index: ((descriptor >> 2) & 7) as u8,
            global: descriptor & (1 << 11) == 0,
        });
    }
    Err(CpuFault::Translation)
}

fn check_copy(
    task: &Task,
    store: &Store,
    address: u64,
    length: usize,
    access: UserAccess,
) -> Result<(), Errno> {
    space::validate_user_range(&task.tables, store, address, length, access)
        .map_err(|_| Errno::Fault)?;
    let mut offset = 0;
    while offset < length {
        let chunk = space::user_chunk(
            &task.tables,
            store,
            address + offset as u64,
            length - offset,
            access,
        )
        .map_err(|_| Errno::Fault)?;
        if !task.owns(store, chunk.physical_address, chunk.length) {
            return Err(Errno::Fault);
        }
        offset += chunk.length;
    }
    Ok(())
}

fn write_to_console(
    task: &Task,
    store: &Store,
    address: u64,
    length: u64,
    output: &mut Vec<u8>,
) -> Result<usize, Errno> {
    let request = abi::decode_syscall(abi::SYS_WRITE, [abi::STDOUT, address, length, 0, 0, 0])?;
    let SyscallRequest::Write { length, .. } = request else {
        unreachable!()
    };
    if length == 0 {
        return Ok(0);
    }
    check_copy(task, store, address, length, UserAccess::Read)?;
    let mut offset = 0;
    while offset < length {
        let chunk = space::user_chunk(
            &task.tables,
            store,
            address + offset as u64,
            length - offset,
            UserAccess::Read,
        )
        .unwrap();
        output.extend_from_slice(store.bytes(chunk.physical_address, chunk.length));
        offset += chunk.length;
    }
    Ok(length)
}

fn copy_to_user(task: &Task, store: &mut Store, address: u64, bytes: &[u8]) -> Result<(), Errno> {
    check_copy(task, store, address, bytes.len(), UserAccess::Write)?;
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk = space::user_chunk(
            &task.tables,
            store,
            address + offset as u64,
            bytes.len() - offset,
            UserAccess::Write,
        )
        .unwrap();
        store
            .bytes_mut(chunk.physical_address, chunk.length)
            .copy_from_slice(&bytes[offset..offset + chunk.length]);
        offset += chunk.length;
    }
    Ok(())
}

#[test]
fn independent_cpu_walk_enforces_el0_wx_guards_and_kernel_mmio_isolation() {
    let mut store = Store::new();
    let mut kernel = PageTables::new(&mut store).unwrap();
    kernel_mappings(&mut kernel, &mut store).unwrap();
    let task = Task::create(&mut store).unwrap();
    let root = task.tables.root_address();
    for (address, physical, access) in [
        (space::USER_CODE + 8, task.code + 8, UserAccess::Read),
        (space::USER_CODE + 8, task.code + 8, UserAccess::Execute),
        (space::USER_DATA, task.first_data, UserAccess::Write),
        (
            space::USER_STACK_TOP - 8,
            task.stack + space::USER_STACK_PAGES * PAGE_SIZE as u64 - 8,
            UserAccess::Write,
        ),
    ] {
        let translation = cpu_translate(&store, root, address, Privilege::El0, access).unwrap();
        assert_eq!(translation.physical_address, physical);
        assert_eq!(translation.attribute_index, 0);
        assert!(!translation.global);
    }
    for (address, access) in [
        (space::USER_CODE, UserAccess::Write),
        (space::USER_DATA, UserAccess::Execute),
        (space::USER_STACK_BASE, UserAccess::Execute),
        (KERNEL_TEXT, UserAccess::Read),
        (KERNEL_DATA, UserAccess::Write),
        (task.code, UserAccess::Read),
        (UART, UserAccess::Read),
        (GIC, UserAccess::Write),
    ] {
        assert_eq!(
            cpu_translate(&store, root, address, Privilege::El0, access),
            Err(CpuFault::Permission)
        );
    }
    for address in [0, space::USER_STACK_GUARD, space::USER_STACK_TOP] {
        assert_eq!(
            cpu_translate(&store, root, address, Privilege::El0, UserAccess::Read),
            Err(CpuFault::Translation)
        );
    }
    assert_eq!(
        cpu_translate(
            &store,
            root,
            space::USER_CODE,
            Privilege::El1,
            UserAccess::Execute
        ),
        Err(CpuFault::Permission)
    );
    assert_eq!(
        cpu_translate(
            &store,
            root,
            KERNEL_TEXT,
            Privilege::El1,
            UserAccess::Execute
        )
        .unwrap()
        .physical_address,
        KERNEL_TEXT
    );
    assert_eq!(
        cpu_translate(&store, root, KERNEL_TEXT, Privilege::El1, UserAccess::Write),
        Err(CpuFault::Permission)
    );
    let device = cpu_translate(&store, root, UART, Privilege::El1, UserAccess::Write).unwrap();
    assert_eq!(device.attribute_index, 1);
    assert!(device.global);
    assert_eq!(
        cpu_translate(&store, root, UART, Privilege::El1, UserAccess::Execute),
        Err(CpuFault::Permission)
    );
    assert_eq!(
        cpu_translate(
            &store,
            kernel.root_address(),
            space::USER_CODE,
            Privilege::El0,
            UserAccess::Execute
        ),
        Err(CpuFault::Translation)
    );
    task.reclaim(&mut store);
    kernel.destroy(&mut store);
    store.assert_empty();
}

#[test]
fn full_copy_validation_precedes_console_output_and_user_memory_changes() {
    let mut store = Store::new();
    let task = Task::create(&mut store).unwrap();
    assert_ne!(task.first_data + PAGE_SIZE as u64, task.second_data);
    let address = space::USER_DATA + PAGE_SIZE as u64 - 3;
    copy_to_user(&task, &mut store, address, b"copy-ok").unwrap();
    assert_eq!(
        store.bytes(task.first_data + PAGE_SIZE as u64 - 3, 3),
        b"cop"
    );
    assert_eq!(store.bytes(task.second_data, 4), b"y-ok");
    let mut output = Vec::new();
    assert_eq!(
        write_to_console(&task, &store, address, 7, &mut output),
        Ok(7)
    );
    assert_eq!(output, b"copy-ok");
    let before = output.clone();
    for (address, length) in [
        (KERNEL_TEXT, 1),
        (UART, 1),
        (u64::MAX, 4),
        (space::USER_ADDRESS_END, 1),
        (space::USER_STACK_TOP - 2, 4),
        (space::USER_DATA + (2 * PAGE_SIZE) as u64 - 2, 4),
    ] {
        assert_eq!(
            write_to_console(&task, &store, address, length, &mut output),
            Err(Errno::Fault)
        );
        assert_eq!(output, before);
    }
    assert_eq!(
        write_to_console(&task, &store, address, 4097, &mut output),
        Err(Errno::InvalidArgument)
    );
    assert_eq!(write_to_console(&task, &store, 0, 0, &mut output), Ok(0));
    assert_eq!(output, before);
    let stack_tail = task.stack + space::USER_STACK_PAGES * PAGE_SIZE as u64 - 2;
    let tail_before = store.bytes(stack_tail, 2).to_vec();
    assert_eq!(
        copy_to_user(&task, &mut store, space::USER_STACK_TOP - 2, b"fail"),
        Err(Errno::Fault)
    );
    assert_eq!(store.bytes(stack_tail, 2), tail_before);
    let code_before = store.bytes(task.code, 4).to_vec();
    assert_eq!(
        copy_to_user(&task, &mut store, space::USER_CODE, b"fail"),
        Err(Errno::Fault)
    );
    assert_eq!(store.bytes(task.code, 4), code_before);
    task.reclaim(&mut store);
    store.assert_empty();
}

#[test]
fn switching_private_roots_reuses_user_virtual_addresses_without_sharing_pages() {
    let mut store = Store::new();
    let mut kernel = PageTables::new(&mut store).unwrap();
    kernel_mappings(&mut kernel, &mut store).unwrap();
    let mut first = Task::create(&mut store).unwrap();
    let second = Task::create(&mut store).unwrap();
    assert_ne!(first.tables.root_address(), second.tables.root_address());
    assert_ne!(first.code, second.code);
    assert_ne!(first.first_data, second.first_data);
    copy_to_user(&first, &mut store, space::USER_DATA, b"first").unwrap();
    copy_to_user(&second, &mut store, space::USER_DATA, b"other").unwrap();
    for (task, expected) in [(&first, b"first"), (&second, b"other")] {
        let active_root = task.tables.root_address();
        let translation = cpu_translate(
            &store,
            active_root,
            space::USER_DATA,
            Privilege::El0,
            UserAccess::Read,
        )
        .unwrap();
        assert_eq!(translation.physical_address, task.first_data);
        assert_eq!(store.bytes(translation.physical_address, 5), expected);
        assert_eq!(
            cpu_translate(
                &store,
                active_root,
                KERNEL_TEXT,
                Privilege::El1,
                UserAccess::Execute
            )
            .unwrap()
            .physical_address,
            KERNEL_TEXT
        );
    }
    // Even a mistakenly installed, architecturally valid alias into another
    // task's page must fail the copy layer's independent ownership check.
    let foreign_alias = space::USER_DATA + 0x1_0000;
    first
        .tables
        .map_user_range(
            &mut store,
            foreign_alias,
            second.first_data,
            PAGE_SIZE as u64,
            true,
            false,
        )
        .unwrap();
    space::validate_user_range(&first.tables, &store, foreign_alias, 5, UserAccess::Read).unwrap();
    let mut output = Vec::new();
    assert_eq!(
        write_to_console(&first, &store, foreign_alias, 5, &mut output),
        Err(Errno::Fault)
    );
    assert!(output.is_empty());
    assert_eq!(
        copy_to_user(&first, &mut store, foreign_alias, b"wrong"),
        Err(Errno::Fault)
    );
    assert_eq!(store.bytes(second.first_data, 5), b"other");
    // The hardware implementation must flush before reclaim. This host model
    // has no TLB; explicitly restore the kernel root before releasing a task.
    let active_root = kernel.root_address();
    assert_eq!(
        cpu_translate(
            &store,
            active_root,
            space::USER_DATA,
            Privilege::El0,
            UserAccess::Read
        ),
        Err(CpuFault::Translation)
    );
    first.reclaim(&mut store);
    assert_eq!(store.bytes(second.first_data, 5), b"other");
    second.reclaim(&mut store);
    kernel.destroy(&mut store);
    store.assert_empty();
}

#[test]
fn every_task_construction_allocation_failure_reclaims_pages_and_preserves_kernel_root() {
    let mut baseline = Store::new();
    let mut kernel = PageTables::new(&mut baseline).unwrap();
    kernel_mappings(&mut kernel, &mut baseline).unwrap();
    let kernel_pages = baseline.allocator.stats().allocated_pages;
    let complete = Task::create(&mut baseline).unwrap();
    let task_pages = baseline.allocator.stats().allocated_pages - kernel_pages;
    assert!(task_pages > space::USER_STACK_PAGES as usize + 3);
    complete.reclaim(&mut baseline);
    assert_eq!(baseline.allocator.stats().allocated_pages, kernel_pages);
    kernel.destroy(&mut baseline);
    baseline.assert_empty();
    for budget in 0..task_pages {
        let mut store = Store::new();
        let mut kernel = PageTables::new(&mut store).unwrap();
        kernel_mappings(&mut kernel, &mut store).unwrap();
        let before = store.allocator.stats();
        let kernel_entries: BTreeMap<_, _> = store
            .tables
            .iter()
            .map(|(&address, table)| (address, table.entries.clone()))
            .collect();
        store.allocation_limit = before.allocated_pages + budget;
        assert!(matches!(
            Task::create(&mut store),
            Err(PagingError::OutOfMemory)
        ));
        assert_eq!(store.allocator.stats(), before);
        assert!(store.data.is_empty());
        assert_eq!(store.tables.len(), kernel_entries.len());
        for (address, entries) in kernel_entries {
            assert_eq!(store.tables[&address].entries, entries);
        }
        assert_eq!(
            cpu_translate(
                &store,
                kernel.root_address(),
                KERNEL_TEXT,
                Privilege::El1,
                UserAccess::Execute
            )
            .unwrap()
            .physical_address,
            KERNEL_TEXT
        );
        kernel.destroy(&mut store);
        store.assert_empty();
    }
}
