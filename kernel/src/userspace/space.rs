//! Allocation-free validation of user mappings and copy boundaries.
//!
//! The M3 low-half address space retains EL1 identity mappings and places EL0
//! mappings at 256 GiB, above the platform RAM/MMIO identity ranges used
//! by Raspberry Pi 4. Copying returns physical chunks: the caller uses the
//! kernel's EL1 identity mapping, never blindly dereferences an EL0 pointer.
//! The owner must prevent page-table mutation/process teardown throughout a
//! validation-and-copy operation. These functions do not modify active tables.

use crate::memory::paging::{MemoryType, PAGE_SIZE, PageTables, TableMemory};

pub const USER_CODE: u64 = 0x40_0000_0000;
pub const USER_DATA: u64 = USER_CODE + 0x1_0000;
pub const USER_STACK_GUARD: u64 = USER_CODE + 0x20_0000;
pub const USER_STACK_BASE: u64 = USER_STACK_GUARD + PAGE_SIZE;
pub const USER_STACK_PAGES: u64 = 4;
pub const USER_STACK_TOP: u64 = USER_STACK_BASE + USER_STACK_PAGES * PAGE_SIZE;
pub const USER_ADDRESS_START: u64 = USER_CODE;
pub const USER_ADDRESS_END: u64 = USER_ADDRESS_START + (1 << 30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAccess {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCopyError {
    InvalidRange,
    Unmapped,
    PermissionDenied,
    DeviceMemory,
    NotOwned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserChunk {
    pub physical_address: u64,
    /// Bytes available in this mapping, bounded to one 4 KiB page.
    pub length: usize,
}

fn range_end(address: u64, length: usize) -> Result<u64, UserCopyError> {
    if !(USER_ADDRESS_START..USER_ADDRESS_END).contains(&address) {
        return Err(UserCopyError::InvalidRange);
    }
    address
        .checked_add(length as u64)
        .filter(|end| *end <= USER_ADDRESS_END)
        .ok_or(UserCopyError::InvalidRange)
}

/// Translate the next copy chunk without accessing user bytes. The whole range
/// must be validated first with `validate_owned_user_range` before accessing
/// task-owned bytes, so a later invalid page cannot cause partial effects.
/// Zero-length chunks are invalid.
pub fn user_chunk(
    tables: &PageTables,
    memory: &impl TableMemory,
    address: u64,
    length: usize,
    access: UserAccess,
) -> Result<UserChunk, UserCopyError> {
    range_end(address, length)?;
    if length == 0 {
        return Err(UserCopyError::InvalidRange);
    }
    let translation = tables
        .lookup(memory, address)
        .ok_or(UserCopyError::Unmapped)?;
    if !translation.user_accessible {
        return Err(UserCopyError::PermissionDenied);
    }
    if translation.attrs.memory != MemoryType::NormalNonCacheable {
        return Err(UserCopyError::DeviceMemory);
    }
    if (access == UserAccess::Write && !translation.attrs.writable)
        || (access == UserAccess::Execute && !translation.user_executable)
    {
        return Err(UserCopyError::PermissionDenied);
    }
    Ok(UserChunk {
        physical_address: translation.physical_address,
        length: length.min((PAGE_SIZE - address % PAGE_SIZE) as usize),
    })
}

/// Validate every page in a byte range, including overflow and kernel/MMIO
/// exclusion. Empty ranges still require a canonical pointer in the user area;
/// they touch no pages. Validation is read-only and performs no allocation.
pub fn validate_user_range(
    tables: &PageTables,
    memory: &impl TableMemory,
    address: u64,
    length: usize,
    access: UserAccess,
) -> Result<(), UserCopyError> {
    validate_owned_user_range(tables, memory, address, length, access, |_, _| true)
}

/// Validate mapping permissions and task ownership for the complete range
/// before any bytes are read, written, or emitted. `owns` checks physical byte
/// ranges, including offsets within a page and physically discontiguous pages.
/// The caller must keep mappings and ownership stable through validation and
/// the later copy. Empty ranges validate the pointer but need no owned pages.
pub fn validate_owned_user_range(
    tables: &PageTables,
    memory: &impl TableMemory,
    address: u64,
    length: usize,
    access: UserAccess,
    owns: impl Fn(u64, usize) -> bool,
) -> Result<(), UserCopyError> {
    let end = range_end(address, length)?;
    let mut cursor = address;
    while cursor < end {
        let chunk = user_chunk(tables, memory, cursor, (end - cursor) as usize, access)?;
        if !owns(chunk.physical_address, chunk.length) {
            return Err(UserCopyError::NotOwned);
        }
        cursor += chunk.length as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::paging::{MappingAttrs, PagingError};
    use std::collections::BTreeMap;

    struct Model {
        pages: BTreeMap<u64, [u64; 512]>,
        next: u64,
    }

    impl Model {
        fn new() -> Self {
            Self {
                pages: BTreeMap::new(),
                next: 0x1000,
            }
        }
    }

    impl TableMemory for Model {
        fn allocate_table(&mut self) -> Result<u64, PagingError> {
            let address = self.next;
            self.next += PAGE_SIZE;
            assert!(self.pages.insert(address, [0; 512]).is_none());
            Ok(address)
        }

        fn read_entry(&self, table: u64, index: usize) -> u64 {
            self.pages[&table][index]
        }

        fn write_entry(&mut self, table: u64, index: usize, value: u64) {
            self.pages.get_mut(&table).unwrap()[index] = value;
        }
    }

    #[test]
    fn cross_page_copy_uses_each_physical_page_and_offset() {
        let mut memory = Model::new();
        let mut tables = PageTables::new(&mut memory).unwrap();
        for (va, pa) in [(USER_DATA, 0x50_0000), (USER_DATA + PAGE_SIZE, 0x80_0000)] {
            tables
                .map_user_range(&mut memory, va, pa, PAGE_SIZE, true, false)
                .unwrap();
        }
        let address = USER_DATA + PAGE_SIZE - 3;
        validate_user_range(&tables, &memory, address, 7, UserAccess::Write).unwrap();
        let first = user_chunk(&tables, &memory, address, 7, UserAccess::Write).unwrap();
        assert_eq!(
            first,
            UserChunk {
                physical_address: 0x50_0ffd,
                length: 3
            }
        );
        let second = user_chunk(&tables, &memory, address + 3, 4, UserAccess::Write).unwrap();
        assert_eq!(
            second,
            UserChunk {
                physical_address: 0x80_0000,
                length: 4
            }
        );
    }

    #[test]
    fn user_code_data_and_kernel_permissions_remain_distinct() {
        let mut memory = Model::new();
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_user_range(&mut memory, USER_CODE, 0x40_0000, PAGE_SIZE, false, true)
            .unwrap();
        tables
            .map_user_range(&mut memory, USER_DATA, 0x50_0000, PAGE_SIZE, true, false)
            .unwrap();
        let kernel_alias = USER_DATA + PAGE_SIZE;
        tables
            .map_range(
                &mut memory,
                kernel_alias,
                0x20_0000,
                PAGE_SIZE,
                MappingAttrs {
                    memory: MemoryType::NormalNonCacheable,
                    writable: false,
                    executable: true,
                },
            )
            .unwrap();
        validate_user_range(&tables, &memory, USER_CODE, 1, UserAccess::Execute).unwrap();
        validate_user_range(&tables, &memory, USER_CODE, 1, UserAccess::Read).unwrap();
        assert_eq!(
            validate_user_range(&tables, &memory, USER_CODE, 1, UserAccess::Write),
            Err(UserCopyError::PermissionDenied)
        );
        assert_eq!(
            validate_user_range(&tables, &memory, USER_DATA, 1, UserAccess::Execute),
            Err(UserCopyError::PermissionDenied)
        );
        assert_eq!(
            validate_user_range(&tables, &memory, kernel_alias, 1, UserAccess::Read),
            Err(UserCopyError::PermissionDenied)
        );
    }

    #[test]
    fn stack_guards_and_unmapped_later_page_reject_whole_copy() {
        let mut memory = Model::new();
        let mut tables = PageTables::new(&mut memory).unwrap();
        tables
            .map_user_range(
                &mut memory,
                USER_STACK_BASE,
                0x60_0000,
                USER_STACK_PAGES * PAGE_SIZE,
                true,
                false,
            )
            .unwrap();
        validate_user_range(
            &tables,
            &memory,
            USER_STACK_BASE,
            (USER_STACK_PAGES * PAGE_SIZE) as usize,
            UserAccess::Write,
        )
        .unwrap();
        assert_eq!(
            validate_user_range(&tables, &memory, USER_STACK_GUARD, 1, UserAccess::Read),
            Err(UserCopyError::Unmapped)
        );
        assert_eq!(
            validate_user_range(&tables, &memory, USER_STACK_TOP, 1, UserAccess::Write),
            Err(UserCopyError::Unmapped)
        );
        assert_eq!(
            validate_user_range(&tables, &memory, USER_STACK_TOP - 2, 4, UserAccess::Write),
            Err(UserCopyError::Unmapped)
        );
        assert_eq!(USER_STACK_TOP % 16, 0);
    }

    #[test]
    fn overflow_noncanonical_and_identity_addresses_never_translate() {
        let mut memory = Model::new();
        let tables = PageTables::new(&mut memory).unwrap();
        for (address, length) in [
            (0, 0),
            (0x20_0000, 1),
            (0xfe00_0000, 1),
            (USER_ADDRESS_END, 0),
            (u64::MAX, 4),
            (USER_DATA, usize::MAX),
            (USER_ADDRESS_END - 1, 2),
        ] {
            assert_eq!(
                validate_user_range(&tables, &memory, address, length, UserAccess::Read),
                Err(UserCopyError::InvalidRange)
            );
        }
        assert_eq!(
            validate_user_range(&tables, &memory, USER_DATA, 0, UserAccess::Read),
            Ok(())
        );
    }
}
