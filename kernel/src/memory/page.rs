use super::region::Region;

pub const PAGE_SIZE: usize = 4096;
const PAGES_PER_WORD: usize = u64::BITS as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    NotInitialized,
    AlreadyInitialized,
    InvalidRegion,
    Capacity,
    Overflow,
    OutOfMemory,
    Misaligned,
    OutOfRange,
    NotManaged,
    DoubleFree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageStats {
    /// Pages owned by this allocator after excluding holes and reservations.
    pub total_pages: usize,
    pub free_pages: usize,
}

/// CPU-private physical page bookkeeping. The caller supplies synchronization;
/// this implementation deliberately requires exclusive mutable access and uses
/// no exclusive-load/store atomics while the MMU and caches are disabled.
///
/// Bit positions are absolute physical page numbers, so `WORDS` describes the
/// supported physical address ceiling, rather than just the amount of RAM.
/// A managed bit survives allocation; it prevents `free` from admitting holes
/// or reserved memory into the free pool.
pub struct PageAllocator<const WORDS: usize> {
    managed: [u64; WORDS],
    free: [u64; WORDS],
    total_pages: usize,
    free_pages: usize,
    search_word: usize,
    initialized: bool,
}

impl<const WORDS: usize> PageAllocator<WORDS> {
    /// All-zero state permits placement in static BSS without a runtime
    /// constructor. Initialization changes the existing bitmaps in place.
    pub const fn new() -> Self {
        Self {
            managed: [0; WORDS],
            free: [0; WORDS],
            total_pages: 0,
            free_pages: 0,
            search_word: 0,
            initialized: false,
        }
    }

    pub fn initialize(&mut self, ram: &[Region], reserved: &[Region]) -> Result<(), PageError> {
        // Rebuilding these bitmaps would erase ownership of outstanding pages.
        // A live allocator is deliberately initialized exactly once.
        if self.initialized {
            return Err(PageError::AlreadyInitialized);
        }
        let capacity = Self::capacity()?;

        // Validate before mutating state, so a rejected configuration may be
        // corrected and retried without partial allocator initialization.
        for region in ram {
            Self::validate_region(*region)?;
            if region.end > capacity {
                return Err(PageError::Capacity);
            }
            Self::align_up(region.start)?;
        }
        for region in reserved {
            Self::validate_region(*region)?;
            Self::align_up(region.end)?;
        }

        // Do not construct temporary bitmap arrays on the small boot stack.
        self.managed.fill(0);
        self.free.fill(0);
        self.total_pages = 0;
        self.free_pages = 0;
        self.search_word = 0;

        for region in ram {
            // A page must lie completely inside a RAM bank. Partial boundary
            // pages are conservatively unavailable.
            let first = Self::align_up(region.start)? / PAGE_SIZE;
            let end = region.end / PAGE_SIZE;
            for page in first.max(1)..end {
                let word = page / PAGES_PER_WORD;
                let mask = 1_u64 << (page % PAGES_PER_WORD);
                if self.managed[word] & mask == 0 {
                    self.managed[word] |= mask;
                    self.free[word] |= mask;
                    self.total_pages += 1;
                }
            }
        }

        let capacity_pages = capacity / PAGE_SIZE;
        for region in reserved {
            // Reserve every page touched by a byte range. Reservations outside
            // the supported RAM address space have no effect.
            let first = (region.start / PAGE_SIZE).min(capacity_pages);
            let end = (Self::align_up(region.end)? / PAGE_SIZE).min(capacity_pages);
            for page in first..end {
                let word = page / PAGES_PER_WORD;
                let mask = 1_u64 << (page % PAGES_PER_WORD);
                if self.managed[word] & mask != 0 {
                    self.managed[word] &= !mask;
                    self.free[word] &= !mask;
                    self.total_pages -= 1;
                }
            }
        }

        self.free_pages = self.total_pages;
        self.initialized = true;
        Ok(())
    }

    /// Allocate the lowest available physical page. The page contents are not
    /// initialized here; zeroing and memory mapping belong to the caller.
    pub fn alloc(&mut self) -> Result<usize, PageError> {
        if !self.initialized {
            return Err(PageError::NotInitialized);
        }
        if self.free_pages == 0 {
            return Err(PageError::OutOfMemory);
        }
        while self.search_word < WORDS {
            let available = self.free[self.search_word];
            if available == 0 {
                self.search_word += 1;
                continue;
            }
            let bit = available.trailing_zeros() as usize;
            self.free[self.search_word] &= !(1_u64 << bit);
            self.free_pages -= 1;
            let page = self.search_word * PAGES_PER_WORD + bit;
            return Ok(page * PAGE_SIZE);
        }
        // Only reachable if bookkeeping invariants are violated; never return
        // an address outside the initialized bitmap.
        Err(PageError::OutOfMemory)
    }

    pub fn free(&mut self, address: usize) -> Result<(), PageError> {
        if !self.initialized {
            return Err(PageError::NotInitialized);
        }
        if address % PAGE_SIZE != 0 {
            return Err(PageError::Misaligned);
        }
        if address >= Self::capacity()? {
            return Err(PageError::OutOfRange);
        }
        let page = address / PAGE_SIZE;
        let word = page / PAGES_PER_WORD;
        let mask = 1_u64 << (page % PAGES_PER_WORD);
        if self.managed[word] & mask == 0 {
            return Err(PageError::NotManaged);
        }
        if self.free[word] & mask != 0 {
            return Err(PageError::DoubleFree);
        }
        self.free[word] |= mask;
        self.free_pages += 1;
        self.search_word = self.search_word.min(word);
        Ok(())
    }

    pub const fn stats(&self) -> PageStats {
        PageStats {
            total_pages: self.total_pages,
            free_pages: self.free_pages,
        }
    }

    fn capacity() -> Result<usize, PageError> {
        WORDS
            .checked_mul(PAGES_PER_WORD)
            .and_then(|pages| pages.checked_mul(PAGE_SIZE))
            .ok_or(PageError::Overflow)
    }

    fn validate_region(region: Region) -> Result<(), PageError> {
        if region.start >= region.end {
            return Err(PageError::InvalidRegion);
        }
        Ok(())
    }

    fn align_up(address: usize) -> Result<usize, PageError> {
        address
            .checked_add(PAGE_SIZE - 1)
            .map(|value| value & !(PAGE_SIZE - 1))
            .ok_or(PageError::Overflow)
    }
}

impl<const WORDS: usize> Default for PageAllocator<WORDS> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start: usize, end: usize) -> Region {
        Region::new(start, end).unwrap()
    }

    #[test]
    fn excludes_zero_holes_and_partial_ram_pages() {
        let mut pages = PageAllocator::<1>::new();
        pages
            .initialize(
                &[
                    region(0, 3 * PAGE_SIZE + 100),
                    region(5 * PAGE_SIZE + 1, 8 * PAGE_SIZE),
                ],
                &[],
            )
            .unwrap();
        assert_eq!(pages.stats().total_pages, 4);
        for expected in [1, 2, 6, 7] {
            assert_eq!(pages.alloc(), Ok(expected * PAGE_SIZE));
        }
        assert_eq!(pages.alloc(), Err(PageError::OutOfMemory));
        for unavailable in [0, 3, 4, 5, 8] {
            assert_eq!(
                pages.free(unavailable * PAGE_SIZE),
                Err(PageError::NotManaged)
            );
        }
    }

    #[test]
    fn deduplicates_banks_and_rounds_reservations_outward() {
        let mut pages = PageAllocator::<1>::new();
        pages
            .initialize(
                &[region(0, 9 * PAGE_SIZE), region(PAGE_SIZE, 8 * PAGE_SIZE)],
                &[
                    region(2 * PAGE_SIZE + 1, 3 * PAGE_SIZE + 1),
                    region(3 * PAGE_SIZE, 4 * PAGE_SIZE),
                    region(6 * PAGE_SIZE - 1, 6 * PAGE_SIZE),
                    region(100 * PAGE_SIZE, 101 * PAGE_SIZE),
                ],
            )
            .unwrap();
        assert_eq!(pages.stats().total_pages, 5);
        for expected in [1, 4, 6, 7, 8] {
            assert_eq!(pages.alloc(), Ok(expected * PAGE_SIZE));
        }
        assert_eq!(pages.alloc(), Err(PageError::OutOfMemory));
        assert_eq!(pages.stats().free_pages, 0);
        for reserved in [2, 3, 5] {
            assert_eq!(pages.free(reserved * PAGE_SIZE), Err(PageError::NotManaged));
        }
    }

    #[test]
    fn exhaustion_reuse_and_invalid_free_preserve_counts() {
        let mut pages = PageAllocator::<2>::new();
        assert_eq!(pages.alloc(), Err(PageError::NotInitialized));
        assert_eq!(pages.free(PAGE_SIZE), Err(PageError::NotInitialized));
        pages
            .initialize(
                &[region(63 * PAGE_SIZE, 67 * PAGE_SIZE)],
                &[region(64 * PAGE_SIZE, 65 * PAGE_SIZE)],
            )
            .unwrap();
        assert_eq!(pages.free(63 * PAGE_SIZE), Err(PageError::DoubleFree));
        let first = pages.alloc().unwrap();
        assert_eq!(first, 63 * PAGE_SIZE);
        assert_eq!(pages.alloc(), Ok(65 * PAGE_SIZE));
        assert_eq!(pages.alloc(), Ok(66 * PAGE_SIZE));
        assert_eq!(pages.alloc(), Err(PageError::OutOfMemory));
        assert_eq!(pages.free(first + 1), Err(PageError::Misaligned));
        assert_eq!(pages.free(128 * PAGE_SIZE), Err(PageError::OutOfRange));
        assert_eq!(pages.free(PAGE_SIZE), Err(PageError::NotManaged));
        assert_eq!(pages.free(64 * PAGE_SIZE), Err(PageError::NotManaged));
        assert_eq!(pages.stats().free_pages, 0);
        pages.free(first).unwrap();
        assert_eq!(pages.free(first), Err(PageError::DoubleFree));
        assert_eq!(pages.stats().free_pages, 1);
        assert_eq!(pages.alloc(), Ok(first));
        assert_eq!(pages.stats().total_pages, 3);
    }

    #[test]
    fn rejects_bad_geometry_and_reinitialization() {
        let mut pages = PageAllocator::<1>::new();
        assert_eq!(
            pages.initialize(&[region(63 * PAGE_SIZE, 65 * PAGE_SIZE)], &[]),
            Err(PageError::Capacity)
        );
        assert_eq!(
            pages.initialize(&[Region { start: 5, end: 4 }], &[]),
            Err(PageError::InvalidRegion)
        );
        assert_eq!(
            pages.initialize(&[], &[region(usize::MAX - PAGE_SIZE, usize::MAX)]),
            Err(PageError::Overflow)
        );
        assert_eq!(pages.alloc(), Err(PageError::NotInitialized));
        pages
            .initialize(&[region(PAGE_SIZE, 3 * PAGE_SIZE)], &[])
            .unwrap();
        let address = pages.alloc().unwrap();
        assert_eq!(
            pages.initialize(&[region(PAGE_SIZE, 3 * PAGE_SIZE)], &[]),
            Err(PageError::AlreadyInitialized)
        );
        assert_eq!(pages.stats().free_pages, 1);
        pages.free(address).unwrap();
        assert_eq!(pages.stats().free_pages, 2);
        // A bank ending exactly at the physical ceiling is valid.
        let mut ceiling = PageAllocator::<1>::new();
        ceiling
            .initialize(&[region(63 * PAGE_SIZE, 64 * PAGE_SIZE)], &[])
            .unwrap();
        assert_eq!(ceiling.alloc(), Ok(63 * PAGE_SIZE));
        let mut zero = PageAllocator::<0>::new();
        zero.initialize(&[], &[]).unwrap();
        assert_eq!(zero.alloc(), Err(PageError::OutOfMemory));
        assert_eq!(zero.free(0), Err(PageError::OutOfRange));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn retains_absolute_physical_addresses_above_four_gib() {
        extern crate std;
        use std::sync::Mutex;

        const FOUR_GIB: usize = 0x1_0000_0000;
        const WORDS: usize = FOUR_GIB / PAGE_SIZE / PAGES_PER_WORD + 1;
        // This bitmap is over 256 KiB; construct it statically so the host test
        // verifies large geometry without consuming the test thread's stack.
        static PAGES: Mutex<PageAllocator<WORDS>> = Mutex::new(PageAllocator::new());
        let mut pages = PAGES.lock().unwrap();
        pages
            .initialize(
                &[region(FOUR_GIB, FOUR_GIB + 3 * PAGE_SIZE)],
                &[region(FOUR_GIB + PAGE_SIZE + 1, FOUR_GIB + 2 * PAGE_SIZE)],
            )
            .unwrap();
        assert_eq!(pages.stats().total_pages, 2);
        assert_eq!(pages.alloc(), Ok(FOUR_GIB));
        assert_eq!(pages.alloc(), Ok(FOUR_GIB + 2 * PAGE_SIZE));
        assert_eq!(pages.alloc(), Err(PageError::OutOfMemory));
        pages.free(FOUR_GIB).unwrap();
        assert_eq!(pages.alloc(), Ok(FOUR_GIB));
    }
}
