use core::cell::{Cell, UnsafeCell};
use core::ptr::{read_volatile, write_volatile};

use kernel::memory::dtb::{self, DtbError};
use kernel::memory::page::{PAGE_SIZE, PageAllocator, PageError, PageStats};
use kernel::memory::region::{RangeError, Region};

use crate::arch::interrupt::with_irq_masked;
use crate::drivers::mailbox::{Mailbox, MailboxError};

// 16 GiB of physical address coverage, including Pi4 RAM above 4 GiB.
// Two bitmaps cost 1 MiB of BSS; never construct them on the 64 KiB stack.
const BITMAP_WORDS: usize = 65_536;
const MAX_DTB_SIZE: usize = 2 * 1024 * 1024;
const LOW_MEMORY_LIMIT: usize = 0x4000_0000;

struct Pages {
    allocator: UnsafeCell<PageAllocator<BITMAP_WORDS>>,
    ready: Cell<bool>,
}

// Only CPU0 runs; every access is IRQ-masked, never exposes a reference, and
// executes no callback into the console or exception subsystem while borrowed.
unsafe impl Sync for Pages {}

static PAGES: Pages = Pages {
    allocator: UnsafeCell::new(PageAllocator::new()),
    ready: Cell::new(false),
};

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

#[derive(Debug)]
pub enum MemoryError {
    InvalidDtbPointer,
    InvalidDtbSize,
    Dtb(DtbError),
    Range(RangeError),
    Mailbox(MailboxError),
    Page(PageError),
    NoUsablePages,
    SelfTest,
}

impl From<RangeError> for MemoryError {
    fn from(error: RangeError) -> Self {
        Self::Range(error)
    }
}

impl From<PageError> for MemoryError {
    fn from(error: PageError) -> Self {
        Self::Page(error)
    }
}

/// # Safety
/// `dtb_address` must be the firmware-provided readable, immutable physical DTB.
/// Header checks cannot prove an arbitrary physical pointer is backed by RAM.
/// Call once on CPU0 during boot, with secondary cores parked and MMU/caches
/// disabled. Firmware must describe usable RAM and all live reservations.
pub unsafe fn init(
    dtb_address: usize,
    framebuffer: Option<Region>,
) -> Result<PageStats, MemoryError> {
    let kernel = Region::new(
        core::ptr::addr_of!(__kernel_start) as usize,
        core::ptr::addr_of!(__kernel_end) as usize,
    )?;
    if dtb_address == 0 || dtb_address & 7 != 0 {
        return Err(MemoryError::InvalidDtbPointer);
    }
    let header = Region::from_size(dtb_address, 40)?;
    if header.end > LOW_MEMORY_LIMIT || header.overlaps(kernel) {
        return Err(MemoryError::InvalidDtbPointer);
    }

    // Read only the fixed header first. Firmware has relinquished DTB ownership;
    // MMU/caches are disabled. The parser checks magic/version/block boundaries.
    let mut prefix = [0u8; 8];
    for (index, byte) in prefix.iter_mut().enumerate() {
        *byte = unsafe { read_volatile((dtb_address as *const u8).add(index)) };
    }
    if u32::from_be_bytes(prefix[..4].try_into().unwrap()) != 0xd00d_feed {
        return Err(MemoryError::InvalidDtbPointer);
    }
    let size = u32::from_be_bytes(prefix[4..].try_into().unwrap()) as usize;
    if !(40..=MAX_DTB_SIZE).contains(&size) {
        return Err(MemoryError::InvalidDtbSize);
    }
    let dtb_region = Region::from_size(dtb_address, size)?;
    if dtb_region.end > LOW_MEMORY_LIMIT || dtb_region.overlaps(kernel) {
        return Err(MemoryError::InvalidDtbPointer);
    }
    let blob = unsafe { core::slice::from_raw_parts(dtb_address as *const u8, size) };
    let mut info = dtb::parse(blob).map_err(MemoryError::Dtb)?;
    if !info
        .ram
        .as_slice()
        .iter()
        .any(|ram| ram.start <= dtb_region.start && ram.end >= dtb_region.end)
    {
        return Err(MemoryError::InvalidDtbPointer);
    }

    // The loader and firmware startup structures live below the kernel image.
    info.reserved.push(Region::new(0, 0x0020_0000)?)?;
    info.reserved.push(kernel)?; // Includes BSS bitmaps and the complete boot stack.
    info.reserved.push(dtb_region)?;
    // Low peripheral aliases, GIC and local peripherals must never become RAM.
    info.reserved
        .push(Region::new(0xfc00_0000, 0x1_0000_0000)?)?;

    let (vc_start, vc_size) = Mailbox::new().vc_memory().map_err(MemoryError::Mailbox)?;
    if vc_size != 0 {
        info.reserved.push(Region::from_size(vc_start, vc_size)?)?;
    }
    if let Some(framebuffer) = framebuffer {
        info.reserved.push(framebuffer)?;
    }

    // Firmware describes dynamic reservation constraints; this OS chooses their
    // physical location only after all currently live regions are excluded.
    dtb::resolve_dynamic(&mut info).map_err(MemoryError::Dtb)?;

    for ram in info.ram.as_slice() {
        crate::println!("RAM          : {:#018x}..{:#018x}", ram.start, ram.end);
    }
    crate::println!("DTB size     : {} bytes", info.total_size);
    crate::println!("Reserved     : {} regions", info.reserved.as_slice().len());
    let stats = with_irq_masked(|| {
        // Single initialization during boot, before IRQ unmasking.
        let pages = unsafe { &mut *PAGES.allocator.get() };
        pages.initialize(info.ram.as_slice(), info.reserved.as_slice())?;
        Ok::<_, PageError>(pages.stats())
    })?;
    if stats.free_pages == 0 {
        return Err(MemoryError::NoUsablePages);
    }
    self_test()?;
    with_irq_masked(|| PAGES.ready.set(true));
    Ok(stats)
}

/// The returned physical page is owned by the caller and cleared before use.
pub fn allocate_zeroed_page() -> Result<usize, PageError> {
    zeroed_page(true)
}

fn zeroed_page(require_ready: bool) -> Result<usize, PageError> {
    let address = with_irq_masked(|| {
        if require_ready && !PAGES.ready.get() {
            return Err(PageError::NotInitialized);
        }
        unsafe { (&mut *PAGES.allocator.get()).alloc() }
    })?;
    // Page-aligned u64 volatile stores avoid unaligned/exclusive memory accesses
    // in the current MMU-off environment. No other caller owns this page.
    for word in 0..PAGE_SIZE / 8 {
        unsafe { write_volatile((address as *mut u64).add(word), 0) };
    }
    Ok(address)
}

/// # Safety
/// The caller must own this page and ensure no reference, DMA transfer or
/// mapping can access it after release. Reserved pages are rejected.
pub unsafe fn free_page(address: usize) -> Result<(), PageError> {
    release_page(address, true)
}

fn release_page(address: usize, require_ready: bool) -> Result<(), PageError> {
    with_irq_masked(|| {
        if require_ready && !PAGES.ready.get() {
            return Err(PageError::NotInitialized);
        }
        unsafe { (&mut *PAGES.allocator.get()).free(address) }
    })
}

pub fn stats() -> PageStats {
    with_irq_masked(|| unsafe { (&*PAGES.allocator.get()).stats() })
}

fn self_test() -> Result<(), MemoryError> {
    let before = stats();
    // These private operations are allowed only while boot owns the pool.
    // Public allocation remains disabled until every check succeeds.
    let first = zeroed_page(false)?;
    let second = match zeroed_page(false) {
        Ok(page) => page,
        Err(error) => {
            release_page(first, false)?;
            return Err(error.into());
        }
    };
    let distinct = first != second;
    let mut zeroed = true;
    for word in 0..PAGE_SIZE / 8 {
        zeroed &= unsafe { read_volatile((first as *const u64).add(word)) == 0 };
        zeroed &= unsafe { read_volatile((second as *const u64).add(word)) == 0 };
    }
    let pattern = 0x414e_414e_4f53_5047u64;
    unsafe { write_volatile(first as *mut u64, pattern) };
    let preserved = unsafe { read_volatile(first as *const u64) == pattern };
    release_page(first, false)?;
    release_page(second, false)?;
    let reused = zeroed_page(false)?;
    let cleared = unsafe { read_volatile(reused as *const u64) == 0 };
    release_page(reused, false)?;
    if !distinct || !zeroed || !preserved || !cleared || reused != first || stats() != before {
        return Err(MemoryError::SelfTest);
    }
    crate::println!("[ OK ] PAGE ALLOC/FREE/ZERO SELF-TEST");
    Ok(())
}
