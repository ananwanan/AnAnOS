//! CPU0 bootstrap and allocator access while MMU/cache remain disabled.

use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr;

use super::fdt::{self, FdtError};
use super::heap::Heap;
use super::page::{PAGE_SIZE, PageAllocator, PageError, PageStats, PhysicalPages};
use super::regions::{Region, RegionError, RegionSet};
use crate::drivers::mailbox::{Mailbox, MailboxError};

const DTB_LIMIT: usize = 256 * 1024;
const LOW_MEMORY_END: usize = 0x4000_0000;
pub const HEAP_SIZE: usize = 1024 * 1024;

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

#[derive(Debug)]
pub enum MemoryError {
    InvalidDtbAddress,
    InvalidDtbSize,
    DtbOverlapsKernel,
    Region(RegionError),
    Fdt(FdtError),
    Firmware(MailboxError),
    Pages(PageError),
    Heap(&'static str),
    SelfTest,
}

impl From<RegionError> for MemoryError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}
impl From<FdtError> for MemoryError {
    fn from(error: FdtError) -> Self {
        Self::Fdt(error)
    }
}
impl From<PageError> for MemoryError {
    fn from(error: PageError) -> Self {
        Self::Pages(error)
    }
}

/// No LDXR/STXR spinlock: secondary cores remain parked and CPU0 masks IRQ/FIQ
/// while accessing allocator state. This is deliberately not an SMP lock.
struct Cpu0Cell<T>(UnsafeCell<T>);

// SAFETY: used exclusively on CPU0. All accesses go through `with`; IRQ/FIQ
// cannot preempt its closure. Allocator closures never call into the allocator.
unsafe impl<T> Sync for Cpu0Cell<T> {}

impl<T> Cpu0Cell<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    fn with<R>(&self, function: impl FnOnce(&mut T) -> R) -> R {
        let _guard = InterruptGuard::new();
        // SAFETY: the single active CPU and interrupt mask serialize this borrow.
        unsafe { function(&mut *self.0.get()) }
    }
}

struct InterruptGuard(u64);

impl InterruptGuard {
    fn new() -> Self {
        let saved;
        // No `nomem`: keep compiler memory accesses inside the critical section.
        unsafe {
            asm!(
                "mrs {saved}, daif",
                "msr daifset, #3",
                "isb",
                saved = out(reg) saved,
                options(nostack, preserves_flags),
            );
        }
        Self(saved)
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        unsafe {
            asm!("msr daif, {saved}", "isb", saved = in(reg) self.0,
                 options(nostack, preserves_flags));
        }
    }
}

struct PageState {
    allocator: PageAllocator,
    // Retain ownership forever: the heap's backing pages cannot be freed via
    // the public page API while heap objects still reference them.
    heap_pages: Option<PhysicalPages>,
}

static PAGES: Cpu0Cell<PageState> = Cpu0Cell::new(PageState {
    allocator: PageAllocator::new(),
    heap_pages: None,
});

struct KernelAllocator(Cpu0Cell<Heap>);

// SAFETY: Heap's ownership contract is established once by bootstrap. CPU0Cell
// serializes access without allocation or logging. Exhaustion returns null.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.with(|heap| unsafe { heap.allocate(layout) })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        self.0
            .with(|heap| unsafe { heap.deallocate(pointer, layout) });
    }
}

#[global_allocator]
static KERNEL_ALLOCATOR: KernelAllocator = KernelAllocator(Cpu0Cell::new(Heap::new()));

/// Identify RAM/reservations first, test page ownership, then donate a page run
/// to the heap. Missing/invalid input never falls back to a guessed RAM size.
///
/// # Safety
/// Must run once on CPU0, using the firmware DTB forwarded by early boot.
/// The DTB header and its declared size must be readable immutable physical RAM.
/// The framebuffer range, if supplied, must cover the complete firmware buffer.
pub unsafe fn init(
    dtb_address: usize,
    framebuffer: Option<Region>,
) -> Result<PageStats, MemoryError> {
    if dtb_address == 0 || dtb_address % 8 != 0 || dtb_address > LOW_MEMORY_END - 40 {
        return Err(MemoryError::InvalidDtbAddress);
    }
    let kernel_start = ptr::addr_of!(__kernel_start) as usize;
    let kernel_end = ptr::addr_of!(__kernel_end) as usize;
    if dtb_address < kernel_end && dtb_address + 40 > kernel_start {
        return Err(MemoryError::DtbOverlapsKernel);
    }
    // SAFETY: firmware/bootloader supplied readable, aligned physical storage.
    let header = unsafe { core::slice::from_raw_parts(dtb_address as *const u8, 40) };
    let size = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    let end = dtb_address
        .checked_add(size)
        .ok_or(MemoryError::InvalidDtbSize)?;
    if !(40..=DTB_LIMIT).contains(&size) || end > LOW_MEMORY_END {
        return Err(MemoryError::InvalidDtbSize);
    }
    if dtb_address < kernel_end && end > kernel_start {
        return Err(MemoryError::DtbOverlapsKernel);
    }

    let mut reserved = RegionSet::new();
    // Covers low firmware stubs/spin tables, parked bootloader cores, preserved
    // DTB buffer and the kernel's text/data/BSS/allocator metadata/64 KiB stack.
    reserved.insert(Region {
        start: 0,
        end: kernel_end,
    })?;
    reserved.insert(Region {
        start: dtb_address,
        end,
    })?;
    // BCM2711 peripheral/GIC windows are never general-purpose RAM. High RAM
    // banks above 4 GiB remain eligible when described by the DT.
    reserved.insert(Region {
        start: 0xFC00_0000,
        end: 0x1_0000_0000,
    })?;
    if let Some(framebuffer) = framebuffer {
        reserved.insert(framebuffer)?;
    }
    let (vc_start, vc_size) = Mailbox::new().vc_memory().map_err(MemoryError::Firmware)?;
    if vc_size != 0 {
        reserved.insert(Region::new(vc_start, vc_size)?)?;
    }
    // SAFETY: full length was checked above; bootloader keeps this copy alive.
    let blob = unsafe { core::slice::from_raw_parts(dtb_address as *const u8, size) };
    let layout = fdt::parse_with_reserved(blob, reserved.as_slice())?;

    crate::println!("[INFO] PHYSICAL MEMORY (4 KIB PAGES)");
    for region in layout.ram.as_slice() {
        crate::println!("RAM      : {:#018x}..{:#018x}", region.start, region.end);
    }
    for region in layout.reserved.as_slice() {
        crate::println!("RESERVED : {:#018x}..{:#018x}", region.start, region.end);
    }
    PAGES.with(|state| {
        state
            .allocator
            .init(layout.ram.as_slice(), layout.reserved.as_slice())
    })?;
    page_self_test()?;
    crate::println!("[ OK ] PHYSICAL PAGE SELF-TEST");

    PAGES.with(|state| -> Result<(), MemoryError> {
        let pages = state.allocator.alloc_pages(HEAP_SIZE / PAGE_SIZE, 1)?;
        let address = pages.start_address();
        // SAFETY: this exclusive contiguous page run is ordinary writable RAM;
        // ownership is retained in PageState for the entire kernel lifetime.
        let result = KERNEL_ALLOCATOR
            .0
            .with(|heap| unsafe { heap.init(address, HEAP_SIZE) });
        if let Err(error) = result {
            state.allocator.free_pages(pages)?;
            return Err(MemoryError::Heap(error));
        }
        state.heap_pages = Some(pages);
        Ok(())
    })?;
    heap_self_test()?;
    crate::println!("[ OK ] KERNEL HEAP: {} BYTES", HEAP_SIZE);
    crate::println!("[ OK ] HEAP ALLOCATION/FREE SELF-TEST");
    Ok(page_stats())
}

pub fn allocate_pages(count: usize, alignment_pages: usize) -> Result<PhysicalPages, PageError> {
    PAGES.with(|state| state.allocator.alloc_pages(count, alignment_pages))
}

pub fn free_pages(pages: PhysicalPages) -> Result<(), PageError> {
    PAGES.with(|state| state.allocator.free_pages(pages))
}

pub fn page_stats() -> PageStats {
    PAGES.with(|state| state.allocator.stats())
}

pub fn heap_free_bytes() -> usize {
    KERNEL_ALLOCATOR.0.with(|heap| heap.free_bytes())
}

fn page_self_test() -> Result<(), MemoryError> {
    let before = page_stats();
    let pages = allocate_pages(2, 2)?;
    let start = pages.start_address();
    let tail = start + pages.byte_len() - core::mem::size_of::<u64>();
    // SAFETY: pages belong exclusively to this test, addresses are u64 aligned.
    let valid = unsafe {
        ptr::write_volatile(start as *mut u64, 0xA11A_0000_1234_5678);
        ptr::write_volatile(tail as *mut u64, 0x55AA_F00D_DEAD_BEEF);
        ptr::read_volatile(start as *const u64) == 0xA11A_0000_1234_5678
            && ptr::read_volatile(tail as *const u64) == 0x55AA_F00D_DEAD_BEEF
    };
    free_pages(pages)?;
    if !valid || page_stats() != before {
        return Err(MemoryError::SelfTest);
    }
    Ok(())
}

fn heap_self_test() -> Result<(), MemoryError> {
    let before = heap_free_bytes();
    let layout = Layout::from_size_align(8193, PAGE_SIZE).map_err(|_| MemoryError::SelfTest)?;
    // Exercise the actual registered GlobalAlloc, including page alignment and
    // zeroing. Use fallible allocation so a failed bootstrap remains diagnosable.
    let pointer = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if pointer.is_null() {
        return Err(MemoryError::SelfTest);
    }
    let mut valid = pointer as usize % PAGE_SIZE == 0;
    unsafe {
        valid &= *pointer == 0 && *pointer.add(layout.size() - 1) == 0;
        pointer.write(0x5A);
        pointer.add(layout.size() - 1).write(0xA5);
        valid &= *pointer == 0x5A && *pointer.add(layout.size() - 1) == 0xA5;
        alloc::alloc::dealloc(pointer, layout);
    }
    {
        let mut values = alloc::vec::Vec::<u64>::new();
        values
            .try_reserve_exact(32)
            .map_err(|_| MemoryError::SelfTest)?;
        for value in 0..32 {
            values.push(value);
        }
        valid &= values.iter().sum::<u64>() == 496;
    }
    if !valid || before != heap_free_bytes() {
        return Err(MemoryError::SelfTest);
    }
    Ok(())
}
