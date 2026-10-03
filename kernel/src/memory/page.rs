use super::regions::Region;

pub const PAGE_SIZE: usize = 4096;
const MAX_BANKS: usize = 128;
/// Three bitmaps cover up to 8 GiB of actual RAM, including banks above 4 GiB.
pub const DEFAULT_BITMAP_WORDS: usize = 8 * 1024 * 1024 * 1024 / PAGE_SIZE / 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    NotInitialized,
    AlreadyInitialized,
    InvalidRegion,
    TooManyRegions,
    CapacityExceeded,
    AddressOverflow,
    NoUsableRam,
    InvalidCount,
    InvalidAlignment,
    OutOfMemory,
    WrongAllocator,
    OutsideRam,
    ReservedPage,
    DoubleFree,
    InvalidAllocation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageStats {
    /// All complete pages wholly contained in the discovered RAM banks.
    pub total_pages: usize,
    /// Pages touching a reserved byte range; overlapping reservations count once.
    pub reserved_pages: usize,
    pub free_pages: usize,
    pub allocated_pages: usize,
}

/// An owned, physically contiguous allocation. There is deliberately no Clone
/// or Copy implementation: freeing consumes the whole allocation.
#[derive(Debug)]
#[must_use = "dropping a page token does not free it; call free_pages to return the run"]
pub struct PhysicalPages {
    start: usize,
    count: usize,
    owner: usize,
}

impl PhysicalPages {
    pub fn start_address(&self) -> usize {
        self.start
    }

    pub fn page_count(&self) -> usize {
        self.count
    }

    pub fn byte_len(&self) -> usize {
        // alloc_pages only returns a run whose checked end fits in its RAM bank.
        self.count * PAGE_SIZE
    }
}

#[derive(Clone, Copy)]
struct PageBank {
    start: usize,
    end: usize,
    first_index: usize,
}

impl PageBank {
    const EMPTY: Self = Self {
        start: 0,
        end: 0,
        first_index: 0,
    };

    fn page_count(self) -> usize {
        (self.end - self.start) / PAGE_SIZE
    }

    fn end_index(self) -> usize {
        self.first_index + self.page_count()
    }
}

/// CPU-local, allocation-free physical page bookkeeping.
///
/// Indexes are compact across RAM banks, rather than derived directly from the
/// physical address. MMIO holes therefore use no bitmap storage. No page memory
/// is accessed or cleared here; callers must zero pages before exposing them to
/// a future userspace process.
///
/// Place the default-capacity allocator in static storage using `const new()`:
/// constructing/copying its 768 KiB bitmaps on the boot stack is inappropriate.
/// Keep its address stable while allocations exist. Moving an allocator causes
/// outstanding tokens to be rejected safely as WrongAllocator. The caller must
/// serialize access (currently CPU0 with IRQ exclusion); no exclusive atomics
/// are used while MMU/cache are disabled.
pub struct PageAllocator<const WORDS: usize = DEFAULT_BITMAP_WORDS> {
    allowed: [u64; WORDS],
    allocated: [u64; WORDS],
    starts: [u64; WORDS],
    banks: [PageBank; MAX_BANKS],
    bank_count: usize,
    stats: PageStats,
    search_hint: usize,
    initialized: bool,
}

impl<const WORDS: usize> PageAllocator<WORDS> {
    pub const fn new() -> Self {
        Self {
            allowed: [0; WORDS],
            allocated: [0; WORDS],
            starts: [0; WORDS],
            banks: [PageBank::EMPTY; MAX_BANKS],
            bank_count: 0,
            stats: PageStats {
                total_pages: 0,
                reserved_pages: 0,
                free_pages: 0,
                allocated_pages: 0,
            },
            search_hint: 0,
            initialized: false,
        }
    }

    /// RAM must be sorted and non-overlapping. Align RAM inward and reservations
    /// outward: even one reserved byte makes the entire containing page unusable.
    /// Capacity failure leaves the allocator uninitialized, never truncated.
    pub fn init(&mut self, ram: &[Region], reserved: &[Region]) -> Result<(), PageError> {
        if self.initialized {
            return Err(PageError::AlreadyInitialized);
        }
        if ram.len() > MAX_BANKS {
            return Err(PageError::TooManyRegions);
        }
        let capacity = WORDS.checked_mul(64).ok_or(PageError::CapacityExceeded)?;
        let mut total_pages = 0usize;
        let mut bank_count = 0usize;
        let mut previous_end = 0usize;
        for (position, region) in ram.iter().enumerate() {
            if region.start >= region.end || (position != 0 && region.start < previous_end) {
                return Err(PageError::InvalidRegion);
            }
            previous_end = region.end;
            let start = align_up(region.start, PAGE_SIZE).ok_or(PageError::AddressOverflow)?;
            let end = region.end & !(PAGE_SIZE - 1);
            if start >= end {
                continue;
            }
            let count = (end - start) / PAGE_SIZE;
            let next_total = total_pages
                .checked_add(count)
                .ok_or(PageError::CapacityExceeded)?;
            if next_total > capacity {
                return Err(PageError::CapacityExceeded);
            }
            // Adjacent complete banks are physically contiguous and may support
            // one allocation; a genuine hole must remain a bank boundary.
            if bank_count != 0 && self.banks[bank_count - 1].end == start {
                self.banks[bank_count - 1].end = end;
            } else {
                self.banks[bank_count] = PageBank {
                    start,
                    end,
                    first_index: total_pages,
                };
                bank_count += 1;
            }
            total_pages = next_total;
        }
        if total_pages == 0 {
            return Err(PageError::NoUsableRam);
        }
        if reserved.iter().any(|region| region.start >= region.end) {
            return Err(PageError::InvalidRegion);
        }

        // Fill in place; no bitmap-sized temporary is created on the boot stack.
        self.allowed.fill(0);
        self.allocated.fill(0);
        self.starts.fill(0);
        for word in &mut self.allowed[..total_pages / 64] {
            *word = u64::MAX;
        }
        if total_pages % 64 != 0 {
            self.allowed[total_pages / 64] = (1u64 << (total_pages % 64)) - 1;
        }
        let mut reserved_pages = 0usize;
        for bank in &self.banks[..bank_count] {
            for region in reserved {
                let start = region.start.max(bank.start);
                let end = region.end.min(bank.end);
                if start >= end {
                    continue;
                }
                let first = bank.first_index + (start - bank.start) / PAGE_SIZE;
                let relative_end = end - bank.start;
                let last = bank.first_index
                    + relative_end / PAGE_SIZE
                    + usize::from(relative_end % PAGE_SIZE != 0);
                for index in first..last {
                    if bit(&self.allowed, index) {
                        set_bit(&mut self.allowed, index, false);
                        reserved_pages += 1;
                    }
                }
            }
        }
        self.bank_count = bank_count;
        self.stats = PageStats {
            total_pages,
            reserved_pages,
            free_pages: total_pages - reserved_pages,
            allocated_pages: 0,
        };
        self.search_hint = 0;
        self.initialized = true;
        Ok(())
    }

    pub fn stats(&self) -> PageStats {
        self.stats
    }

    /// Allocate a contiguous run aligned to `alignment_pages` physical pages.
    /// The alignment must be a nonzero power of two. A run cannot cross a RAM
    /// hole or include any reserved page, even if compact bitmap indexes adjoin.
    pub fn alloc_pages(
        &mut self,
        count: usize,
        alignment_pages: usize,
    ) -> Result<PhysicalPages, PageError> {
        if !self.initialized {
            return Err(PageError::NotInitialized);
        }
        if count == 0 {
            return Err(PageError::InvalidCount);
        }
        if !alignment_pages.is_power_of_two() || alignment_pages.checked_mul(PAGE_SIZE).is_none() {
            return Err(PageError::InvalidAlignment);
        }
        if count > self.stats.free_pages {
            return Err(PageError::OutOfMemory);
        }

        // Start near the previous allocation, then wrap. The second pass scans
        // the entire bank containing the hint so a valid run straddling that
        // hint is not mistakenly reported as exhaustion.
        let mut found = None;
        for pass in 0..2 {
            for bank in &self.banks[..self.bank_count] {
                let first = if pass == 0 {
                    bank.first_index.max(self.search_hint)
                } else {
                    if bank.first_index >= self.search_hint {
                        continue;
                    }
                    bank.first_index
                };
                if let Some(index) = self.find_run(*bank, first, count, alignment_pages) {
                    found = Some((*bank, index));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (bank, first) = found.ok_or(PageError::OutOfMemory)?;
        for index in first..first + count {
            set_bit(&mut self.allocated, index, true);
        }
        set_bit(&mut self.starts, first, true);
        self.search_hint = first + count;
        self.stats.free_pages -= count;
        self.stats.allocated_pages += count;
        Ok(PhysicalPages {
            start: bank.start + (first - bank.first_index) * PAGE_SIZE,
            count,
            owner: self as *const Self as usize,
        })
    }

    /// Consume an exact allocation token. Validate the entire run before
    /// changing any bitmap, including its start and end boundaries.
    pub fn free_pages(&mut self, pages: PhysicalPages) -> Result<(), PageError> {
        if !self.initialized {
            return Err(PageError::NotInitialized);
        }
        if pages.owner != self as *const Self as usize {
            return Err(PageError::WrongAllocator);
        }
        if pages.count == 0 {
            return Err(PageError::InvalidCount);
        }
        if pages.start % PAGE_SIZE != 0 {
            return Err(PageError::InvalidAlignment);
        }
        let bank = self.banks[..self.bank_count]
            .iter()
            .find(|bank| pages.start >= bank.start && pages.start < bank.end)
            .copied()
            .ok_or(PageError::OutsideRam)?;
        let first = bank.first_index + (pages.start - bank.start) / PAGE_SIZE;
        let last = first
            .checked_add(pages.count)
            .filter(|last| *last <= bank.end_index())
            .ok_or(PageError::OutsideRam)?;
        for index in first..last {
            if !bit(&self.allowed, index) {
                return Err(PageError::ReservedPage);
            }
            if !bit(&self.allocated, index) {
                return Err(PageError::DoubleFree);
            }
            if (index == first) != bit(&self.starts, index) {
                return Err(PageError::InvalidAllocation);
            }
        }
        if last < bank.end_index() && bit(&self.allocated, last) && !bit(&self.starts, last) {
            return Err(PageError::InvalidAllocation);
        }
        for index in first..last {
            set_bit(&mut self.allocated, index, false);
        }
        set_bit(&mut self.starts, first, false);
        self.search_hint = self.search_hint.min(first);
        self.stats.free_pages += pages.count;
        self.stats.allocated_pages -= pages.count;
        Ok(())
    }

    fn find_run(
        &self,
        bank: PageBank,
        first: usize,
        count: usize,
        alignment: usize,
    ) -> Option<usize> {
        if first >= bank.end_index() {
            return None;
        }
        let physical_first = bank.start / PAGE_SIZE + first - bank.first_index;
        let mut candidate = align_up(physical_first, alignment)?;
        let physical_end = bank.end / PAGE_SIZE;
        while candidate.checked_add(count)? <= physical_end {
            let index = bank.first_index + candidate - bank.start / PAGE_SIZE;
            let mut unavailable = None;
            for offset in 0..count {
                if !bit(&self.allowed, index + offset) || bit(&self.allocated, index + offset) {
                    unavailable = Some(offset);
                    break;
                }
            }
            match unavailable {
                None => return Some(index),
                Some(offset) => candidate = align_up(candidate + offset + 1, alignment)?,
            }
        }
        None
    }
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
}

fn bit<const WORDS: usize>(bitmap: &[u64; WORDS], index: usize) -> bool {
    bitmap[index / 64] & (1u64 << (index % 64)) != 0
}

fn set_bit<const WORDS: usize>(bitmap: &mut [u64; WORDS], index: usize, value: bool) {
    let mask = 1u64 << (index % 64);
    if value {
        bitmap[index / 64] |= mask;
    } else {
        bitmap[index / 64] &= !mask;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start: usize, end: usize) -> Region {
        Region { start, end }
    }

    fn token(allocator: &PageAllocator<1>, start: usize, count: usize) -> PhysicalPages {
        PhysicalPages {
            start,
            count,
            owner: allocator as *const PageAllocator<1> as usize,
        }
    }

    #[test]
    fn reservations_round_outward_and_ram_rounds_inward() {
        let mut allocator = PageAllocator::<1>::new();
        allocator
            .init(
                &[region(1, PAGE_SIZE * 9 - 1)],
                &[
                    region(PAGE_SIZE + 1, PAGE_SIZE + 2),
                    region(PAGE_SIZE + 2, PAGE_SIZE * 3 + 1),
                    region(PAGE_SIZE * 30, PAGE_SIZE * 31),
                ],
            )
            .unwrap();
        assert_eq!(
            allocator.stats(),
            PageStats {
                total_pages: 7,
                reserved_pages: 3,
                free_pages: 4,
                allocated_pages: 0,
            }
        );
        let pages = allocator.alloc_pages(4, 1).unwrap();
        assert_eq!(pages.start_address(), PAGE_SIZE * 4);
        assert_eq!(pages.page_count(), 4);
        assert_eq!(pages.byte_len(), PAGE_SIZE * 4);
        assert_eq!(
            allocator.alloc_pages(1, 1).unwrap_err(),
            PageError::OutOfMemory
        );
        allocator.free_pages(pages).unwrap();
        assert_eq!(allocator.stats().free_pages, 4);
    }

    #[test]
    fn compact_high_banks_never_allocate_across_holes() {
        let high = 0x1_0000_0000usize;
        let mut allocator = PageAllocator::<1>::new();
        allocator
            .init(
                &[region(0, PAGE_SIZE * 2), region(high, high + PAGE_SIZE * 3)],
                &[],
            )
            .unwrap();
        assert_eq!(allocator.stats().total_pages, 5);
        assert_eq!(
            allocator.alloc_pages(4, 1).unwrap_err(),
            PageError::OutOfMemory
        );
        let pages = allocator.alloc_pages(3, 1).unwrap();
        assert_eq!(pages.start_address(), high);
        allocator.free_pages(pages).unwrap();
    }

    #[test]
    fn alignment_uses_physical_address_and_fragmentation_reuses_freed_pages() {
        let mut allocator = PageAllocator::<1>::new();
        allocator
            .init(&[region(PAGE_SIZE, PAGE_SIZE * 17)], &[])
            .unwrap();
        let first = allocator.alloc_pages(2, 4).unwrap();
        let second = allocator.alloc_pages(2, 4).unwrap();
        assert_eq!(first.start_address(), PAGE_SIZE * 4);
        assert_eq!(second.start_address(), PAGE_SIZE * 8);
        allocator.free_pages(first).unwrap();
        let reused = allocator.alloc_pages(2, 4).unwrap();
        assert_eq!(reused.start_address(), PAGE_SIZE * 4);
        allocator.free_pages(reused).unwrap();
        allocator.free_pages(second).unwrap();
        assert_eq!(allocator.stats().allocated_pages, 0);
    }

    #[test]
    fn exact_run_free_rejects_partial_merged_reserved_and_double_free() {
        let mut allocator = PageAllocator::<1>::new();
        allocator
            .init(&[region(0, PAGE_SIZE * 10)], &[region(0, 1)])
            .unwrap();
        let first = allocator.alloc_pages(3, 1).unwrap();
        let second = allocator.alloc_pages(2, 1).unwrap();
        assert_eq!(
            allocator.free_pages(token(&allocator, PAGE_SIZE, 2)),
            Err(PageError::InvalidAllocation)
        );
        assert_eq!(
            allocator.free_pages(token(&allocator, PAGE_SIZE * 2, 2)),
            Err(PageError::InvalidAllocation)
        );
        assert_eq!(
            allocator.free_pages(token(&allocator, PAGE_SIZE, 5)),
            Err(PageError::InvalidAllocation)
        );
        assert_eq!(
            allocator.free_pages(token(&allocator, 0, 1)),
            Err(PageError::ReservedPage)
        );
        assert_eq!(allocator.stats().allocated_pages, 5);
        allocator.free_pages(first).unwrap();
        assert_eq!(
            allocator.free_pages(token(&allocator, PAGE_SIZE, 3)),
            Err(PageError::DoubleFree)
        );
        allocator.free_pages(second).unwrap();
    }

    #[test]
    fn cross_allocator_free_is_rejected_without_mutation() {
        let mut first = PageAllocator::<1>::new();
        let mut second = PageAllocator::<1>::new();
        first.init(&[region(0, PAGE_SIZE * 4)], &[]).unwrap();
        second.init(&[region(0, PAGE_SIZE * 4)], &[]).unwrap();
        let first_token = first.alloc_pages(1, 1).unwrap();
        let second_token = second.alloc_pages(1, 1).unwrap();
        assert_eq!(
            second.free_pages(first_token),
            Err(PageError::WrongAllocator)
        );
        assert_eq!(second.stats().allocated_pages, 1);
        second.free_pages(second_token).unwrap();
    }

    #[test]
    fn explicit_capacity_failure_and_invalid_inputs() {
        let mut allocator = PageAllocator::<1>::new();
        assert_eq!(
            allocator.alloc_pages(1, 1).unwrap_err(),
            PageError::NotInitialized
        );
        assert_eq!(
            allocator.init(&[region(0, PAGE_SIZE * 65)], &[]),
            Err(PageError::CapacityExceeded)
        );
        assert_eq!(
            allocator.init(
                &[region(PAGE_SIZE * 2, PAGE_SIZE * 4), region(0, PAGE_SIZE)],
                &[]
            ),
            Err(PageError::InvalidRegion)
        );
        allocator.init(&[region(0, PAGE_SIZE * 64)], &[]).unwrap();
        assert_eq!(
            allocator.alloc_pages(0, 1).unwrap_err(),
            PageError::InvalidCount
        );
        assert_eq!(
            allocator.alloc_pages(1, 0).unwrap_err(),
            PageError::InvalidAlignment
        );
        assert_eq!(
            allocator.alloc_pages(1, 3).unwrap_err(),
            PageError::InvalidAlignment
        );
        assert_eq!(
            allocator.init(&[region(0, PAGE_SIZE)], &[]),
            Err(PageError::AlreadyInitialized)
        );
    }

    #[test]
    fn adjacent_banks_merge_but_small_holes_still_exclude_partial_pages() {
        let mut allocator = PageAllocator::<1>::new();
        allocator
            .init(
                &[
                    region(0, PAGE_SIZE * 2),
                    region(PAGE_SIZE * 2, PAGE_SIZE * 4),
                    region(PAGE_SIZE * 4 + 1, PAGE_SIZE * 7),
                ],
                &[],
            )
            .unwrap();
        let pages = allocator.alloc_pages(4, 1).unwrap();
        assert_eq!(pages.start_address(), 0);
        assert_eq!(allocator.stats().free_pages, 2);
        allocator.free_pages(pages).unwrap();
        assert_eq!(
            allocator.alloc_pages(5, 1).unwrap_err(),
            PageError::OutOfMemory
        );
    }

    #[test]
    fn word_boundaries_and_full_bitmap_capacity_are_counted_exactly() {
        let mut allocator = PageAllocator::<2>::new();
        allocator
            .init(
                &[region(0, PAGE_SIZE * 128)],
                &[region(0, 1), region(PAGE_SIZE * 63 + 1, PAGE_SIZE * 64 + 1)],
            )
            .unwrap();
        let first = allocator.alloc_pages(62, 1).unwrap();
        let second = allocator.alloc_pages(63, 1).unwrap();
        assert_eq!(first.start_address(), PAGE_SIZE);
        assert_eq!(second.start_address(), PAGE_SIZE * 65);
        assert_eq!(allocator.stats().free_pages, 0);
        assert_eq!(allocator.stats().reserved_pages, 3);
        allocator.free_pages(first).unwrap();
        allocator.free_pages(second).unwrap();
        assert_eq!(allocator.stats().free_pages, 125);
    }

    #[test]
    fn fragmented_allocations_match_a_small_independent_page_model() {
        let high = 0x1_0000_0000usize;
        let mut allocator = PageAllocator::<2>::new();
        allocator
            .init(
                &[
                    region(0, PAGE_SIZE * 96),
                    region(high, high + PAGE_SIZE * 32),
                ],
                &[
                    region(0, PAGE_SIZE * 3),
                    region(PAGE_SIZE * 45, PAGE_SIZE * 48),
                    region(high + PAGE_SIZE * 15, high + PAGE_SIZE * 17),
                ],
            )
            .unwrap();
        let mut unavailable = [false; 128];
        unavailable[..3].fill(true);
        unavailable[45..48].fill(true);
        unavailable[111..113].fill(true);
        let mut live: [Option<PhysicalPages>; 24] = core::array::from_fn(|_| None);
        let mut random = 0x1357_2468u32;
        for _ in 0..2000 {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            let slot = random as usize % live.len();
            if let Some(pages) = live[slot].take() {
                let index = if pages.start_address() < high {
                    pages.start_address() / PAGE_SIZE
                } else {
                    96 + (pages.start_address() - high) / PAGE_SIZE
                };
                unavailable[index..index + pages.page_count()].fill(false);
                allocator.free_pages(pages).unwrap();
            } else {
                let count = (random >> 8) as usize % 7 + 1;
                let alignment = 1usize << ((random >> 16) % 4);
                let exists = [(0usize, 96usize, 0usize), (96, 128, high / PAGE_SIZE)]
                    .iter()
                    .any(|&(first, end, physical_base)| {
                        (first..end).any(|index| {
                            index + count <= end
                                && (physical_base + index - first) % alignment == 0
                                && unavailable[index..index + count]
                                    .iter()
                                    .all(|unavailable| !unavailable)
                        })
                    });
                match allocator.alloc_pages(count, alignment) {
                    Ok(pages) => {
                        assert!(exists);
                        assert_eq!(pages.start_address() / PAGE_SIZE % alignment, 0);
                        let index = if pages.start_address() < high {
                            pages.start_address() / PAGE_SIZE
                        } else {
                            96 + (pages.start_address() - high) / PAGE_SIZE
                        };
                        assert!(unavailable[index..index + count].iter().all(|bit| !bit));
                        unavailable[index..index + count].fill(true);
                        live[slot] = Some(pages);
                    }
                    Err(error) => {
                        assert_eq!(error, PageError::OutOfMemory);
                        assert!(!exists);
                    }
                }
            }
            let allocated = live
                .iter()
                .filter_map(Option::as_ref)
                .map(PhysicalPages::page_count)
                .sum::<usize>();
            assert_eq!(allocator.stats().allocated_pages, allocated);
            assert_eq!(allocator.stats().free_pages + allocated, 120);
        }
        for pages in live.into_iter().flatten() {
            allocator.free_pages(pages).unwrap();
        }
        assert_eq!(allocator.stats().free_pages, 120);
    }
}
