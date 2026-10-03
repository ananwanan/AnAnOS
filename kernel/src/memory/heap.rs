//! Reusable first-fit heap backed by an exclusively owned physical page run.
//!
//! Free-list nodes live in free memory. Every block boundary is 16-byte aligned,
//! so splitting never leaves a fragment too small for a node.

use core::alloc::Layout;
use core::ptr;

const BLOCK_ALIGN: usize = 16;

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

pub struct Heap {
    first: *mut FreeBlock,
    start: usize,
    end: usize,
    free_bytes: usize,
}

impl Heap {
    pub const fn new() -> Self {
        Self {
            first: ptr::null_mut(),
            start: 0,
            end: 0,
            free_bytes: 0,
        }
    }

    /// # Safety
    /// The span must be exclusively owned writable RAM, valid for the lifetime
    /// of the heap, with no outstanding allocations. It must not move.
    pub unsafe fn init(&mut self, start: usize, size: usize) -> Result<(), &'static str> {
        if self.end != 0 {
            return Err("heap already initialized");
        }
        let end = start.checked_add(size).ok_or("heap address overflow")?;
        if start == 0 || start % BLOCK_ALIGN != 0 || size < BLOCK_ALIGN || size % BLOCK_ALIGN != 0 {
            return Err("invalid heap span");
        }
        self.start = start;
        self.end = end;
        self.first = start as *mut FreeBlock;
        self.free_bytes = size;
        // SAFETY: the caller provided exclusive writable, aligned RAM.
        unsafe {
            self.first.write(FreeBlock {
                size,
                next: ptr::null_mut(),
            });
        }
        Ok(())
    }

    pub fn free_bytes(&self) -> usize {
        self.free_bytes
    }

    /// # Safety
    /// All accesses to this heap must be serialized; its backing RAM must
    /// remain writable and exclusively owned as required by `init`.
    pub unsafe fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let Some((size, alignment)) = normalized(layout) else {
            return ptr::null_mut();
        };
        let mut link = &raw mut self.first;
        // SAFETY: links point only to the heap head or live free-list nodes.
        unsafe {
            while !(*link).is_null() {
                let block = *link;
                let start = block as usize;
                let block_size = (*block).size;
                let next = (*block).next;
                let Some(address) = align_up(start, alignment) else {
                    return ptr::null_mut();
                };
                let prefix = address - start;
                if prefix > block_size || size > block_size - prefix {
                    link = &raw mut (*block).next;
                    continue;
                }
                let suffix_size = block_size - prefix - size;
                let suffix = if suffix_size != 0 {
                    let suffix = (address + size) as *mut FreeBlock;
                    suffix.write(FreeBlock {
                        size: suffix_size,
                        next,
                    });
                    suffix
                } else {
                    next
                };
                if prefix != 0 {
                    (*block).size = prefix;
                    (*block).next = suffix;
                } else {
                    *link = suffix;
                }
                self.free_bytes -= size;
                return address as *mut u8;
            }
        }
        ptr::null_mut()
    }

    /// # Safety
    /// `pointer` must be a live allocation made by this heap with exactly
    /// `layout`; it may be freed once, after the last access to its contents.
    /// All accesses to the heap must be serialized.
    pub unsafe fn deallocate(&mut self, pointer: *mut u8, layout: Layout) {
        let Some((size, _)) = normalized(layout) else {
            return;
        };
        let address = pointer as usize;
        // Defensive bounds checks do not replace the caller's ownership contract.
        if address < self.start
            || address % BLOCK_ALIGN != 0
            || size > self.end.saturating_sub(address)
        {
            return;
        }
        let mut previous: *mut FreeBlock = ptr::null_mut();
        let mut link = &raw mut self.first;
        // SAFETY: caller returns a valid live span; list nodes are in free RAM.
        unsafe {
            while !(*link).is_null() && (*link as usize) < address {
                previous = *link;
                link = &raw mut (**link).next;
            }
            let next = *link;
            let block = pointer.cast::<FreeBlock>();
            block.write(FreeBlock { size, next });
            *link = block;
            if !next.is_null() && address + size == next as usize {
                (*block).size += (*next).size;
                (*block).next = (*next).next;
            }
            if !previous.is_null() && previous as usize + (*previous).size == address {
                (*previous).size += (*block).size;
                (*previous).next = (*block).next;
            }
        }
        self.free_bytes += size;
    }
}

fn normalized(layout: Layout) -> Option<(usize, usize)> {
    let size = align_up(layout.size().max(BLOCK_ALIGN), BLOCK_ALIGN)?;
    Some((size, layout.align().max(BLOCK_ALIGN)))
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;

    #[repr(C, align(4096))]
    struct Backing([u8; 16 * 4096]);

    fn fixture() -> (Box<Backing>, Heap) {
        let mut backing = Box::new(Backing([0; 16 * 4096]));
        let mut heap = Heap::new();
        unsafe {
            heap.init(backing.0.as_mut_ptr() as usize, backing.0.len())
                .unwrap()
        };
        (backing, heap)
    }

    #[test]
    fn alignment_non_overlap_and_coalescing() {
        let (_backing, mut heap) = fixture();
        let capacity = heap.free_bytes();
        let small = Layout::from_size_align(17, 8).unwrap();
        let aligned = Layout::from_size_align(100, 4096).unwrap();
        unsafe {
            let a = heap.allocate(small);
            let b = heap.allocate(aligned);
            let c = heap.allocate(small);
            assert!(!a.is_null() && !b.is_null() && !c.is_null());
            assert_eq!(b as usize % 4096, 0);
            a.write_bytes(0x51, small.size());
            b.write_bytes(0xA2, aligned.size());
            c.write_bytes(0x73, small.size());
            assert_eq!(*a, 0x51);
            assert_eq!(*b, 0xA2);
            assert_eq!(*c, 0x73);
            heap.deallocate(b, aligned);
            heap.deallocate(a, small);
            heap.deallocate(c, small);
            assert_eq!(heap.free_bytes(), capacity);
            let all = Layout::from_size_align(capacity, 16).unwrap();
            let run = heap.allocate(all);
            assert_eq!(run as usize, heap.start);
            assert!(heap.allocate(small).is_null());
            heap.deallocate(run, all);
            assert_eq!(heap.free_bytes(), capacity);
        }
    }

    #[test]
    fn fragmented_heap_reuses_freed_blocks() {
        let (_backing, mut heap) = fixture();
        let quarter = Layout::from_size_align(4 * 4096, 16).unwrap();
        let half = Layout::from_size_align(8 * 4096, 16).unwrap();
        unsafe {
            let a = heap.allocate(quarter);
            let b = heap.allocate(quarter);
            let c = heap.allocate(quarter);
            let d = heap.allocate(quarter);
            heap.deallocate(b, quarter);
            heap.deallocate(d, quarter);
            assert!(heap.allocate(half).is_null());
            assert_eq!(heap.allocate(quarter), b);
            heap.deallocate(c, quarter);
            assert!(!heap.allocate(half).is_null());
            assert!(!a.is_null());
        }
    }

    #[test]
    fn empty_heap_and_oversized_request_return_null() {
        let mut heap = Heap::new();
        let huge = Layout::from_size_align(isize::MAX as usize, 1).unwrap();
        assert!(unsafe { heap.allocate(huge) }.is_null());
        let (_backing, mut heap) = fixture();
        assert!(unsafe { heap.allocate(huge) }.is_null());
        assert_eq!(heap.free_bytes(), 16 * 4096);
    }

    #[test]
    fn randomized_splits_preserve_live_payloads_and_fully_coalesce() {
        let (backing, mut heap) = fixture();
        let start = backing.0.as_ptr() as usize;
        let end = start + backing.0.len();
        let capacity = heap.free_bytes();
        let mut slots: [Option<(*mut u8, Layout, u8)>; 64] = [None; 64];
        let mut random = 0x2468_1357u32;

        for _ in 0..10_000 {
            // Deterministic input makes any failed fragmentation sequence
            // reproducible without an external RNG or test dependency.
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            let slot = random as usize % slots.len();
            if let Some((pointer, layout, pattern)) = slots[slot].take() {
                unsafe {
                    assert!(
                        core::slice::from_raw_parts(pointer, layout.size())
                            .iter()
                            .all(|byte| *byte == pattern)
                    );
                    heap.deallocate(pointer, layout);
                }
            } else {
                let size = (random >> 6) as usize % 3000 + 1;
                let alignment = 1usize << ((random >> 18) % 13);
                let layout = Layout::from_size_align(size, alignment).unwrap();
                let pointer = unsafe { heap.allocate(layout) };
                if !pointer.is_null() {
                    let address = pointer as usize;
                    assert_eq!(address % alignment, 0);
                    assert!(address >= start && address + size <= end);
                    let occupied = (size.max(16) + 15) & !15;
                    for &(other, other_layout, _) in slots.iter().flatten() {
                        let other_start = other as usize;
                        let other_occupied = (other_layout.size().max(16) + 15) & !15;
                        assert!(
                            address + occupied <= other_start
                                || other_start + other_occupied <= address
                        );
                    }
                    let pattern = random as u8;
                    unsafe { pointer.write_bytes(pattern, size) };
                    slots[slot] = Some((pointer, layout, pattern));
                }
            }

            // Validate every byte of every live allocation after both success
            // and exhaustion, including free-list writes during coalescing.
            let mut occupied = 0;
            for &(pointer, layout, pattern) in slots.iter().flatten() {
                unsafe {
                    assert!(
                        core::slice::from_raw_parts(pointer, layout.size())
                            .iter()
                            .all(|byte| *byte == pattern)
                    );
                }
                occupied += (layout.size().max(16) + 15) & !15;
            }
            assert_eq!(heap.free_bytes() + occupied, capacity);
        }

        for (pointer, layout, _) in slots.into_iter().flatten() {
            unsafe { heap.deallocate(pointer, layout) };
        }
        assert_eq!(heap.free_bytes(), capacity);
        // Recovering a single complete span proves that adjacent fragments
        // coalesce, rather than merely restoring the free-byte counter.
        let all = Layout::from_size_align(capacity, 16).unwrap();
        let pointer = unsafe { heap.allocate(all) };
        assert_eq!(pointer as usize, start);
        unsafe { heap.deallocate(pointer, all) };
        assert_eq!(heap.free_bytes(), capacity);
    }
}
