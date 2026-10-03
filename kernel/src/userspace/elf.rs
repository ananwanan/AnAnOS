//! Allocation-free static ELF64 loading and AArch64 process-stack construction.
//!
//! Parsing produces an offline plan: it never allocates, maps or switches a
//! page table. The owner allocates zeroed private pages, fills them using
//! `LoadSegment::copy_page`, maps the permissions, then publishes the root.
//! ELF program headers follow the System V generic ABI, chapter 5. Bootstrap
//! limits and unsupported features are deliberately explicit, not libc policy.

use super::space::{USER_ADDRESS_END, USER_ADDRESS_START, USER_STACK_GUARD, USER_STACK_TOP};
use crate::memory::paging::PAGE_SIZE;

pub const MAX_LOAD_SEGMENTS: usize = 16;
pub const MAX_PROGRAM_HEADERS: usize = 32;
pub const MAX_IMAGE_PAGES: usize = 128;
pub const MAX_ARGUMENTS: usize = 8;
pub const MAX_ENVIRONMENT: usize = 8;
pub const MAX_STRING_BYTES: usize = 256;
pub const MAX_STRINGS_BYTES: usize = 4096;
pub const AT_NULL: u64 = 0;
pub const AT_PAGESZ: u64 = 6;
pub const AT_ENTRY: u64 = 9;

const ELF_HEADER_BYTES: usize = 64;
const PROGRAM_HEADER_BYTES: usize = 56;
const PT_LOAD: u32 = 1;
const PT_GNU_STACK: u32 = 0x6474_e551;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    Truncated,
    InvalidHeader,
    UnsupportedFormat,
    UnsupportedSegment,
    TooManySegments,
    ImageTooLarge,
    InvalidRange,
    InvalidAlignment,
    InvalidPermissions,
    OverlappingSegments,
    OutOfOrderSegments,
    InvalidEntry,
    NoLoadSegments,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadSegment {
    pub virtual_address: u64,
    pub memory_size: u64,
    pub file_offset: usize,
    pub file_size: usize,
    pub page_start: u64,
    pub page_count: usize,
    pub writable: bool,
    pub executable: bool,
}

impl LoadSegment {
    const EMPTY: Self = Self {
        virtual_address: 0,
        memory_size: 0,
        file_offset: 0,
        file_size: 0,
        page_start: 0,
        page_count: 0,
        writable: false,
        executable: false,
    };

    /// Initialize one owned page, including BSS and segment/page padding.
    /// `page_index` is relative to `page_start`; file offsets remain tied to
    /// the validated ELF image. This helper does not touch physical addresses.
    pub fn copy_page(
        &self,
        image: &[u8],
        page_index: usize,
        destination: &mut [u8; PAGE_SIZE as usize],
    ) -> Result<(), ElfError> {
        if page_index >= self.page_count {
            return Err(ElfError::InvalidRange);
        }
        let file_end = self
            .file_offset
            .checked_add(self.file_size)
            .filter(|end| *end <= image.len())
            .ok_or(ElfError::Truncated)?;
        let page = self
            .page_start
            .checked_add(
                (page_index as u64)
                    .checked_mul(PAGE_SIZE)
                    .ok_or(ElfError::InvalidRange)?,
            )
            .ok_or(ElfError::InvalidRange)?;
        let page_end = page.checked_add(PAGE_SIZE).ok_or(ElfError::InvalidRange)?;
        let initialized_end = self
            .virtual_address
            .checked_add(self.file_size as u64)
            .ok_or(ElfError::InvalidRange)?;
        let start = page.max(self.virtual_address);
        let end = page_end.min(initialized_end);
        destination.fill(0);
        if start < end {
            let source = self.file_offset + (start - self.virtual_address) as usize;
            let count = (end - start) as usize;
            debug_assert!(source + count <= file_end);
            let target = (start - page) as usize;
            destination[target..target + count].copy_from_slice(&image[source..source + count]);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElfPlan {
    pub entry: u64,
    pub page_count: usize,
    segments: [LoadSegment; MAX_LOAD_SEGMENTS],
    segment_count: usize,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

impl ElfPlan {
    pub fn segments(&self) -> &[LoadSegment] {
        &self.segments[..self.segment_count]
    }

    pub fn parse(image: &[u8]) -> Result<Self, ElfError> {
        if image.len() < ELF_HEADER_BYTES {
            return Err(ElfError::Truncated);
        }
        if &image[..4] != b"\x7fELF" || image[6] != 1 || u32_at(image, 20) != 1 {
            return Err(ElfError::InvalidHeader);
        }
        // ELFCLASS64, ELFDATA2LSB, System V OSABI/version, ET_EXEC, EM_AARCH64.
        // AArch64 has no currently defined e_flags bits in this bootstrap ABI.
        if image[4] != 2
            || image[5] != 1
            || image[7] != 0
            || image[8] != 0
            || u16_at(image, 16) != 2
            || u16_at(image, 18) != 183
            || u32_at(image, 48) != 0
        {
            return Err(ElfError::UnsupportedFormat);
        }
        if u16_at(image, 52) as usize != ELF_HEADER_BYTES
            || u16_at(image, 54) as usize != PROGRAM_HEADER_BYTES
        {
            return Err(ElfError::InvalidHeader);
        }
        let count = u16_at(image, 56) as usize;
        if count == 0 {
            return Err(ElfError::NoLoadSegments);
        }
        if count > MAX_PROGRAM_HEADERS {
            return Err(ElfError::TooManySegments);
        }
        let table = usize::try_from(u64_at(image, 32)).map_err(|_| ElfError::InvalidRange)?;
        if table < ELF_HEADER_BYTES {
            return Err(ElfError::InvalidHeader);
        }
        table
            .checked_add(count * PROGRAM_HEADER_BYTES)
            .filter(|end| *end <= image.len())
            .ok_or(ElfError::Truncated)?;
        let mut plan = Self {
            entry: u64_at(image, 24),
            page_count: 0,
            segments: [LoadSegment::EMPTY; MAX_LOAD_SEGMENTS],
            segment_count: 0,
        };
        let mut previous_address = None;
        for index in 0..count {
            let header = table + index * PROGRAM_HEADER_BYTES;
            let kind = u32_at(image, header);
            let flags = u32_at(image, header + 4);
            match kind {
                // Non-loading metadata and a non-executable stack declaration.
                0 | 4 | 6 => continue,
                PT_GNU_STACK if flags & PF_X == 0 => continue,
                PT_LOAD => {}
                // PT_DYNAMIC/PT_INTERP/PT_SHLIB/PT_TLS and unknown extensions
                // need semantics that this static bootstrap loader lacks.
                _ => return Err(ElfError::UnsupportedSegment),
            }
            let offset = u64_at(image, header + 8);
            let address = u64_at(image, header + 16);
            let file_size = u64_at(image, header + 32);
            let memory_size = u64_at(image, header + 40);
            let alignment = u64_at(image, header + 48);
            if file_size > memory_size {
                return Err(ElfError::InvalidRange);
            }
            if alignment > 1
                && (!alignment.is_power_of_two() || address % alignment != offset % alignment)
            {
                return Err(ElfError::InvalidAlignment);
            }
            if address % PAGE_SIZE != offset % PAGE_SIZE {
                return Err(ElfError::InvalidAlignment);
            }
            // The A72 page format cannot deny EL0 reads while allowing writes
            // or execution. Reject those requests instead of widening access.
            if flags & !7 != 0 || flags & PF_R == 0 || flags & (PF_W | PF_X) == (PF_W | PF_X) {
                return Err(ElfError::InvalidPermissions);
            }
            let file_offset = usize::try_from(offset).map_err(|_| ElfError::InvalidRange)?;
            let file_size = usize::try_from(file_size).map_err(|_| ElfError::InvalidRange)?;
            file_offset
                .checked_add(file_size)
                .filter(|end| *end <= image.len())
                .ok_or(ElfError::Truncated)?;
            if memory_size == 0 {
                continue;
            }
            if previous_address.is_some_and(|previous| address < previous) {
                return Err(ElfError::OutOfOrderSegments);
            }
            previous_address = Some(address);
            let memory_end = address
                .checked_add(memory_size)
                .ok_or(ElfError::InvalidRange)?;
            let page_start = address & !(PAGE_SIZE - 1);
            let page_end = memory_end
                .checked_add(PAGE_SIZE - 1)
                .map(|end| end & !(PAGE_SIZE - 1))
                .ok_or(ElfError::InvalidRange)?;
            // Preserve the lower guard, mapped stack and upper guard. The
            // rounded page range matters even if bytes do not overlap a guard.
            if page_start < USER_ADDRESS_START
                || page_end > USER_ADDRESS_END
                || (page_start < USER_STACK_TOP + PAGE_SIZE && page_end > USER_STACK_GUARD)
            {
                return Err(ElfError::InvalidRange);
            }
            let pages = ((page_end - page_start) / PAGE_SIZE) as usize;
            plan.page_count = plan
                .page_count
                .checked_add(pages)
                .filter(|pages| *pages <= MAX_IMAGE_PAGES)
                .ok_or(ElfError::ImageTooLarge)?;
            // Shared pages across segments have ambiguous initialization and
            // merged permissions. M4 requires distinct PT_LOAD page ranges.
            if plan.segments().iter().any(|segment| {
                let other_end = segment.page_start + segment.page_count as u64 * PAGE_SIZE;
                page_start < other_end && page_end > segment.page_start
            }) {
                return Err(ElfError::OverlappingSegments);
            }
            if plan.segment_count == MAX_LOAD_SEGMENTS {
                return Err(ElfError::TooManySegments);
            }
            plan.segments[plan.segment_count] = LoadSegment {
                virtual_address: address,
                memory_size,
                file_offset,
                file_size,
                page_start,
                page_count: pages,
                writable: flags & PF_W != 0,
                executable: flags & PF_X != 0,
            };
            plan.segment_count += 1;
        }
        if plan.segment_count == 0 {
            return Err(ElfError::NoLoadSegments);
        }
        // A64 instructions are four-byte aligned. Require an initialized
        // executable instruction, rather than permitting an entry in BSS or
        // executable page padding accidentally exposed by page granularity.
        if plan.entry % 4 != 0
            || !plan.segments().iter().any(|segment| {
                segment.executable
                    && plan.entry >= segment.virtual_address
                    && plan.entry.checked_add(4).is_some_and(|end| {
                        end <= segment.virtual_address + segment.file_size as u64
                    })
            })
        {
            return Err(ElfError::InvalidEntry);
        }
        Ok(plan)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    InvalidRange,
    TooManyStrings,
    StringTooLong,
    EmbeddedNull,
    StringsTooLarge,
    TooSmall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialStack {
    pub stack_pointer: u64,
    pub argv_address: u64,
    pub envp_address: u64,
    pub used_bytes: usize,
}

/// Build a little-endian Unix-style entry stack in exclusively owned storage.
/// All validation happens before changing `buffer`; failed calls are atomic.
///
/// SP is rounded down to 16 bytes. From SP upward are u64 argc, argv pointers,
/// NULL, envp pointers, NULL, then (AT_PAGESZ, 4096), (AT_ENTRY, entry), and
/// (AT_NULL, 0). Strings live above those words, each NUL-terminated; pointer
/// values are user virtual addresses, independent of `buffer`'s host address.
/// The complete buffer is zeroed, so alignment padding and unused stack bytes
/// cannot expose kernel or previous-process contents.
pub fn build_initial_stack(
    buffer: &mut [u8],
    stack_base: u64,
    entry: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<InitialStack, StackError> {
    if stack_base % PAGE_SIZE != 0
        || buffer.len() % PAGE_SIZE as usize != 0
        || stack_base < USER_ADDRESS_START
        || stack_base
            .checked_add(buffer.len() as u64)
            .is_none_or(|top| top > USER_ADDRESS_END)
        || !(USER_ADDRESS_START..USER_ADDRESS_END).contains(&entry)
    {
        return Err(StackError::InvalidRange);
    }
    if argv.len() > MAX_ARGUMENTS || envp.len() > MAX_ENVIRONMENT {
        return Err(StackError::TooManyStrings);
    }
    let mut strings_bytes = 0;
    for string in argv.iter().chain(envp.iter()) {
        if string.len() > MAX_STRING_BYTES {
            return Err(StackError::StringTooLong);
        }
        if string.contains(&0) {
            return Err(StackError::EmbeddedNull);
        }
        strings_bytes += string.len() + 1;
    }
    if strings_bytes > MAX_STRINGS_BYTES {
        return Err(StackError::StringsTooLarge);
    }
    let words = 1 + argv.len() + 1 + envp.len() + 1 + 6;
    let stack_offset = buffer
        .len()
        .checked_sub(strings_bytes + words * 8)
        .map(|offset| offset & !15)
        .ok_or(StackError::TooSmall)?;
    let stack_pointer = stack_base + stack_offset as u64;
    let argv_address = stack_pointer + 8;
    let envp_address = argv_address + ((argv.len() + 1) * 8) as u64;
    buffer.fill(0);
    let mut argv_pointers = [0; MAX_ARGUMENTS];
    let mut envp_pointers = [0; MAX_ENVIRONMENT];
    let mut string_cursor = buffer.len();
    for (strings, pointers) in [
        (envp, envp_pointers.as_mut_slice()),
        (argv, argv_pointers.as_mut_slice()),
    ] {
        for (index, string) in strings.iter().enumerate().rev() {
            string_cursor -= string.len() + 1;
            buffer[string_cursor..string_cursor + string.len()].copy_from_slice(string);
            pointers[index] = stack_base + string_cursor as u64;
        }
    }
    let mut word_cursor = stack_offset;
    let mut put_word = |value: u64| {
        buffer[word_cursor..word_cursor + 8].copy_from_slice(&value.to_le_bytes());
        word_cursor += 8;
    };
    put_word(argv.len() as u64);
    for &pointer in &argv_pointers[..argv.len()] {
        put_word(pointer);
    }
    put_word(0);
    for &pointer in &envp_pointers[..envp.len()] {
        put_word(pointer);
    }
    for word in [0, AT_PAGESZ, PAGE_SIZE, AT_ENTRY, entry, AT_NULL, 0] {
        put_word(word);
    }
    Ok(InitialStack {
        stack_pointer,
        argv_address,
        envp_address,
        used_bytes: buffer.len() - stack_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn put16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put64(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn image() -> Vec<u8> {
        let mut image = vec![0; 0x3000];
        image[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x00");
        put16(&mut image, 16, 2);
        put16(&mut image, 18, 183);
        put32(&mut image, 20, 1);
        put64(&mut image, 24, USER_ADDRESS_START);
        put64(&mut image, 32, 64);
        put16(&mut image, 52, 64);
        put16(&mut image, 54, 56);
        put16(&mut image, 56, 2);
        for (index, flags, va, offset, file_size, memory_size) in [
            (0, 5, USER_ADDRESS_START, 0x1000, 8, 8),
            (1, 6, USER_ADDRESS_START + 0x10000, 0x2000, 8, 5000),
        ] {
            let header = 64 + index * 56;
            put32(&mut image, header, 1);
            put32(&mut image, header + 4, flags);
            put64(&mut image, header + 8, offset);
            put64(&mut image, header + 16, va);
            put64(&mut image, header + 32, file_size);
            put64(&mut image, header + 40, memory_size);
            put64(&mut image, header + 48, 4096);
        }
        image[0x1000..0x1008].copy_from_slice(&[0x1f, 0x20, 0x03, 0xd5, 1, 2, 3, 4]);
        image[0x2000..0x2008].copy_from_slice(b"testdata");
        image
    }

    #[test]
    fn static_segments_plan_permissions_and_zero_bss() {
        let image = image();
        let plan = ElfPlan::parse(&image).unwrap();
        assert_eq!(plan.entry, USER_ADDRESS_START);
        assert_eq!(plan.page_count, 3);
        assert_eq!(plan.segments().len(), 2);
        let text = plan.segments()[0];
        assert!(text.executable && !text.writable);
        let data = plan.segments()[1];
        assert!(data.writable && !data.executable);
        let mut page = [0xff; 4096];
        data.copy_page(&image, 0, &mut page).unwrap();
        assert_eq!(&page[..8], b"testdata");
        assert!(page[8..].iter().all(|byte| *byte == 0));
        data.copy_page(&image, 1, &mut page).unwrap();
        assert!(page.iter().all(|byte| *byte == 0));
        assert_eq!(
            data.copy_page(&image, 2, &mut page),
            Err(ElfError::InvalidRange)
        );
    }

    #[test]
    fn unaligned_segment_copies_only_initialized_bytes() {
        let mut image = image();
        put64(&mut image, 64 + 8, 0x1004);
        put64(&mut image, 64 + 16, USER_ADDRESS_START + 4);
        put64(&mut image, 24, USER_ADDRESS_START + 4);
        let plan = ElfPlan::parse(&image).unwrap();
        let mut page = [0xff; 4096];
        plan.segments()[0].copy_page(&image, 0, &mut page).unwrap();
        assert_eq!(&page[..4], &[0; 4]);
        assert_eq!(&page[4..12], &image[0x1004..0x100c]);
        assert!(page[12..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn initialized_data_spanning_pages_is_copied_before_bss() {
        let mut image = image();
        image.resize(0x4000, 0);
        let header = 64 + 56;
        put64(&mut image, header + 8, 0x2004);
        put64(&mut image, header + 16, USER_ADDRESS_START + 0x10004);
        put64(&mut image, header + 32, 4200);
        for (index, byte) in image[0x2004..0x2004 + 4200].iter_mut().enumerate() {
            *byte = index as u8;
        }
        let plan = ElfPlan::parse(&image).unwrap();
        let data = plan.segments()[1];
        let mut page = [0xff; 4096];
        data.copy_page(&image, 0, &mut page).unwrap();
        assert_eq!(&page[..4], &[0; 4]);
        assert_eq!(&page[4..], &image[0x2004..0x3000]);
        data.copy_page(&image, 1, &mut page).unwrap();
        assert_eq!(&page[..108], &image[0x3000..0x306c]);
        assert!(page[108..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn header_and_table_truncation_is_rejected_without_panic() {
        let image = image();
        for length in 0..176 {
            assert!(ElfPlan::parse(&image[..length]).is_err());
        }
        for (offset, value) in [(0, 0), (4, 1), (5, 2), (6, 0), (7, 3), (8, 1)] {
            let mut damaged = image.clone();
            damaged[offset] = value;
            assert!(ElfPlan::parse(&damaged).is_err());
        }
        let mut damaged = image.clone();
        put64(&mut damaged, 32, u64::MAX);
        assert!(ElfPlan::parse(&damaged).is_err());
        for count in [0, 33, 0xffff] {
            let mut damaged = image.clone();
            put16(&mut damaged, 56, count);
            assert!(ElfPlan::parse(&damaged).is_err());
        }
    }

    #[test]
    fn unsupported_architecture_dynamic_and_tls_are_explicit() {
        for (offset, value) in [(16, 3), (18, 62)] {
            let mut damaged = image();
            put16(&mut damaged, offset, value);
            assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::UnsupportedFormat));
        }
        for kind in [2, 3, 5, 7, 0x6000_0000] {
            let mut damaged = image();
            put32(&mut damaged, 64 + 56, kind);
            assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::UnsupportedSegment));
        }
        let mut damaged = image();
        put32(&mut damaged, 64 + 56, PT_GNU_STACK);
        put32(&mut damaged, 64 + 56 + 4, 7);
        assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::UnsupportedSegment));
    }

    #[test]
    fn range_alignment_permissions_and_resource_limits_are_checked() {
        for (field, value, expected) in [
            (32, 9, ElfError::InvalidRange),
            (8, 0x1001, ElfError::InvalidAlignment),
            (48, 3, ElfError::InvalidAlignment),
            (8, u64::MAX - 4095, ElfError::Truncated),
            (40, u64::MAX, ElfError::InvalidRange),
            (
                40,
                (MAX_IMAGE_PAGES as u64 + 1) * PAGE_SIZE,
                ElfError::ImageTooLarge,
            ),
            (16, 0x20_0000, ElfError::InvalidRange),
            (16, USER_STACK_GUARD, ElfError::InvalidRange),
            (16, USER_STACK_TOP, ElfError::InvalidRange),
            (16, USER_ADDRESS_END, ElfError::InvalidRange),
        ] {
            let mut damaged = image();
            put64(&mut damaged, 64 + field, value);
            assert_eq!(
                ElfPlan::parse(&damaged),
                Err(expected),
                "field {field}, value {value:#x}"
            );
        }
        for flags in [0, 1, 2, 3, 7, 0x104] {
            let mut damaged = image();
            put32(&mut damaged, 64 + 4, flags);
            assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::InvalidPermissions));
        }
    }

    #[test]
    fn page_overlap_and_invalid_entry_are_rejected() {
        let mut damaged = image();
        put64(&mut damaged, 64 + 56 + 16, USER_ADDRESS_START);
        assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::OverlappingSegments));
        for entry in [
            USER_ADDRESS_START + 1,
            USER_ADDRESS_START + 8,
            USER_ADDRESS_START + 0x10000,
            USER_ADDRESS_START + 0x10008,
            USER_ADDRESS_START - 4,
            u64::MAX - 3,
        ] {
            let mut damaged = image();
            put64(&mut damaged, 24, entry);
            assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::InvalidEntry));
        }
    }

    #[test]
    fn segment_count_and_load_order_are_bounded() {
        let mut damaged = image();
        put64(&mut damaged, 64 + 16, USER_ADDRESS_START + 0x20000);
        assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::OutOfOrderSegments));
        let mut damaged = image();
        put32(&mut damaged, 64, 4);
        put32(&mut damaged, 64 + 56, 4);
        assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::NoLoadSegments));
        let mut damaged = image();
        put16(&mut damaged, 56, (MAX_LOAD_SEGMENTS + 1) as u16);
        for index in 0..=MAX_LOAD_SEGMENTS {
            let header = 64 + index * PROGRAM_HEADER_BYTES;
            put32(&mut damaged, header, PT_LOAD);
            put32(&mut damaged, header + 4, PF_R | PF_X);
            put64(&mut damaged, header + 8, 0x1000);
            put64(
                &mut damaged,
                header + 16,
                USER_ADDRESS_START + index as u64 * PAGE_SIZE,
            );
            put64(&mut damaged, header + 32, 4);
            put64(&mut damaged, header + 40, 4);
            put64(&mut damaged, header + 48, PAGE_SIZE);
        }
        assert_eq!(ElfPlan::parse(&damaged), Err(ElfError::TooManySegments));
    }

    #[test]
    fn stack_has_fixed_width_pointers_terminators_auxv_and_alignment() {
        let mut bytes = [0xff; 4096];
        let base = super::super::space::USER_STACK_BASE;
        let stack = build_initial_stack(
            &mut bytes,
            base,
            USER_ADDRESS_START,
            &[b"/init/bin/init", b"hello"],
            &[b"PATH=/init/bin", b"TERM=uart"],
        )
        .unwrap();
        assert_eq!(stack.stack_pointer % 16, 0);
        let offset = (stack.stack_pointer - base) as usize;
        assert_eq!(u64_at(&bytes, offset), 2);
        assert_eq!(stack.argv_address, stack.stack_pointer + 8);
        assert_eq!(stack.envp_address, stack.argv_address + 24);
        for (word, string) in [
            (1, b"/init/bin/init".as_slice()),
            (2, b"hello"),
            (4, b"PATH=/init/bin"),
            (5, b"TERM=uart"),
        ] {
            let address = u64_at(&bytes, offset + word * 8);
            let start = (address - base) as usize;
            assert_eq!(&bytes[start..start + string.len()], string);
            assert_eq!(bytes[start + string.len()], 0);
        }
        let tail: Vec<_> = (0..8)
            .map(|index| u64_at(&bytes, offset + (6 + index) * 8))
            .collect();
        assert_eq!(
            tail,
            [
                0,
                AT_PAGESZ,
                PAGE_SIZE,
                AT_ENTRY,
                USER_ADDRESS_START,
                AT_NULL,
                0,
                0
            ]
        );
        assert!(bytes[..offset].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn invalid_stack_inputs_leave_storage_unchanged() {
        let base = super::super::space::USER_STACK_BASE;
        let mut bytes = [0xab; 4096];
        for (argv, expected) in [
            (vec![b"a\0b".as_slice()], StackError::EmbeddedNull),
            (
                vec![b"x".as_slice(); MAX_ARGUMENTS + 1],
                StackError::TooManyStrings,
            ),
            (
                vec![&[b'a'; MAX_STRING_BYTES + 1][..]],
                StackError::StringTooLong,
            ),
        ] {
            assert_eq!(
                build_initial_stack(&mut bytes, base, USER_ADDRESS_START, &argv, &[]),
                Err(expected)
            );
            assert!(bytes.iter().all(|byte| *byte == 0xab));
        }
        assert_eq!(
            build_initial_stack(&mut [], base, USER_ADDRESS_START, &[], &[]),
            Err(StackError::TooSmall)
        );
        assert_eq!(
            build_initial_stack(&mut bytes, base + 1, USER_ADDRESS_START, &[], &[]),
            Err(StackError::InvalidRange)
        );
        assert_eq!(
            build_initial_stack(&mut bytes, USER_ADDRESS_END, USER_ADDRESS_START, &[], &[]),
            Err(StackError::InvalidRange)
        );
        let large = [&[b'a'; MAX_STRING_BYTES][..]; MAX_ARGUMENTS];
        assert_eq!(
            build_initial_stack(&mut bytes, base, USER_ADDRESS_START, &large, &large),
            Err(StackError::StringsTooLarge)
        );
        let shorter = [&[b'a'; 250][..]; MAX_ENVIRONMENT];
        assert_eq!(
            build_initial_stack(&mut bytes, base, USER_ADDRESS_START, &large, &shorter),
            Err(StackError::TooSmall)
        );
        assert!(bytes.iter().all(|byte| *byte == 0xab));
    }

    #[test]
    fn empty_arguments_still_supply_null_arrays_and_auxiliary_vector() {
        let base = super::super::space::USER_STACK_BASE;
        let mut bytes = [0xff; 4096];
        let stack = build_initial_stack(&mut bytes, base, USER_ADDRESS_START, &[], &[]).unwrap();
        let offset = (stack.stack_pointer - base) as usize;
        assert_eq!(
            (0..9)
                .map(|index| u64_at(&bytes, offset + index * 8))
                .collect::<Vec<_>>(),
            [
                0,
                0,
                0,
                AT_PAGESZ,
                PAGE_SIZE,
                AT_ENTRY,
                USER_ADDRESS_START,
                AT_NULL,
                0
            ]
        );
    }

    #[test]
    fn independently_linked_application_fixtures_have_distinct_permissions_and_bss() {
        for image in [
            include_bytes!("../../../userspace/images/init.elf").as_slice(),
            include_bytes!("../../../userspace/images/child.elf").as_slice(),
            include_bytes!("../../../userspace/images/exec.elf").as_slice(),
        ] {
            let plan = ElfPlan::parse(image).unwrap();
            assert_eq!(plan.entry, USER_ADDRESS_START);
            assert_eq!(plan.page_count, 3);
            assert_eq!(plan.segments().len(), 3);
            assert_eq!(
                (plan.segments()[0].writable, plan.segments()[0].executable),
                (false, true)
            );
            assert_eq!(
                (plan.segments()[1].writable, plan.segments()[1].executable),
                (false, false)
            );
            let data = plan.segments()[2];
            assert!(data.writable && !data.executable);
            assert!(data.memory_size > data.file_size as u64);
            let mut page = [0xff; 4096];
            data.copy_page(image, 0, &mut page).unwrap();
            let data_offset = (data.virtual_address - data.page_start) as usize;
            assert!(
                page[data_offset + data.file_size..]
                    .iter()
                    .all(|byte| *byte == 0)
            );
            assert_eq!(
                &page[data_offset..data_offset + data.file_size],
                &image[data.file_offset..data.file_offset + data.file_size]
            );
            let mut stack_bytes = [0xff; 4096];
            let stack = build_initial_stack(
                &mut stack_bytes,
                super::super::space::USER_STACK_BASE,
                plan.entry,
                &[b"fixture"],
                &[b"X=1"],
            )
            .unwrap();
            assert_eq!(stack.stack_pointer % 16, 0);
        }
    }

    #[test]
    fn actual_initramfs_mount_exposes_exact_independent_elf_payloads() {
        let mut filesystem = std::boxed::Box::new(crate::fs::FileSystem::new());
        filesystem.initialize().unwrap();
        filesystem
            .mount_tar(include_bytes!("../../../userspace/images/initramfs.tar"))
            .unwrap();
        let mut process = crate::fs::ProcessFs::new();
        filesystem.attach_process(&mut process).unwrap();
        for (path, expected) in [
            (
                b"/init/bin/init".as_slice(),
                include_bytes!("../../../userspace/images/init.elf").as_slice(),
            ),
            (
                b"/init/bin/child".as_slice(),
                include_bytes!("../../../userspace/images/child.elf").as_slice(),
            ),
            (
                b"/init/bin/exec".as_slice(),
                include_bytes!("../../../userspace/images/exec.elf").as_slice(),
            ),
        ] {
            let bytes = filesystem.file_bytes(&process, path).unwrap();
            assert_eq!(bytes, expected);
            assert_eq!(ElfPlan::parse(bytes).unwrap().page_count, 3);
        }
        assert_eq!(
            filesystem
                .file_bytes(&process, b"/init/etc/message")
                .unwrap(),
            b"AnanOS initramfs\n"
        );
        filesystem.cleanup_process(&mut process);
    }
}
