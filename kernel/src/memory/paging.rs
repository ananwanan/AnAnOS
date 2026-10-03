//! Offline EL1 stage-1 translation tables for the Cortex-A72 bootstrap.
//!
//! A 4 KiB granule and T0SZ=25 use one level-1 root (39-bit TTBR0 addresses).
//! Table/page output addresses are bounded to 40 bits, matching the bootstrap
//! TCR_EL1.IPS setting. AttrIdx 0 must be Normal Non-cacheable and AttrIdx 1
//! Device-nGnRnE in MAIR_EL1. Kernel mappings deny EL0 access and execution;
//! explicit user mappings are Normal Non-cacheable, non-global and EL1 PXN.
//!
//! These operations are exclusively for tables that no CPU is currently using.
//! Replacing live descriptors requires architectural break-before-make, barriers
//! and TLB invalidation, which this allocation-free module does not implement.
//! The descriptor definitions follow Arm 100940_0101, sections 4 and 7.

pub const PAGE_SIZE: u64 = 4096;
pub const VIRTUAL_ADDRESS_LIMIT: u64 = 1 << 39;
pub const PHYSICAL_ADDRESS_LIMIT: u64 = 1 << 40;
const ENTRY_COUNT: usize = 512;
const ADDRESS_MASK: u64 = 0x0000_00ff_ffff_f000;
const ARCH_ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;
const VALID: u64 = 1;
const TABLE_OR_PAGE: u64 = 1 << 1;
const ATTR_INDEX_MASK: u64 = 0b111 << 2;
const AP_READ_ONLY: u64 = 0b10 << 6;
const AP_USER_ACCESS: u64 = 1 << 6;
const AP_MASK: u64 = 0b11 << 6;
const SHAREABLE_OUTER: u64 = 0b10 << 8;
const SHAREABLE_INNER: u64 = 0b11 << 8;
const ACCESS_FLAG: u64 = 1 << 10;
const NOT_GLOBAL: u64 = 1 << 11;
const PRIVILEGED_EXECUTE_NEVER: u64 = 1 << 53;
const USER_EXECUTE_NEVER: u64 = 1 << 54;
// Mixed kernel/user trees leave hierarchical permissions unrestricted. Leaf
// AP/PXN/UXN bits are authoritative; kernel leaves still deny every EL0 access.
const TABLE_USER_EXECUTE_NEVER: u64 = 1 << 60;
const TABLE_NO_EL0_ACCESS: u64 = 1 << 61;
const TABLE_READ_ONLY: u64 = 1 << 62;
const TABLE_PRIVILEGED_EXECUTE_NEVER: u64 = 1 << 59;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryType {
    NormalNonCacheable,
    Device,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappingAttrs {
    pub memory: MemoryType,
    pub writable: bool,
    pub executable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingError {
    Unaligned,
    InvalidRange,
    WritableExecutable,
    InvalidAttributes,
    OutOfMemory,
    InvalidTableAddress,
    AlreadyMapped,
    UnsupportedDescriptor,
}

/// Caller-owned storage for table pages. Addresses are physical, 4 KiB aligned
/// and below the 40-bit PA limit. Read/write must address a live owned page.
///
/// Table allocation may fail; the mapper initializes every entry before linking
/// the new table into its parent. Implementations need not return cleared pages.
/// A storage implementation that cannot free individual pages may leave
/// `release_table` as a no-op and reclaim its complete arena after construction.
/// Calls are CPU-local; the owner must exclude concurrent access and IRQs.
pub trait TableMemory {
    fn allocate_table(&mut self) -> Result<u64, PagingError>;
    fn read_entry(&self, table: u64, index: usize) -> u64;
    fn write_entry(&mut self, table: u64, index: usize, entry: u64);
    fn release_table(&mut self, _table: u64) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    /// Includes the byte offset into the block or page.
    pub physical_address: u64,
    pub attrs: MappingAttrs,
    /// True only for leaves with AP[0] set. Kernel/MMIO mappings stay private.
    pub user_accessible: bool,
    /// EL0 execution permission, separate from `attrs.executable` (EL1).
    pub user_executable: bool,
    /// 1: 1 GiB block, 2: 2 MiB block, 3: 4 KiB page.
    pub level: u8,
}

/// Owns the root address, while the caller retains ownership of backing pages.
/// There is no implicit Drop: use `destroy` or reclaim the complete table store.
#[derive(Debug)]
pub struct PageTables {
    root: u64,
}

#[derive(Clone, Copy)]
struct Parent {
    table: u64,
    index: usize,
    child: u64,
}

impl Parent {
    const EMPTY: Self = Self {
        table: 0,
        index: 0,
        child: 0,
    };
}

struct Walk {
    table: u64,
    index: usize,
    entry: u64,
    level: u8,
    parents: [Parent; 2],
    parent_count: usize,
}

impl PageTables {
    pub fn new(memory: &mut impl TableMemory) -> Result<Self, PagingError> {
        Ok(Self {
            root: allocate_table(memory)?,
        })
    }

    pub fn root_address(&self) -> u64 {
        self.root
    }

    /// Map a disjoint, page-aligned range. Prefer 1 GiB/2 MiB blocks where both
    /// addresses and the remaining length allow it, otherwise use 4 KiB pages.
    /// Existing mappings are never replaced, even when they are identical.
    ///
    /// Validation and collision checks precede any change. Allocation failure
    /// can leave a mapped prefix and empty intermediate tables; destroy/reclaim
    /// the offline store rather than installing a partially constructed map.
    /// Physical aliases are permitted; callers must use consistent memory
    /// attributes for every alias and define ownership outside this module.
    pub fn map_range(
        &mut self,
        memory: &mut impl TableMemory,
        virtual_address: u64,
        physical_address: u64,
        size: u64,
        attrs: MappingAttrs,
    ) -> Result<(), PagingError> {
        self.map_range_with_access(
            memory,
            virtual_address,
            physical_address,
            size,
            attrs,
            false,
        )
    }

    /// Add an explicit EL0 mapping to an offline address space. User memory is
    /// Normal Non-cacheable; AP allows EL0, PXN prevents EL1 execution and nG
    /// prevents a process mapping from becoming a global TLB entry. Writable
    /// executable pages are rejected. Data-page ownership remains with callers.
    pub fn map_user_range(
        &mut self,
        memory: &mut impl TableMemory,
        virtual_address: u64,
        physical_address: u64,
        size: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), PagingError> {
        self.map_range_with_access(
            memory,
            virtual_address,
            physical_address,
            size,
            MappingAttrs {
                memory: MemoryType::NormalNonCacheable,
                writable,
                executable,
            },
            true,
        )
    }

    fn map_range_with_access(
        &mut self,
        memory: &mut impl TableMemory,
        virtual_address: u64,
        physical_address: u64,
        size: u64,
        attrs: MappingAttrs,
        user: bool,
    ) -> Result<(), PagingError> {
        let end = validate_virtual_range(virtual_address, size)?;
        if physical_address % PAGE_SIZE != 0 {
            return Err(PagingError::Unaligned);
        }
        if physical_address
            .checked_add(size)
            .filter(|end| *end <= PHYSICAL_ADDRESS_LIMIT)
            .is_none()
        {
            return Err(PagingError::InvalidRange);
        }
        if attrs.writable && attrs.executable {
            return Err(PagingError::WritableExecutable);
        }
        if attrs.memory == MemoryType::Device && attrs.executable {
            return Err(PagingError::InvalidAttributes);
        }

        let mut cursor = virtual_address;
        while cursor < end {
            let walk = self.walk(memory, cursor)?;
            if walk.entry & VALID != 0 {
                return Err(PagingError::AlreadyMapped);
            }
            cursor = next_boundary(cursor, walk.level).min(end);
        }

        let mut cursor = virtual_address;
        let mut physical = physical_address;
        while cursor < end {
            let remaining = end - cursor;
            let preferred_level = if aligned_for(cursor, physical, 1) && remaining >= level_size(1)
            {
                1
            } else if aligned_for(cursor, physical, 2) && remaining >= level_size(2) {
                2
            } else {
                3
            };
            let mapped = self.map_entry(memory, cursor, physical, preferred_level, attrs, user)?;
            cursor += mapped;
            physical += mapped;
        }
        Ok(())
    }

    pub fn lookup(&self, memory: &impl TableMemory, virtual_address: u64) -> Option<Translation> {
        if virtual_address >= VIRTUAL_ADDRESS_LIMIT {
            return None;
        }
        let walk = self.walk(memory, virtual_address).ok()?;
        if walk.entry & VALID == 0 {
            return None;
        }
        let attrs = decode_attrs(walk.entry)?;
        Some(Translation {
            physical_address: (walk.entry & ADDRESS_MASK)
                + (virtual_address & (level_size(walk.level) - 1)),
            attrs,
            user_accessible: walk.entry & AP_USER_ACCESS != 0,
            user_executable: walk.entry & (AP_USER_ACCESS | USER_EXECUTE_NEVER) == AP_USER_ACCESS,
            level: walk.level,
        })
    }

    /// Remove mappings from an offline table. Holes are allowed. Removing part
    /// of a block splits it without changing the unaffected translations or
    /// attributes; empty child tables are pruned through `release_table`.
    /// Allocation failure during splitting can leave an unmapped prefix.
    pub fn unmap_range(
        &mut self,
        memory: &mut impl TableMemory,
        virtual_address: u64,
        size: u64,
    ) -> Result<(), PagingError> {
        let end = validate_virtual_range(virtual_address, size)?;
        let mut cursor = virtual_address;
        while cursor < end {
            let walk = self.walk(memory, cursor)?;
            let boundary = next_boundary(cursor, walk.level);
            if walk.entry & VALID == 0 {
                cursor = boundary.min(end);
                self.prune(memory, &walk);
            } else if cursor % level_size(walk.level) == 0 && boundary <= end {
                memory.write_entry(walk.table, walk.index, 0);
                self.prune(memory, &walk);
                cursor = boundary;
            } else {
                // A page-aligned range can only partially cover an L1/L2 block.
                self.split_block(memory, &walk)?;
            }
        }
        Ok(())
    }

    /// Release every table page, including the root. No mapped data page is
    /// owned or freed by this operation. The table must remain offline.
    pub fn destroy(self, memory: &mut impl TableMemory) {
        release_tree(memory, self.root, 1);
    }

    fn walk(&self, memory: &impl TableMemory, virtual_address: u64) -> Result<Walk, PagingError> {
        let mut table = self.root;
        let mut parents = [Parent::EMPTY; 2];
        let mut parent_count = 0;
        for level in 1..=3 {
            let index = level_index(virtual_address, level);
            let entry = memory.read_entry(table, index);
            if entry & VALID == 0 {
                return Ok(Walk {
                    table,
                    index,
                    entry,
                    level,
                    parents,
                    parent_count,
                });
            }
            validate_descriptor(entry, level)?;
            if level == 3 || entry & TABLE_OR_PAGE == 0 {
                return Ok(Walk {
                    table,
                    index,
                    entry,
                    level,
                    parents,
                    parent_count,
                });
            }
            let child = entry & ADDRESS_MASK;
            parents[parent_count] = Parent {
                table,
                index,
                child,
            };
            parent_count += 1;
            table = child;
        }
        Err(PagingError::UnsupportedDescriptor)
    }

    fn map_entry(
        &mut self,
        memory: &mut impl TableMemory,
        virtual_address: u64,
        physical_address: u64,
        mut target_level: u8,
        attrs: MappingAttrs,
        user: bool,
    ) -> Result<u64, PagingError> {
        let mut table = self.root;
        for level in 1..=3 {
            let index = level_index(virtual_address, level);
            let entry = memory.read_entry(table, index);
            if level == target_level && entry & VALID == 0 {
                memory.write_entry(
                    table,
                    index,
                    if user {
                        user_leaf_descriptor(physical_address, level, attrs)
                    } else {
                        leaf_descriptor(physical_address, level, attrs)
                    },
                );
                return Ok(level_size(level));
            }
            if entry & VALID == 0 {
                let child = allocate_table(memory)?;
                memory.write_entry(table, index, table_descriptor(child));
                table = child;
            } else {
                validate_descriptor(entry, level)?;
                if level == 3 || entry & TABLE_OR_PAGE == 0 {
                    return Err(PagingError::AlreadyMapped);
                }
                table = entry & ADDRESS_MASK;
                // A partially populated table cannot be replaced by a block.
                // Descend one level and use its smaller block/page instead.
                if level == target_level {
                    target_level += 1;
                }
            }
        }
        Err(PagingError::UnsupportedDescriptor)
    }

    fn split_block(
        &mut self,
        memory: &mut impl TableMemory,
        walk: &Walk,
    ) -> Result<(), PagingError> {
        if walk.level >= 3 {
            return Err(PagingError::UnsupportedDescriptor);
        }
        let child = allocate_table(memory)?;
        let start = walk.entry & ADDRESS_MASK;
        let child_level = walk.level + 1;
        for index in 0..ENTRY_COUNT {
            memory.write_entry(
                child,
                index,
                // Preserve all validated leaf permissions, including EL0 AP,
                // PXN/UXN and nG, when splitting a user or kernel block.
                (walk.entry & !(ARCH_ADDRESS_MASK | TABLE_OR_PAGE))
                    | (start + index as u64 * level_size(child_level))
                    | if child_level == 3 { TABLE_OR_PAGE } else { 0 },
            );
        }
        memory.write_entry(walk.table, walk.index, table_descriptor(child));
        Ok(())
    }

    fn prune(&mut self, memory: &mut impl TableMemory, walk: &Walk) {
        for parent in walk.parents[..walk.parent_count].iter().rev() {
            if (0..ENTRY_COUNT).any(|index| memory.read_entry(parent.child, index) & VALID != 0) {
                break;
            }
            memory.write_entry(parent.table, parent.index, 0);
            memory.release_table(parent.child);
        }
    }
}

fn validate_virtual_range(virtual_address: u64, size: u64) -> Result<u64, PagingError> {
    if virtual_address % PAGE_SIZE != 0 || size % PAGE_SIZE != 0 {
        return Err(PagingError::Unaligned);
    }
    virtual_address
        .checked_add(size)
        .filter(|end| size != 0 && *end <= VIRTUAL_ADDRESS_LIMIT)
        .ok_or(PagingError::InvalidRange)
}

fn allocate_table(memory: &mut impl TableMemory) -> Result<u64, PagingError> {
    let address = memory.allocate_table()?;
    if address % PAGE_SIZE != 0 || address >= PHYSICAL_ADDRESS_LIMIT {
        memory.release_table(address);
        return Err(PagingError::InvalidTableAddress);
    }
    for index in 0..ENTRY_COUNT {
        memory.write_entry(address, index, 0);
    }
    Ok(address)
}

fn table_descriptor(address: u64) -> u64 {
    address | VALID | TABLE_OR_PAGE
}

fn leaf_descriptor(address: u64, level: u8, attrs: MappingAttrs) -> u64 {
    let memory_attrs = match attrs.memory {
        MemoryType::NormalNonCacheable => SHAREABLE_INNER,
        MemoryType::Device => (1 << 2) | SHAREABLE_OUTER,
    };
    address
        | VALID
        | if level == 3 { TABLE_OR_PAGE } else { 0 }
        | memory_attrs
        | ACCESS_FLAG
        | USER_EXECUTE_NEVER
        | if attrs.writable { 0 } else { AP_READ_ONLY }
        | if attrs.executable {
            0
        } else {
            PRIVILEGED_EXECUTE_NEVER
        }
}

fn user_leaf_descriptor(address: u64, level: u8, attrs: MappingAttrs) -> u64 {
    let descriptor = leaf_descriptor(address, level, attrs)
        | AP_USER_ACCESS
        | PRIVILEGED_EXECUTE_NEVER
        | NOT_GLOBAL;
    if attrs.executable {
        descriptor & !USER_EXECUTE_NEVER
    } else {
        descriptor
    }
}

fn decode_attrs(entry: u64) -> Option<MappingAttrs> {
    let memory = match entry & ATTR_INDEX_MASK {
        0 => MemoryType::NormalNonCacheable,
        4 => MemoryType::Device,
        _ => return None,
    };
    let writable = matches!(entry & AP_MASK, 0 | AP_USER_ACCESS);
    Some(MappingAttrs {
        memory,
        writable,
        executable: entry & PRIVILEGED_EXECUTE_NEVER == 0,
    })
}

fn validate_descriptor(entry: u64, level: u8) -> Result<(), PagingError> {
    if entry & (ARCH_ADDRESS_MASK & !ADDRESS_MASK) != 0 {
        return Err(PagingError::UnsupportedDescriptor);
    }
    if level < 3 && entry & TABLE_OR_PAGE != 0 {
        // This mapper produces unrestricted table descriptors. Reject external
        // hierarchy restrictions rather than reporting incorrect leaf access.
        if entry
            & (TABLE_USER_EXECUTE_NEVER
                | TABLE_NO_EL0_ACCESS
                | TABLE_READ_ONLY
                | TABLE_PRIVILEGED_EXECUTE_NEVER)
            != 0
        {
            return Err(PagingError::UnsupportedDescriptor);
        }
    } else if (level == 3 && entry & TABLE_OR_PAGE == 0)
        || entry & ADDRESS_MASK & (level_size(level) - 1) != 0
        || entry & ACCESS_FLAG == 0
        || decode_attrs(entry).is_none()
        || !valid_leaf_permissions(entry)
    {
        return Err(PagingError::UnsupportedDescriptor);
    }
    Ok(())
}

fn valid_leaf_permissions(entry: u64) -> bool {
    if entry & AP_USER_ACCESS == 0 {
        return entry & USER_EXECUTE_NEVER != 0;
    }
    // User mappings never expose MMIO or permit privileged execution. User
    // writable code and global user TLB entries are not part of this contract.
    entry & ATTR_INDEX_MASK == 0
        && entry & (PRIVILEGED_EXECUTE_NEVER | NOT_GLOBAL) == PRIVILEGED_EXECUTE_NEVER | NOT_GLOBAL
        && (entry & AP_READ_ONLY != 0 || entry & USER_EXECUTE_NEVER != 0)
}

fn level_size(level: u8) -> u64 {
    1 << level_shift(level)
}

fn level_shift(level: u8) -> u32 {
    match level {
        1 => 30,
        2 => 21,
        3 => 12,
        _ => unreachable!(),
    }
}

fn level_index(virtual_address: u64, level: u8) -> usize {
    ((virtual_address >> level_shift(level)) & 0x1ff) as usize
}

fn next_boundary(virtual_address: u64, level: u8) -> u64 {
    (virtual_address & !(level_size(level) - 1)) + level_size(level)
}

fn aligned_for(virtual_address: u64, physical_address: u64, level: u8) -> bool {
    (virtual_address | physical_address) & (level_size(level) - 1) == 0
}

fn release_tree(memory: &mut impl TableMemory, table: u64, level: u8) {
    if level < 3 {
        for index in 0..ENTRY_COUNT {
            let entry = memory.read_entry(table, index);
            if entry & (VALID | TABLE_OR_PAGE) == VALID | TABLE_OR_PAGE {
                release_tree(memory, entry & ADDRESS_MASK, level + 1);
            }
        }
    }
    memory.release_table(table);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const DATA: MappingAttrs = MappingAttrs {
        memory: MemoryType::NormalNonCacheable,
        writable: true,
        executable: false,
    };
    const TEXT: MappingAttrs = MappingAttrs {
        memory: MemoryType::NormalNonCacheable,
        writable: false,
        executable: true,
    };
    const DEVICE: MappingAttrs = MappingAttrs {
        memory: MemoryType::Device,
        writable: true,
        executable: false,
    };

    struct Model {
        pages: BTreeMap<u64, [u64; ENTRY_COUNT]>,
        next_address: u64,
        max_live: usize,
        freed: usize,
    }

    impl Model {
        fn new(max_live: usize) -> Self {
            Self {
                pages: BTreeMap::new(),
                next_address: 0x1_0000_0000,
                max_live,
                freed: 0,
            }
        }
    }

    impl TableMemory for Model {
        fn allocate_table(&mut self) -> Result<u64, PagingError> {
            if self.pages.len() == self.max_live {
                return Err(PagingError::OutOfMemory);
            }
            let address = self.next_address;
            self.next_address += PAGE_SIZE;
            // Exercise initialization rather than relying on zeroed storage.
            assert!(
                self.pages
                    .insert(address, [u64::MAX; ENTRY_COUNT])
                    .is_none()
            );
            Ok(address)
        }

        fn read_entry(&self, table: u64, index: usize) -> u64 {
            self.pages[&table][index]
        }

        fn write_entry(&mut self, table: u64, index: usize, entry: u64) {
            self.pages.get_mut(&table).unwrap()[index] = entry;
        }

        fn release_table(&mut self, table: u64) {
            assert!(self.pages.remove(&table).is_some());
            self.freed += 1;
        }
    }

    #[test]
    fn root_is_aligned_above_four_gib_and_cleared() {
        let mut memory = Model::new(1);
        let tables = PageTables::new(&mut memory).unwrap();
        assert_eq!(tables.root_address(), 0x1_0000_0000);
        assert!(
            memory.pages[&tables.root_address()]
                .iter()
                .all(|entry| *entry == 0)
        );
        tables.destroy(&mut memory);
        assert!(memory.pages.is_empty());
    }

    #[test]
    fn descriptor_bits_match_el1_access_and_memory_attributes() {
        let data = leaf_descriptor(0x8_0000_0000, 1, DATA);
        assert_eq!(data & (VALID | TABLE_OR_PAGE), VALID);
        assert_eq!(data & ARCH_ADDRESS_MASK, 0x8_0000_0000);
        assert_eq!(data & ATTR_INDEX_MASK, 0);
        assert_eq!(data & AP_MASK, 0);
        assert_eq!((data >> 8) & 3, 3);
        assert_ne!(data & ACCESS_FLAG, 0);
        assert_ne!(data & USER_EXECUTE_NEVER, 0);
        assert_ne!(data & PRIVILEGED_EXECUTE_NEVER, 0);

        let text = leaf_descriptor(0x20_0000, 3, TEXT);
        assert_eq!(text & (VALID | TABLE_OR_PAGE), 3);
        assert_eq!((text >> 6) & 3, 2);
        assert_eq!(text & PRIVILEGED_EXECUTE_NEVER, 0);
        assert_ne!(text & USER_EXECUTE_NEVER, 0);

        let device = leaf_descriptor(0xfe00_0000, 2, DEVICE);
        assert_eq!((device >> 2) & 7, 1);
        assert_eq!((device >> 8) & 3, 2);
        assert_ne!(device & PRIVILEGED_EXECUTE_NEVER, 0);

        let table = table_descriptor(0x1_0000_0000);
        assert_eq!(table & (VALID | TABLE_OR_PAGE), 3);
        assert_eq!((table >> 61) & 3, 0);
        assert_eq!(table & TABLE_USER_EXECUTE_NEVER, 0);
        assert_eq!(table & (1 << 59), 0);
    }

    #[test]
    fn user_leaf_permissions_keep_kernel_and_device_leaves_private() {
        let mut memory = Model::new(12);
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_range(&mut memory, 0x20_0000, 0x20_0000, PAGE_SIZE, TEXT)
            .unwrap();
        tables
            .map_range(&mut memory, 0xfe00_0000, 0xfe00_0000, PAGE_SIZE, DEVICE)
            .unwrap();
        let user_code = 0x40_0000_0000;
        let user_data = user_code + PAGE_SIZE;
        tables
            .map_user_range(&mut memory, user_code, 0x40_0000, PAGE_SIZE, false, true)
            .unwrap();
        tables
            .map_user_range(&mut memory, user_data, 0x50_0000, PAGE_SIZE, true, false)
            .unwrap();
        let code = tables.lookup(&memory, user_code + 12).unwrap();
        assert_eq!(code.physical_address, 0x40_000c);
        assert!(code.user_accessible && code.user_executable);
        assert!(!code.attrs.writable && !code.attrs.executable);
        let data = tables.lookup(&memory, user_data).unwrap();
        assert!(data.user_accessible && data.attrs.writable);
        assert!(!data.user_executable && !data.attrs.executable);
        for address in [0x20_0000, 0xfe00_0000] {
            let translation = tables.lookup(&memory, address).unwrap();
            assert!(!translation.user_accessible && !translation.user_executable);
        }
        let code_walk = tables.walk(&memory, user_code).unwrap();
        assert_eq!((code_walk.entry >> 6) & 3, 3);
        assert_ne!(code_walk.entry & NOT_GLOBAL, 0);
        assert_ne!(code_walk.entry & PRIVILEGED_EXECUTE_NEVER, 0);
        assert_eq!(code_walk.entry & USER_EXECUTE_NEVER, 0);
    }

    #[test]
    fn user_writable_code_rejected_before_mutation() {
        let mut memory = Model::new(4);
        let mut tables = PageTables::new(&mut memory).unwrap();
        let before = memory.pages.clone();
        assert_eq!(
            tables.map_user_range(
                &mut memory,
                0x40_0000_0000,
                0x40_0000,
                PAGE_SIZE,
                true,
                true
            ),
            Err(PagingError::WritableExecutable)
        );
        assert_eq!(memory.pages, before);
    }

    #[test]
    fn partial_user_block_unmap_preserves_user_permissions() {
        let mut memory = Model::new(8);
        let mut tables = PageTables::new(&mut memory).unwrap();
        let start = 0x40_0000_0000;
        tables
            .map_user_range(&mut memory, start, 0x4000_0000, level_size(1), false, true)
            .unwrap();
        tables
            .unmap_range(&mut memory, start + PAGE_SIZE, PAGE_SIZE)
            .unwrap();
        assert!(tables.lookup(&memory, start + PAGE_SIZE).is_none());
        for address in [start, start + PAGE_SIZE * 2, start + level_size(1) - 1] {
            let translation = tables.lookup(&memory, address).unwrap();
            assert!(translation.user_accessible && translation.user_executable);
            assert!(!translation.attrs.writable && !translation.attrs.executable);
        }
        tables.destroy(&mut memory);
        assert!(memory.pages.is_empty());
    }

    #[test]
    fn malformed_user_privileged_device_and_global_leaves_are_denied() {
        let mut memory = Model::new(4);
        let tables = PageTables::new(&mut memory).unwrap();
        let valid = user_leaf_descriptor(0, 1, TEXT);
        for malformed in [
            valid & !PRIVILEGED_EXECUTE_NEVER,
            valid & !NOT_GLOBAL,
            valid | (1 << 2),
            valid & !AP_READ_ONLY,
        ] {
            memory.write_entry(tables.root, 0, malformed);
            assert!(tables.lookup(&memory, 0).is_none());
        }
    }

    #[test]
    fn greedy_blocks_pages_and_inexact_ends_do_not_map_extra_bytes() {
        let mut memory = Model::new(16);
        let mut tables = PageTables::new(&mut memory).unwrap();
        let start = level_size(1);
        let size = level_size(1) + level_size(2) + PAGE_SIZE;
        tables
            .map_range(&mut memory, start, 0x4_0000_0000, size, DATA)
            .unwrap();
        assert_eq!(tables.lookup(&memory, start + 1).unwrap().level, 1);
        assert_eq!(
            tables
                .lookup(&memory, start + level_size(1) + 1)
                .unwrap()
                .level,
            2
        );
        let tail = start + level_size(1) + level_size(2);
        assert_eq!(
            tables.lookup(&memory, tail + PAGE_SIZE - 1).unwrap().level,
            3
        );
        assert_eq!(tables.lookup(&memory, tail + PAGE_SIZE), None);
        assert_eq!(tables.lookup(&memory, start - 1), None);
        assert_eq!(memory.pages.len(), 3);
        tables.destroy(&mut memory);
        assert!(memory.pages.is_empty());
    }

    #[test]
    fn sparse_ranges_preserve_holes_and_offsets() {
        let mut memory = Model::new(16);
        let mut tables = PageTables::new(&mut memory).unwrap();
        for (va, pa, attrs) in [
            (PAGE_SIZE, 0x2000, TEXT),
            (0x1000_0000, 0xfe00_0000, DEVICE),
        ] {
            tables
                .map_range(&mut memory, va, pa, PAGE_SIZE, attrs)
                .unwrap();
            assert_eq!(
                tables.lookup(&memory, va + 123).unwrap(),
                Translation {
                    physical_address: pa + 123,
                    attrs,
                    user_accessible: false,
                    user_executable: false,
                    level: 3,
                }
            );
        }
        assert_eq!(tables.lookup(&memory, 0), None);
        assert_eq!(tables.lookup(&memory, PAGE_SIZE * 2), None);
        assert_eq!(tables.lookup(&memory, 0xffff_ffff_ffff_f000), None);
    }

    #[test]
    fn non_matching_alignment_uses_pages_instead_of_inexact_blocks() {
        let mut memory = Model::new(8);
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_range(&mut memory, 0, PAGE_SIZE, level_size(2), DATA)
            .unwrap();
        assert_eq!(tables.lookup(&memory, 0).unwrap().level, 3);
        assert_eq!(
            tables
                .lookup(&memory, level_size(2) - 1)
                .unwrap()
                .physical_address,
            PAGE_SIZE + level_size(2) - 1
        );
        assert_eq!(tables.lookup(&memory, level_size(2)), None);
        assert_eq!(memory.pages.len(), 3);
    }

    #[test]
    fn existing_empty_subranges_use_smaller_leaves_without_overwriting_tables() {
        let mut memory = Model::new(8);
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_range(&mut memory, PAGE_SIZE, PAGE_SIZE, PAGE_SIZE, TEXT)
            .unwrap();
        tables
            .unmap_range(&mut memory, PAGE_SIZE, PAGE_SIZE)
            .unwrap();
        // Keep another L3 table below the same root entry while mapping a large
        // aligned range into the adjacent, currently empty L2 slot.
        tables
            .map_range(&mut memory, PAGE_SIZE, PAGE_SIZE, PAGE_SIZE, TEXT)
            .unwrap();
        tables
            .map_range(
                &mut memory,
                level_size(2),
                level_size(2),
                level_size(2),
                DATA,
            )
            .unwrap();
        assert_eq!(tables.lookup(&memory, PAGE_SIZE).unwrap().attrs, TEXT);
        assert_eq!(tables.lookup(&memory, level_size(2)).unwrap().level, 2);
    }

    #[test]
    fn virtual_aliases_and_last_supported_addresses_translate() {
        let mut memory = Model::new(12);
        let mut tables = PageTables::new(&mut memory).unwrap();
        let physical = PHYSICAL_ADDRESS_LIMIT - PAGE_SIZE;
        for address in [0, level_size(1), VIRTUAL_ADDRESS_LIMIT - PAGE_SIZE] {
            tables
                .map_range(&mut memory, address, physical, PAGE_SIZE, DATA)
                .unwrap();
            assert_eq!(
                tables
                    .lookup(&memory, address + PAGE_SIZE - 1)
                    .unwrap()
                    .physical_address,
                PHYSICAL_ADDRESS_LIMIT - 1
            );
        }
        assert_eq!(tables.lookup(&memory, VIRTUAL_ADDRESS_LIMIT), None);
    }

    #[test]
    fn collision_is_checked_before_a_free_prefix_is_mapped() {
        let mut memory = Model::new(8);
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_range(&mut memory, PAGE_SIZE * 3, PAGE_SIZE * 8, PAGE_SIZE, TEXT)
            .unwrap();
        let before = memory.pages.clone();
        assert_eq!(
            tables.map_range(&mut memory, PAGE_SIZE, PAGE_SIZE, PAGE_SIZE * 3, DATA),
            Err(PagingError::AlreadyMapped)
        );
        assert_eq!(memory.pages, before);
        assert_eq!(tables.lookup(&memory, PAGE_SIZE), None);
        assert_eq!(
            tables.map_range(&mut memory, PAGE_SIZE * 3, PAGE_SIZE * 8, PAGE_SIZE, TEXT),
            Err(PagingError::AlreadyMapped)
        );
    }

    #[test]
    fn invalid_inputs_do_not_mutate_tables() {
        let mut memory = Model::new(16);
        let mut tables = PageTables::new(&mut memory).unwrap();
        for (va, pa, size, attrs, error) in [
            (1, 0, PAGE_SIZE, DATA, PagingError::Unaligned),
            (0, 1, PAGE_SIZE, DATA, PagingError::Unaligned),
            (0, 0, PAGE_SIZE - 1, DATA, PagingError::Unaligned),
            (0, 0, 0, DATA, PagingError::InvalidRange),
            (
                VIRTUAL_ADDRESS_LIMIT,
                0,
                PAGE_SIZE,
                DATA,
                PagingError::InvalidRange,
            ),
            (
                0,
                PHYSICAL_ADDRESS_LIMIT,
                PAGE_SIZE,
                DATA,
                PagingError::InvalidRange,
            ),
            (
                0,
                u64::MAX - PAGE_SIZE + 1,
                PAGE_SIZE,
                DATA,
                PagingError::InvalidRange,
            ),
            (
                0,
                0,
                PAGE_SIZE,
                MappingAttrs {
                    executable: true,
                    ..DATA
                },
                PagingError::WritableExecutable,
            ),
            (
                0,
                0,
                PAGE_SIZE,
                MappingAttrs {
                    writable: false,
                    executable: true,
                    ..DEVICE
                },
                PagingError::InvalidAttributes,
            ),
        ] {
            assert_eq!(
                tables.map_range(&mut memory, va, pa, size, attrs),
                Err(error)
            );
            assert!(memory.pages[&tables.root].iter().all(|entry| *entry == 0));
            assert_eq!(memory.pages.len(), 1);
        }
    }

    #[test]
    fn partial_block_unmap_splits_and_preserves_neighbor_attributes() {
        let mut memory = Model::new(8);
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_range(&mut memory, 0, 0x4_0000_0000, level_size(1), TEXT)
            .unwrap();
        tables
            .unmap_range(&mut memory, level_size(2) + PAGE_SIZE, PAGE_SIZE)
            .unwrap();
        assert_eq!(tables.lookup(&memory, level_size(2) + PAGE_SIZE), None);
        for va in [
            0,
            level_size(2),
            level_size(2) + PAGE_SIZE * 2,
            level_size(1) - 1,
        ] {
            let translation = tables.lookup(&memory, va).unwrap();
            assert_eq!(translation.physical_address, 0x4_0000_0000 + va);
            assert_eq!(translation.attrs, TEXT);
        }
        assert_eq!(tables.lookup(&memory, 0).unwrap().level, 2);
        assert_eq!(tables.lookup(&memory, level_size(2)).unwrap().level, 3);
        assert_eq!(memory.pages.len(), 3);
        tables.unmap_range(&mut memory, 0, level_size(1)).unwrap();
        assert_eq!(memory.pages.len(), 1);
        assert!(memory.pages[&tables.root].iter().all(|entry| *entry == 0));
    }

    #[test]
    fn split_allocation_failure_preserves_the_block_and_can_be_destroyed() {
        let mut memory = Model::new(1);
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_range(&mut memory, 0, 0, level_size(1), DATA)
            .unwrap();
        assert_eq!(
            tables.unmap_range(&mut memory, PAGE_SIZE, PAGE_SIZE),
            Err(PagingError::OutOfMemory)
        );
        assert_eq!(tables.lookup(&memory, PAGE_SIZE).unwrap().level, 1);
        tables.destroy(&mut memory);
        assert!(memory.pages.is_empty());
    }

    #[test]
    fn map_allocation_failure_keeps_prefix_and_all_tables_reclaimable() {
        let mut memory = Model::new(2);
        let mut tables = PageTables::new(&mut memory).unwrap();
        let size = level_size(1) + level_size(2) + PAGE_SIZE;
        assert_eq!(
            tables.map_range(&mut memory, 0, 0, size, DATA),
            Err(PagingError::OutOfMemory)
        );
        assert_eq!(tables.lookup(&memory, 0).unwrap().level, 1);
        assert_eq!(tables.lookup(&memory, level_size(1)).unwrap().level, 2);
        assert_eq!(tables.lookup(&memory, level_size(1) + level_size(2)), None);
        tables.destroy(&mut memory);
        assert!(memory.pages.is_empty());
        assert_eq!(memory.freed, 2);
    }

    #[test]
    fn invalid_allocator_address_is_released_before_any_access() {
        for address in [0x1001, PHYSICAL_ADDRESS_LIMIT] {
            let mut memory = Model::new(1);
            memory.next_address = address;
            assert_eq!(
                PageTables::new(&mut memory).unwrap_err(),
                PagingError::InvalidTableAddress
            );
            assert!(memory.pages.is_empty());
            assert_eq!(memory.freed, 1);
        }
        assert_eq!(
            PageTables::new(&mut Model::new(0)).unwrap_err(),
            PagingError::OutOfMemory
        );
    }

    #[test]
    fn malformed_leaf_cannot_be_traversed_or_replaced() {
        let mut memory = Model::new(4);
        let mut tables = PageTables::new(&mut memory).unwrap();
        let mut descriptor = leaf_descriptor(0, 1, DATA);
        descriptor |= 1 << 40;
        memory.write_entry(tables.root, 0, descriptor);
        assert_eq!(tables.lookup(&memory, 0), None);
        assert_eq!(
            tables.map_range(&mut memory, 0, 0, PAGE_SIZE, DATA),
            Err(PagingError::UnsupportedDescriptor)
        );
        memory.write_entry(tables.root, 0, leaf_descriptor(PAGE_SIZE, 1, DATA));
        assert_eq!(tables.lookup(&memory, 0), None);
    }

    #[test]
    fn repeated_sparse_map_unmap_reclaims_tables_without_freeing_data() {
        let mut memory = Model::new(12);
        let mut tables = PageTables::new(&mut memory).unwrap();
        for iteration in 0..100 {
            let mut addresses = [0u64; 4];
            for (index, address) in addresses.iter_mut().enumerate() {
                *address = (index as u64 + 1) * level_size(1) + iteration * PAGE_SIZE;
                tables
                    .map_range(
                        &mut memory,
                        *address,
                        (index as u64 + 1) * PAGE_SIZE,
                        PAGE_SIZE,
                        DATA,
                    )
                    .unwrap();
            }
            assert_eq!(memory.pages.len(), 9);
            for address in addresses {
                tables.unmap_range(&mut memory, address, PAGE_SIZE).unwrap();
                assert_eq!(tables.lookup(&memory, address), None);
            }
            assert_eq!(memory.pages.len(), 1);
        }
        tables.destroy(&mut memory);
        assert!(memory.pages.is_empty());
        assert_eq!(memory.freed, 801);
    }
}
