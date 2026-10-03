//! Allocation-free discovery of physical RAM and firmware-owned memory.
//!
//! DTSpec v17 structure/header and reserved-memory bindings:
//! https://devicetree-specification.readthedocs.io/en/latest/chapter5-flattened-format.html
//! https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html
//! Firmware supplies the final memory sizes; the unmodified Pi DTS has placeholders.
//! This parser never dereferences a firmware pointer. The caller must first obtain a
//! bounded, readable slice and also reserve the physical DTB itself.

use super::region::{RangeError, Region, Regions};

const HEADER_SIZE: usize = 40;
const MAX_DEPTH: usize = 32;
const MAX_DYNAMIC_RESERVATIONS: usize = 8;
const MAGIC: u32 = 0xd00d_feed;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

#[derive(Debug)]
pub struct MemoryInfo {
    pub ram: Regions<16>,
    pub reserved: Regions<64>,
    /// Header totalsize, including padding: reserve this many bytes at the DTB address.
    pub total_size: usize,
    dynamic: [DynamicReservation; MAX_DYNAMIC_RESERVATIONS],
    dynamic_len: usize,
    reservations_ready: bool,
}

impl MemoryInfo {
    /// Parsed requests are retained for diagnostics after successful placement.
    pub fn dynamic_reservations(&self) -> &[DynamicReservation] {
        &self.dynamic[..self.dynamic_len]
    }

    /// The caller must resolve requests after reserving its own memory and before
    /// handing the RAM map to a physical allocator, even if there are no pools.
    pub const fn reservations_ready(&self) -> bool {
        self.reservations_ready
    }

    fn push_dynamic(&mut self, reservation: DynamicReservation) -> Result<(), DtbError> {
        let entry = self
            .dynamic
            .get_mut(self.dynamic_len)
            .ok_or(DtbError::Capacity)?;
        *entry = reservation;
        self.dynamic_len += 1;
        Ok(())
    }
}

#[derive(Debug)]
pub struct DynamicReservation {
    pub size: usize,
    pub alignment: usize,
    pub alloc_ranges: Regions<16>,
}

impl DynamicReservation {
    const fn new() -> Self {
        Self {
            size: 0,
            alignment: 1,
            alloc_ranges: Regions::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtbError {
    Truncated,
    InvalidMagic,
    UnsupportedVersion,
    InvalidLayout,
    InvalidStructure,
    DepthExceeded,
    InvalidProperty,
    DuplicateProperty,
    MissingCells,
    UnsupportedCells,
    UnsupportedMemory,
    UnsupportedReservation,
    InvalidRange,
    Overflow,
    Capacity,
    OverlappingMemory,
    NoMemory,
    NoReservationSpace,
    AlreadyResolved,
}

impl From<RangeError> for DtbError {
    fn from(error: RangeError) -> Self {
        match error {
            RangeError::InvalidRange => Self::InvalidRange,
            RangeError::Overflow => Self::Overflow,
            RangeError::Capacity => Self::Capacity,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Root,
    Reserved,
    Reservation,
    Chosen,
    Other,
}

#[derive(Clone, Copy)]
struct Node<'a> {
    kind: Kind,
    memory_name: bool,
    inherited_enabled: bool,
    status: Option<bool>,
    children: bool,
    address_cells: Option<u32>,
    size_cells: Option<u32>,
    memory_type: Option<bool>,
    reg: Option<&'a [u8]>,
    ranges: Option<&'a [u8]>,
    size: Option<&'a [u8]>,
    alignment: Option<&'a [u8]>,
    alloc_ranges: Option<&'a [u8]>,
    initrd_start: Option<&'a [u8]>,
    initrd_end: Option<&'a [u8]>,
    no_map: bool,
    reusable: bool,
    usable_memory: bool,
}

impl<'a> Node<'a> {
    const EMPTY: Self = Self {
        kind: Kind::Other,
        memory_name: false,
        inherited_enabled: true,
        status: None,
        children: false,
        address_cells: None,
        size_cells: None,
        memory_type: None,
        reg: None,
        ranges: None,
        size: None,
        alignment: None,
        alloc_ranges: None,
        initrd_start: None,
        initrd_end: None,
        no_map: false,
        reusable: false,
        usable_memory: false,
    };

    fn enabled(self) -> bool {
        self.inherited_enabled && self.status.unwrap_or(true)
    }

    fn cells(self) -> Result<(usize, usize), DtbError> {
        let address = self.address_cells.ok_or(DtbError::MissingCells)?;
        let size = self.size_cells.ok_or(DtbError::MissingCells)?;
        if !(1..=2).contains(&address) || !(1..=2).contains(&size) {
            return Err(DtbError::UnsupportedCells);
        }
        Ok((address as usize, size as usize))
    }

    fn property(&mut self, name: &[u8], value: &'a [u8]) -> Result<(), DtbError> {
        match name {
            b"#address-cells" => set_once(&mut self.address_cells, single_u32(value)?)?,
            b"#size-cells" => set_once(&mut self.size_cells, single_u32(value)?)?,
            b"status" => {
                let status = single_string(value)?;
                let enabled = match status {
                    b"okay" | b"ok" => true,
                    b"disabled" | b"reserved" | b"fail" => false,
                    _ if status.starts_with(b"fail-") => false,
                    _ => return Err(DtbError::InvalidProperty),
                };
                set_once(&mut self.status, enabled)?;
            }
            b"device_type" => {
                set_once(&mut self.memory_type, single_string(value)? == b"memory")?;
            }
            b"reg" => set_once(&mut self.reg, value)?,
            b"ranges" => set_once(&mut self.ranges, value)?,
            b"size" => set_once(&mut self.size, value)?,
            b"alignment" => set_once(&mut self.alignment, value)?,
            b"alloc-ranges" => set_once(&mut self.alloc_ranges, value)?,
            b"linux,initrd-start" => set_once(&mut self.initrd_start, value)?,
            b"linux,initrd-end" => set_once(&mut self.initrd_end, value)?,
            b"no-map" | b"reusable" if self.kind == Kind::Reservation => {
                if !value.is_empty() {
                    return Err(DtbError::InvalidProperty);
                }
                let flag = if name == b"no-map" {
                    &mut self.no_map
                } else {
                    &mut self.reusable
                };
                if *flag {
                    return Err(DtbError::DuplicateProperty);
                }
                *flag = true;
            }
            b"linux,usable-memory" | b"linux,usable-memory-range" => {
                // Crash-kernel memory restrictions need deliberate handling, not
                // silent fallback to unrestricted reg banks.
                self.usable_memory = true;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Parse a v17-compatible DTB. Malformed or ambiguous memory metadata produces
/// an error; callers must keep their allocator disabled on error.
///
/// Only direct root memory nodes and identity-addressed /reserved-memory are
/// supported. Enabled dynamic reservations are recorded for a later call to
/// resolve_dynamic, after runtime-owned regions have been excluded. A dynamic
/// pool without explicit alloc-ranges cannot be safely located and is rejected.
pub fn parse(blob: &[u8]) -> Result<MemoryInfo, DtbError> {
    if blob.len() < HEADER_SIZE {
        return Err(DtbError::Truncated);
    }
    if read_u32(blob, 0)? != MAGIC {
        return Err(DtbError::InvalidMagic);
    }
    let total_size = read_u32(blob, 4)? as usize;
    if total_size < HEADER_SIZE {
        return Err(DtbError::InvalidLayout);
    }
    let blob = blob.get(..total_size).ok_or(DtbError::Truncated)?;
    let version = read_u32(blob, 20)?;
    let compatible = read_u32(blob, 24)?;
    if version < 17 || compatible > 17 || compatible > version {
        return Err(DtbError::UnsupportedVersion);
    }
    let structure_start = read_u32(blob, 8)? as usize;
    let strings_start = read_u32(blob, 12)? as usize;
    let reservation_start = read_u32(blob, 16)? as usize;
    let strings_size = read_u32(blob, 32)? as usize;
    let structure_size = read_u32(blob, 36)? as usize;
    let structure_end = block_end(structure_start, structure_size, total_size)?;
    let strings_end = block_end(strings_start, strings_size, total_size)?;
    if structure_start % 4 != 0
        || structure_size % 4 != 0
        || structure_size < 16
        || reservation_start < HEADER_SIZE
        || reservation_start % 8 != 0
        || reservation_start >= total_size
        || overlaps(structure_start, structure_end, strings_start, strings_end)
        || (structure_start..structure_end).contains(&reservation_start)
        || (strings_start..strings_end).contains(&reservation_start)
    {
        return Err(DtbError::InvalidLayout);
    }
    let mut info = MemoryInfo {
        ram: Regions::new(),
        reserved: Regions::new(),
        total_size,
        dynamic: [const { DynamicReservation::new() }; MAX_DYNAMIC_RESERVATIONS],
        dynamic_len: 0,
        reservations_ready: false,
    };
    // The reservation list has no size header. Stop before either known block,
    // so a missing terminator can never consume structure/string bytes.
    let mut reservation_limit = total_size;
    for start in [structure_start, strings_start] {
        if start > reservation_start {
            reservation_limit = reservation_limit.min(start);
        }
    }
    let mut offset = reservation_start;
    loop {
        let end = offset.checked_add(16).ok_or(DtbError::Overflow)?;
        if end > reservation_limit {
            return Err(DtbError::InvalidLayout);
        }
        let address = read_u64(blob, offset)?;
        let size = read_u64(blob, offset + 8)?;
        offset = end;
        if address == 0 && size == 0 {
            break;
        }
        let region = region_from_u64(address, size)?;
        if info
            .reserved
            .as_slice()
            .iter()
            .any(|old| old.overlaps(region))
        {
            return Err(DtbError::InvalidRange);
        }
        info.reserved.push(region)?;
    }
    let structure = &blob[structure_start..structure_end];
    let strings = &blob[strings_start..strings_end];
    parse_structure(structure, strings, &mut info)?;
    if info.ram.as_slice().is_empty() {
        return Err(DtbError::NoMemory);
    }
    Ok(info)
}

/// Place each dynamic reserved-memory request in an aligned, contiguous range
/// contained in both a single RAM bank and one of the request's alloc-ranges.
///
/// Call this exactly once after adding kernel, bootloader, DTB, framebuffer,
/// firmware and MMIO exclusions, and before initializing a live page allocator.
/// DTSpec dynamic reservations are allocated by the OS; alloc-ranges describes
/// allowed placement, not memory already owned by firmware. The first usable
/// placement in DTB bank/range order is chosen without Linux-specific exceptions.
/// All pools remain excluded, including reusable pools, until their owners exist.
///
/// Failure leaves the reserved map and resolution state unchanged. A successful
/// second call is rejected rather than assigning additional memory to each pool.
pub fn resolve_dynamic(info: &mut MemoryInfo) -> Result<(), DtbError> {
    if info.reservations_ready {
        return Err(DtbError::AlreadyResolved);
    }
    if info.reserved.as_slice().len() + info.dynamic_len > 64 {
        return Err(DtbError::Capacity);
    }
    // Stage the small range list; never construct page-sized temporary bitmaps.
    let mut staged = Regions::<64>::new();
    for region in info.reserved.as_slice() {
        staged.push(*region)?;
    }
    for pool in info.dynamic_reservations() {
        let placement = find_placement(pool, info.ram.as_slice(), staged.as_slice())
            .ok_or(DtbError::NoReservationSpace)?;
        staged.push(placement)?;
    }
    info.reserved = staged;
    info.reservations_ready = true;
    Ok(())
}

fn find_placement(
    pool: &DynamicReservation,
    ram: &[Region],
    reserved: &[Region],
) -> Option<Region> {
    for bank in ram {
        for allowed in pool.alloc_ranges.as_slice() {
            let start = bank.start.max(allowed.start);
            let limit = bank.end.min(allowed.end);
            let Some(mut candidate) = align_up(start, pool.alignment) else {
                continue;
            };
            while let Some(end) = candidate.checked_add(pool.size) {
                if end > limit {
                    break;
                }
                let placement = Region {
                    start: candidate,
                    end,
                };
                let mut blocked_end: Option<usize> = None;
                for region in reserved {
                    if region.overlaps(placement) {
                        blocked_end = Some(blocked_end.unwrap_or(0).max(region.end));
                    }
                }
                let Some(blocked_end) = blocked_end else {
                    return Some(placement);
                };
                // Every conflict advances past at least one reservation's end;
                // this loop is bounded by the number of reservation ranges.
                let Some(next) = align_up(blocked_end, pool.alignment) else {
                    break;
                };
                candidate = next;
            }
        }
    }
    None
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    let padding = value.wrapping_neg() & (alignment - 1);
    value.checked_add(padding)
}

fn parse_structure(
    structure: &[u8],
    strings: &[u8],
    info: &mut MemoryInfo,
) -> Result<(), DtbError> {
    let mut nodes = [Node::EMPTY; MAX_DEPTH];
    let mut depth = 0;
    let mut offset = 0;
    let mut root_seen = false;
    let mut reserved_seen = false;
    let mut chosen_seen = false;
    let mut root_cells = None;
    while offset < structure.len() {
        let token = read_u32(structure, offset)?;
        offset += 4;
        match token {
            BEGIN_NODE => {
                if depth == MAX_DEPTH {
                    return Err(DtbError::DepthExceeded);
                }
                let (name, after_name) = c_string(structure, offset)?;
                offset = padded_end(structure, after_name)?;
                let mut node = Node::EMPTY;
                if depth == 0 {
                    if root_seen || !name.is_empty() {
                        return Err(DtbError::InvalidStructure);
                    }
                    root_seen = true;
                    node.kind = Kind::Root;
                } else {
                    if name.is_empty() || name.contains(&b'/') || !name.is_ascii() {
                        return Err(DtbError::InvalidStructure);
                    }
                    let parent = &mut nodes[depth - 1];
                    node.inherited_enabled = parent.enabled();
                    parent.children = true;
                    if parent.kind == Kind::Root {
                        root_cells = Some(parent.cells()?);
                        node.memory_name = unit_name(name) == b"memory";
                        match unit_name(name) {
                            b"reserved-memory" => {
                                if reserved_seen || name != b"reserved-memory" {
                                    return Err(DtbError::InvalidStructure);
                                }
                                reserved_seen = true;
                                node.kind = Kind::Reserved;
                            }
                            b"chosen" => {
                                if chosen_seen || (name != b"chosen" && name != b"chosen@0") {
                                    return Err(DtbError::InvalidStructure);
                                }
                                chosen_seen = true;
                                node.kind = Kind::Chosen;
                            }
                            _ => {}
                        }
                    } else if parent.kind == Kind::Reserved {
                        if parent.enabled() {
                            validate_reserved_parent(*parent, root_cells)?;
                        }
                        node.kind = Kind::Reservation;
                    } else if parent.kind == Kind::Reservation && parent.enabled() {
                        return Err(DtbError::UnsupportedReservation);
                    }
                }
                nodes[depth] = node;
                depth += 1;
            }
            END_NODE => {
                if depth == 0 {
                    return Err(DtbError::InvalidStructure);
                }
                depth -= 1;
                finish_node(nodes[depth], depth, root_cells, info)?;
            }
            PROP => {
                if depth == 0 || nodes[depth - 1].children {
                    return Err(DtbError::InvalidStructure);
                }
                let size = read_u32(structure, offset)? as usize;
                let name_offset = read_u32(structure, offset + 4)? as usize;
                offset += 8;
                let end = offset.checked_add(size).ok_or(DtbError::Overflow)?;
                let value = structure.get(offset..end).ok_or(DtbError::Truncated)?;
                offset = padded_end(structure, end)?;
                let (name, _) = c_string(strings, name_offset)?;
                if name.is_empty() || !name.is_ascii() {
                    return Err(DtbError::InvalidProperty);
                }
                nodes[depth - 1].property(name, value)?;
            }
            NOP => {}
            END => {
                if !root_seen || depth != 0 || offset != structure.len() {
                    return Err(DtbError::InvalidStructure);
                }
                return Ok(());
            }
            _ => return Err(DtbError::InvalidStructure),
        }
    }
    Err(DtbError::InvalidStructure)
}

fn finish_node(
    node: Node<'_>,
    depth: usize,
    root_cells: Option<(usize, usize)>,
    info: &mut MemoryInfo,
) -> Result<(), DtbError> {
    if node.kind == Kind::Root {
        node.cells()?;
    }
    if !node.enabled() {
        return Ok(());
    }
    match node.kind {
        Kind::Reserved => validate_reserved_parent(node, root_cells)?,
        Kind::Reservation => {
            if node.no_map && node.reusable {
                return Err(DtbError::InvalidProperty);
            }
            let cells = root_cells.ok_or(DtbError::MissingCells)?;
            if let Some(reg) = node.reg {
                // DTSpec says reg takes precedence over dynamic size/alignment.
                read_regions(reg, cells, &mut info.reserved, false)?;
            } else {
                info.push_dynamic(parse_dynamic(node, cells)?)?;
            }
        }
        Kind::Chosen => {
            if node.usable_memory {
                return Err(DtbError::UnsupportedMemory);
            }
            match (node.initrd_start, node.initrd_end) {
                (None, None) => {}
                (Some(start), Some(end)) => {
                    // Linux firmware convention permits either 32-bit or 64-bit
                    // address properties. The end address is exclusive.
                    let start = address_property(start)?;
                    let end = address_property(end)?;
                    info.reserved.push(Region::new(start, end)?)?;
                }
                _ => return Err(DtbError::InvalidProperty),
            }
        }
        _ if node.memory_name || node.memory_type == Some(true) => {
            if depth != 1
                || !node.memory_name
                || node.memory_type != Some(true)
                || node.children
                || node.usable_memory
            {
                return Err(DtbError::UnsupportedMemory);
            }
            let reg = node.reg.ok_or(DtbError::InvalidProperty)?;
            read_regions(
                reg,
                root_cells.ok_or(DtbError::MissingCells)?,
                &mut info.ram,
                true,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_reserved_parent(
    node: Node<'_>,
    root_cells: Option<(usize, usize)>,
) -> Result<(), DtbError> {
    if node.cells()? != root_cells.ok_or(DtbError::MissingCells)? || node.ranges != Some(&[]) {
        return Err(DtbError::UnsupportedReservation);
    }
    Ok(())
}

fn parse_dynamic(node: Node<'_>, cells: (usize, usize)) -> Result<DynamicReservation, DtbError> {
    let size = cell_value(node.size.ok_or(DtbError::UnsupportedReservation)?, cells.1)?;
    let alignment = match node.alignment {
        Some(value) => cell_value(value, cells.1)?,
        None => 1,
    };
    if size == 0 || !alignment.is_power_of_two() {
        return Err(DtbError::InvalidProperty);
    }
    let ranges = node.alloc_ranges.ok_or(DtbError::UnsupportedReservation)?;
    let mut reservation = DynamicReservation {
        size,
        alignment,
        alloc_ranges: Regions::new(),
    };
    read_regions(ranges, cells, &mut reservation.alloc_ranges, false)?;
    Ok(reservation)
}

fn read_regions<const N: usize>(
    value: &[u8],
    cells: (usize, usize),
    output: &mut Regions<N>,
    reject_overlap: bool,
) -> Result<(), DtbError> {
    let tuple_size = (cells.0 + cells.1) * 4;
    if value.is_empty() || value.len() % tuple_size != 0 {
        return Err(DtbError::InvalidProperty);
    }
    for tuple in value.chunks_exact(tuple_size) {
        let region = decode_region(tuple, cells)?;
        if reject_overlap && output.as_slice().iter().any(|old| old.overlaps(region)) {
            return Err(DtbError::OverlappingMemory);
        }
        output.push(region)?;
    }
    Ok(())
}

fn decode_region(value: &[u8], cells: (usize, usize)) -> Result<Region, DtbError> {
    let split = cells.0 * 4;
    let address = cell_value(&value[..split], cells.0)?;
    let size = cell_value(&value[split..], cells.1)?;
    Ok(Region::from_size(address, size)?)
}

fn cell_value(value: &[u8], count: usize) -> Result<usize, DtbError> {
    if !(1..=2).contains(&count) || value.len() != count * 4 {
        return Err(DtbError::InvalidProperty);
    }
    let value = if count == 1 {
        read_u32(value, 0)? as u64
    } else {
        read_u64(value, 0)?
    };
    usize::try_from(value).map_err(|_| DtbError::Overflow)
}

fn address_property(value: &[u8]) -> Result<usize, DtbError> {
    if value.len() != 4 && value.len() != 8 {
        return Err(DtbError::InvalidProperty);
    }
    cell_value(value, value.len() / 4)
}

fn region_from_u64(address: u64, size: u64) -> Result<Region, DtbError> {
    let address = usize::try_from(address).map_err(|_| DtbError::Overflow)?;
    let size = usize::try_from(size).map_err(|_| DtbError::Overflow)?;
    Ok(Region::from_size(address, size)?)
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), DtbError> {
    if slot.is_some() {
        return Err(DtbError::DuplicateProperty);
    }
    *slot = Some(value);
    Ok(())
}

fn single_u32(value: &[u8]) -> Result<u32, DtbError> {
    if value.len() != 4 {
        return Err(DtbError::InvalidProperty);
    }
    read_u32(value, 0)
}

fn single_string(value: &[u8]) -> Result<&[u8], DtbError> {
    let (string, end) = c_string(value, 0)?;
    if end != value.len() || !string.is_ascii() {
        return Err(DtbError::InvalidProperty);
    }
    Ok(string)
}

fn unit_name(name: &[u8]) -> &[u8] {
    &name[..name
        .iter()
        .position(|byte| *byte == b'@')
        .unwrap_or(name.len())]
}

fn c_string(bytes: &[u8], start: usize) -> Result<(&[u8], usize), DtbError> {
    let remaining = bytes.get(start..).ok_or(DtbError::Truncated)?;
    let length = remaining
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(DtbError::Truncated)?;
    Ok((&remaining[..length], start + length + 1))
}

fn padded_end(bytes: &[u8], end: usize) -> Result<usize, DtbError> {
    let padded = end.checked_add(3).ok_or(DtbError::Overflow)? & !3;
    let padding = bytes.get(end..padded).ok_or(DtbError::Truncated)?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(DtbError::InvalidStructure);
    }
    Ok(padded)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, DtbError> {
    let end = offset.checked_add(4).ok_or(DtbError::Overflow)?;
    let value = bytes.get(offset..end).ok_or(DtbError::Truncated)?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, DtbError> {
    let end = offset.checked_add(8).ok_or(DtbError::Overflow)?;
    let value = bytes.get(offset..end).ok_or(DtbError::Truncated)?;
    Ok(u64::from_be_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn block_end(start: usize, size: usize, total: usize) -> Result<usize, DtbError> {
    let end = start.checked_add(size).ok_or(DtbError::Overflow)?;
    if start < HEADER_SIZE || end > total {
        return Err(DtbError::InvalidLayout);
    }
    Ok(end)
}

fn overlaps(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start < b_end && b_start < a_end
}

#[cfg(test)]
mod tests {
    use super::super::page::{PAGE_SIZE, PageAllocator, PageError};
    use super::*;
    use std::vec::Vec;

    struct Builder {
        structure: Vec<u8>,
        strings: Vec<u8>,
        reservations: Vec<(u64, u64)>,
    }

    impl Builder {
        fn new(address: u32, size: u32) -> Self {
            let mut builder = Self {
                structure: Vec::new(),
                strings: Vec::new(),
                reservations: Vec::new(),
            };
            builder.begin("");
            builder.prop("#address-cells", &address.to_be_bytes());
            builder.prop("#size-cells", &size.to_be_bytes());
            builder
        }

        fn token(&mut self, token: u32) {
            self.structure.extend_from_slice(&token.to_be_bytes());
        }

        fn begin(&mut self, name: &str) {
            self.token(BEGIN_NODE);
            self.structure.extend_from_slice(name.as_bytes());
            self.structure.push(0);
            self.pad();
        }

        fn pad(&mut self) {
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }

        fn prop(&mut self, name: &str, value: &[u8]) {
            let name_offset = self.strings.len();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.token(PROP);
            self.structure
                .extend_from_slice(&(value.len() as u32).to_be_bytes());
            self.structure
                .extend_from_slice(&(name_offset as u32).to_be_bytes());
            self.structure.extend_from_slice(value);
            self.pad();
        }

        fn memory(&mut self, name: &str, values: &[u32]) {
            self.begin(name);
            self.prop("device_type", b"memory\0");
            self.prop("reg", &words(values));
            self.token(END_NODE);
        }

        fn reserved_parent(&mut self, address: u32, size: u32) {
            self.begin("reserved-memory");
            self.prop("#address-cells", &address.to_be_bytes());
            self.prop("#size-cells", &size.to_be_bytes());
            self.prop("ranges", &[]);
        }

        /// Test helper for a parent with one address cell and one size cell.
        fn pool(&mut self, name: &str, size: u32, alignment: u32, ranges: &[u32]) {
            self.begin(name);
            self.prop("size", &words(&[size]));
            self.prop("alignment", &words(&[alignment]));
            self.prop("alloc-ranges", &words(ranges));
            self.token(END_NODE);
        }

        fn finish(mut self) -> Vec<u8> {
            self.token(END_NODE);
            self.token(END);
            self.raw()
        }

        fn raw(self) -> Vec<u8> {
            let structure_start = HEADER_SIZE + (self.reservations.len() + 1) * 16;
            let strings_start = structure_start + self.structure.len();
            let total = strings_start + self.strings.len();
            let mut blob = words(&[
                MAGIC,
                total as u32,
                structure_start as u32,
                strings_start as u32,
                HEADER_SIZE as u32,
                17,
                16,
                0,
                self.strings.len() as u32,
                self.structure.len() as u32,
            ]);
            for (address, size) in self.reservations {
                blob.extend_from_slice(&address.to_be_bytes());
                blob.extend_from_slice(&size.to_be_bytes());
            }
            blob.extend_from_slice(&[0; 16]);
            blob.extend_from_slice(&self.structure);
            blob.extend_from_slice(&self.strings);
            blob
        }
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    fn basic() -> Vec<u8> {
        let mut builder = Builder::new(2, 1);
        builder.memory("memory@0", &[0, 0, 0x3b40_0000, 1, 0, 0x4000_0000]);
        builder.finish()
    }

    fn assert_error(blob: &[u8], error: DtbError) {
        assert_eq!(parse(blob).unwrap_err(), error);
    }

    fn set_header(blob: &mut [u8], field: usize, value: u32) {
        blob[field * 4..field * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn reads_pi_mixed_cells_high_ram_and_all_static_reservation_sources() {
        let mut builder = Builder::new(2, 1);
        builder.reservations.push((0x1000, 0x2000));
        builder.memory("memory@0", &[0, 0, 0x3b40_0000, 1, 0, 0x4000_0000]);
        builder.reserved_parent(2, 1);
        builder.begin("nvram@10000");
        builder.prop("reg", &words(&[0, 0x10000, 0x4000]));
        builder.prop("no-map", &[]);
        builder.token(END_NODE);
        builder.token(END_NODE);
        builder.begin("chosen");
        builder.prop("linux,initrd-start", &words(&[0x800_0000]));
        builder.prop("linux,initrd-end", &words(&[0x810_0000]));
        builder.token(END_NODE);
        let blob = builder.finish();
        let memory = parse(&blob).unwrap();
        assert_eq!(memory.total_size, blob.len());
        assert_eq!(
            memory.ram.as_slice(),
            &[
                Region {
                    start: 0,
                    end: 0x3b40_0000
                },
                Region {
                    start: 0x1_0000_0000,
                    end: 0x1_4000_0000
                }
            ]
        );
        assert_eq!(
            memory.reserved.as_slice(),
            &[
                Region {
                    start: 0x1000,
                    end: 0x3000
                },
                Region {
                    start: 0x10000,
                    end: 0x14000
                },
                Region {
                    start: 0x800_0000,
                    end: 0x810_0000
                }
            ]
        );
    }

    #[test]
    fn handles_all_supported_cell_width_combinations() {
        for address in 1..=2 {
            for size in 1..=2 {
                let mut values = Vec::new();
                if address == 2 {
                    values.push(0);
                }
                values.push(0x10000);
                if size == 2 {
                    values.push(0);
                }
                values.push(0x80000);
                let mut builder = Builder::new(address, size);
                builder.memory("memory@10000", &values);
                assert_eq!(
                    parse(&builder.finish()).unwrap().ram.as_slice(),
                    &[Region {
                        start: 0x10000,
                        end: 0x90000
                    }]
                );
            }
        }
    }

    #[test]
    fn disables_unavailable_ram_and_reserved_subtrees() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.begin("memory@bad");
        builder.prop("status", b"disabled\0");
        builder.prop("device_type", b"memory\0");
        builder.prop("reg", &[1]);
        builder.token(END_NODE);
        builder.reserved_parent(1, 1);
        builder.begin("nvram@0");
        builder.prop("status", b"disabled\0");
        builder.prop("reg", &words(&[0, 0]));
        builder.token(END_NODE);
        builder.begin("unused-pool");
        builder.prop("status", b"disabled\0");
        builder.prop("size", &words(&[0x1000]));
        builder.token(END_NODE);
        builder.token(END_NODE);
        assert!(
            parse(&builder.finish())
                .unwrap()
                .reserved
                .as_slice()
                .is_empty()
        );
    }

    #[test]
    fn places_pi_one_gib_dynamic_pool_after_runtime_kernel_reservations() {
        let mut builder = Builder::new(2, 1);
        builder.memory("memory@0", &[0, 0, 0x3b40_0000]);
        builder.reserved_parent(2, 1);
        builder.begin("linux,cma");
        builder.prop("size", &words(&[0x400_0000]));
        builder.prop("alloc-ranges", &words(&[0, 0, 0x4000_0000]));
        builder.prop("reusable", &[]);
        builder.token(END_NODE);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        assert!(memory.reserved.as_slice().is_empty());
        assert_eq!(memory.dynamic_reservations().len(), 1);
        assert_eq!(memory.dynamic_reservations()[0].alignment, 1);
        assert!(!memory.reservations_ready());
        memory
            .reserved
            .push(Region::new(0, 0x180_0000).unwrap())
            .unwrap();
        resolve_dynamic(&mut memory).unwrap();
        assert_eq!(
            memory.reserved.as_slice(),
            &[
                Region {
                    start: 0,
                    end: 0x180_0000
                },
                Region {
                    start: 0x180_0000,
                    end: 0x580_0000
                },
            ]
        );
        assert!(memory.reservations_ready());
        assert_eq!(resolve_dynamic(&mut memory), Err(DtbError::AlreadyResolved));
        assert_eq!(memory.reserved.as_slice().len(), 2);
    }

    #[test]
    fn static_reg_takes_precedence_over_dynamic_parameters() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.reserved_parent(1, 1);
        builder.begin("pool@1000");
        builder.prop("reg", &words(&[0x1000, 0x2000]));
        builder.prop("size", &[]);
        builder.prop("alignment", &[]);
        builder.token(END_NODE);
        builder.token(END_NODE);
        assert_eq!(
            parse(&builder.finish()).unwrap().reserved.as_slice(),
            &[Region {
                start: 0x1000,
                end: 0x3000
            }]
        );
    }

    #[test]
    fn rejects_unlocatable_dynamic_reservation_and_invalid_alignment() {
        for alignment in [0, 3, 0x1000] {
            let mut builder = Builder::new(1, 1);
            builder.memory("memory@0", &[0, 0x100000]);
            builder.reserved_parent(1, 1);
            builder.begin("pool");
            builder.prop("size", &words(&[0x1000]));
            builder.prop("alignment", &words(&[alignment]));
            builder.token(END_NODE);
            builder.token(END_NODE);
            assert_error(
                &builder.finish(),
                if alignment == 0x1000 {
                    DtbError::UnsupportedReservation
                } else {
                    DtbError::InvalidProperty
                },
            );
        }
    }

    #[test]
    fn rejects_dynamic_pool_that_cannot_fit_after_alignment() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.reserved_parent(1, 1);
        builder.begin("pool");
        builder.prop("size", &words(&[0x1000]));
        builder.prop("alignment", &words(&[0x1000]));
        builder.prop("alloc-ranges", &words(&[0x1800, 0x1000]));
        builder.token(END_NODE);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        assert_eq!(
            resolve_dynamic(&mut memory),
            Err(DtbError::NoReservationSpace)
        );
        assert!(memory.reserved.as_slice().is_empty());
        assert!(!memory.reservations_ready());
    }

    #[test]
    fn rejects_translated_or_missing_reserved_memory_ranges() {
        for ranges in [None, Some(&[0u8; 12][..])] {
            let mut builder = Builder::new(1, 1);
            builder.memory("memory@0", &[0, 0x100000]);
            builder.begin("reserved-memory");
            builder.prop("#address-cells", &words(&[1]));
            builder.prop("#size-cells", &words(&[1]));
            if let Some(ranges) = ranges {
                builder.prop("ranges", ranges);
            }
            builder.token(END_NODE);
            assert_error(&builder.finish(), DtbError::UnsupportedReservation);
        }
    }

    #[test]
    fn rejects_missing_or_unsupported_root_cell_metadata() {
        let mut builder = Builder {
            structure: Vec::new(),
            strings: Vec::new(),
            reservations: Vec::new(),
        };
        builder.begin("");
        builder.memory("memory@0", &[0, 0x100000]);
        assert_error(&builder.finish(), DtbError::MissingCells);
        let mut builder = Builder::new(3, 1);
        builder.memory("memory@0", &[0, 0, 0, 0x100000]);
        assert_error(&builder.finish(), DtbError::UnsupportedCells);
    }

    #[test]
    fn rejects_duplicate_properties_and_late_parent_properties() {
        let mut builder = Builder::new(1, 1);
        builder.prop("#address-cells", &words(&[1]));
        assert_error(&builder.finish(), DtbError::DuplicateProperty);
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.prop("#address-cells", &words(&[1]));
        assert_error(&builder.finish(), DtbError::InvalidStructure);
    }

    #[test]
    fn rejects_overlapping_empty_wrapping_or_malformed_memory() {
        for (cells, values, error) in [
            (
                (1, 1),
                words(&[0, 0x1000, 0x800, 0x1000]),
                DtbError::OverlappingMemory,
            ),
            ((1, 1), words(&[0, 0]), DtbError::InvalidRange),
            (
                (2, 2),
                words(&[u32::MAX, 0xffff_f000, 0, 0x2000]),
                DtbError::Overflow,
            ),
            ((1, 1), words(&[0]), DtbError::InvalidProperty),
        ] {
            let mut builder = Builder::new(cells.0, cells.1);
            builder.begin("memory@0");
            builder.prop("device_type", b"memory\0");
            builder.prop("reg", &values);
            builder.token(END_NODE);
            assert_error(&builder.finish(), error);
        }
    }

    #[test]
    fn rejects_memory_under_bus_instead_of_guessing_translation() {
        let mut builder = Builder::new(1, 1);
        builder.begin("bus");
        builder.memory("memory@0", &[0, 0x1000]);
        builder.token(END_NODE);
        assert_error(&builder.finish(), DtbError::UnsupportedMemory);
    }

    #[test]
    fn bounds_header_offsets_sizes_and_version_compatibility() {
        let cases = [
            (0, 0, DtbError::InvalidMagic),
            (1, 39, DtbError::InvalidLayout),
            (1, u32::MAX, DtbError::Truncated),
            (2, 41, DtbError::InvalidLayout),
            (3, u32::MAX, DtbError::InvalidLayout),
            (4, 41, DtbError::InvalidLayout),
            (5, 16, DtbError::UnsupportedVersion),
            (6, 18, DtbError::UnsupportedVersion),
            (8, u32::MAX, DtbError::InvalidLayout),
            (9, u32::MAX, DtbError::InvalidLayout),
        ];
        for (field, value, error) in cases {
            let mut blob = basic();
            set_header(&mut blob, field, value);
            assert_error(&blob, error);
        }
        let mut blob = basic();
        set_header(&mut blob, 5, 18);
        set_header(&mut blob, 6, 17);
        assert!(parse(&blob).is_ok());
    }

    #[test]
    fn rejects_reservation_list_without_terminator_and_overlapping_blocks() {
        let mut blob = basic();
        blob[40..56].copy_from_slice(&[1; 16]);
        assert_error(&blob, DtbError::InvalidLayout);
        let mut blob = basic();
        let structure_start = read_u32(&blob, 8).unwrap();
        set_header(&mut blob, 3, structure_start);
        assert_error(&blob, DtbError::InvalidLayout);
    }

    #[test]
    fn accepts_nops_but_rejects_unknown_tokens_unbalanced_tree_and_trailing_data() {
        let mut builder = Builder::new(1, 1);
        builder.token(NOP);
        builder.memory("memory@0", &[0, 0x1000]);
        assert!(parse(&builder.finish()).is_ok());
        for token in [0, END, END_NODE] {
            let mut builder = Builder::new(1, 1);
            builder.token(token);
            assert_error(&builder.finish(), DtbError::InvalidStructure);
        }
        let mut builder = Builder::new(1, 1);
        builder.token(END_NODE);
        builder.token(END);
        builder.token(NOP);
        assert_error(&builder.raw(), DtbError::InvalidStructure);
    }

    #[test]
    fn bounds_depth_ram_and_reserved_capacity() {
        let mut builder = Builder::new(1, 1);
        for _ in 0..MAX_DEPTH {
            builder.begin("bus");
        }
        assert_error(&builder.raw(), DtbError::DepthExceeded);
        let mut builder = Builder::new(1, 1);
        let mut values = Vec::new();
        for bank in 0..17 {
            values.extend_from_slice(&[bank * 0x1000, 0x1000]);
        }
        builder.memory("memory@0", &values);
        assert_error(&builder.finish(), DtbError::Capacity);
        let mut builder = Builder::new(1, 1);
        for region in 0..65 {
            builder.reservations.push((region * 0x1000, 0x1000));
        }
        builder.memory("memory@0", &[0, 0x100000]);
        assert_error(&builder.finish(), DtbError::Capacity);
    }

    #[test]
    fn rejects_partial_or_reversed_initrd_and_reserves_64_bit_initrd() {
        for (end, error) in [
            (None, DtbError::InvalidProperty),
            (Some(0x800), DtbError::InvalidRange),
        ] {
            let mut builder = Builder::new(1, 1);
            builder.memory("memory@0", &[0, 0x100000]);
            builder.begin("chosen");
            builder.prop("linux,initrd-start", &words(&[0x1000]));
            if let Some(end) = end {
                builder.prop("linux,initrd-end", &words(&[end]));
            }
            builder.token(END_NODE);
            assert_error(&builder.finish(), error);
        }
        let mut builder = Builder::new(2, 1);
        builder.memory("memory@0", &[0, 0, 0x100000]);
        builder.begin("chosen");
        builder.prop("linux,initrd-start", &words(&[1, 0]));
        builder.prop("linux,initrd-end", &words(&[1, 0x1000]));
        builder.token(END_NODE);
        assert_eq!(
            parse(&builder.finish()).unwrap().reserved.as_slice(),
            &[Region {
                start: 0x1_0000_0000,
                end: 0x1_0000_1000
            }]
        );
    }

    #[test]
    fn every_truncated_prefix_is_rejected_without_panicking() {
        let blob = basic();
        for length in 0..blob.len() {
            assert!(parse(&blob[..length]).is_err(), "accepted prefix {length}");
        }
    }

    #[test]
    fn rejects_invalid_padding_unterminated_names_and_bad_string_offsets() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x1000]);
        let mut blob = builder.finish();
        let structure_start = read_u32(&blob, 8).unwrap() as usize;
        blob[structure_start + 5] = 1; // Root-name padding.
        assert_error(&blob, DtbError::InvalidStructure);
        let mut blob = basic();
        let strings_start = read_u32(&blob, 12).unwrap() as usize;
        blob[strings_start..].fill(b'x');
        assert_error(&blob, DtbError::Truncated);
        let mut blob = basic();
        let structure_start = read_u32(&blob, 8).unwrap() as usize;
        blob[structure_start + 16..structure_start + 20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_error(&blob, DtbError::Truncated);
    }

    #[test]
    fn parsed_firmware_and_runtime_reservations_remain_excluded_through_exhaustion() {
        let page = PAGE_SIZE as u32;
        let mut builder = Builder::new(1, 1);
        // Partial header reservations exclude both touched pages.
        builder
            .reservations
            .push((u64::from(page + 1), u64::from(page)));
        builder.memory("memory@0", &[0, 16 * page, 32 * page, 16 * page]);
        builder.reserved_parent(1, 1);
        builder.begin("firmware@5001");
        builder.prop("reg", &words(&[5 * page + 1, 1]));
        builder.token(END_NODE);
        builder.begin("cma");
        builder.prop("size", &words(&[page]));
        builder.prop("alignment", &words(&[page]));
        builder.prop("alloc-ranges", &words(&[32 * page, 4 * page]));
        builder.token(END_NODE);
        builder.token(END_NODE);
        builder.begin("chosen");
        builder.prop("linux,initrd-start", &words(&[9 * page]));
        builder.prop("linux,initrd-end", &words(&[11 * page]));
        builder.token(END_NODE);
        let blob = builder.finish();
        let mut memory = parse(&blob).unwrap();
        assert!(memory.total_size < PAGE_SIZE - 5);
        // The parser has no physical location information: its caller adds
        // kernel-owned memory and the actual DTB location before initialize.
        memory
            .reserved
            .push(Region::new(3 * PAGE_SIZE, 5 * PAGE_SIZE).unwrap())
            .unwrap();
        memory
            .reserved
            .push(Region::from_size(14 * PAGE_SIZE + 5, memory.total_size).unwrap())
            .unwrap();
        resolve_dynamic(&mut memory).unwrap();
        let mut allocator = PageAllocator::<1>::new();
        allocator
            .initialize(memory.ram.as_slice(), memory.reserved.as_slice())
            .unwrap();
        let mut allocated = Vec::new();
        while let Ok(address) = allocator.alloc() {
            let allocated_page = Region::from_size(address, PAGE_SIZE).unwrap();
            assert!(
                memory.ram.as_slice().iter().any(
                    |bank| bank.start <= allocated_page.start && allocated_page.end <= bank.end
                )
            );
            assert!(
                memory
                    .reserved
                    .as_slice()
                    .iter()
                    .all(|reserved| !reserved.overlaps(allocated_page))
            );
            assert!(!allocated.contains(&address));
            allocated.push(address);
        }
        assert_eq!(allocated.len(), 22);
        assert_eq!(allocator.alloc(), Err(PageError::OutOfMemory));
        assert_eq!(allocator.stats().free_pages, 0);
        for address in allocated {
            allocator.free(address).unwrap();
        }
        assert_eq!(allocator.stats().free_pages, 22);
        // Reserved pages cannot enter the pool through an invalid free.
        for unavailable in [0, 1, 2, 3, 4, 5, 9, 10, 14, 16, 32] {
            assert_eq!(
                allocator.free(unavailable * PAGE_SIZE),
                Err(PageError::NotManaged)
            );
        }
    }

    #[test]
    fn malformed_byte_mutations_never_panic() {
        let original = basic();
        for offset in 0..original.len() {
            for value in [0, 1, 0x7f, 0xff] {
                let mut blob = original.clone();
                blob[offset] = value;
                let _ = parse(&blob);
            }
        }
    }

    #[test]
    fn dynamic_placement_preserves_exact_size_and_alignment_after_partial_conflict() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x10000]);
        builder.reserved_parent(1, 1);
        builder.pool("pool", 0x1800, 0x2000, &[0x1800, 0x7000]);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        memory
            .reserved
            .push(Region::new(0x2001, 0x2200).unwrap())
            .unwrap();
        resolve_dynamic(&mut memory).unwrap();
        assert_eq!(
            memory.reserved.as_slice()[1],
            Region {
                start: 0x4000,
                end: 0x5800
            }
        );
    }

    #[test]
    fn dynamic_pools_cannot_cross_ram_holes_or_overlap_previous_pool() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@1000", &[0x1000, 0x6000, 0x9000, 0x7000]);
        builder.reserved_parent(1, 1);
        builder.pool("large", 0x4000, 0x1000, &[0, 0x10000]);
        builder.pool("aligned", 0x1000, 0x4000, &[0x1000, 0xf000]);
        builder.pool("small", 0x1000, 0x1000, &[0, 0x10000]);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        memory
            .reserved
            .push(Region::new(0, 0x2800).unwrap())
            .unwrap();
        memory
            .reserved
            .push(Region::new(0x5000, 0x6000).unwrap())
            .unwrap();
        resolve_dynamic(&mut memory).unwrap();
        assert_eq!(
            &memory.reserved.as_slice()[2..],
            &[
                Region {
                    start: 0x9000,
                    end: 0xd000
                },
                Region {
                    start: 0x4000,
                    end: 0x5000
                },
                Region {
                    start: 0x3000,
                    end: 0x4000
                },
            ]
        );
    }

    #[test]
    fn dynamic_placement_advances_over_unsorted_overlapping_reservations() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@1000", &[0x1000, 0xf000]);
        builder.reserved_parent(1, 1);
        builder.pool("first", 0x2000, 0x1000, &[0, 0x10000]);
        builder.pool("second", 0x2000, 0x1000, &[0, 0x10000]);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        for (start, end) in [(0x4800, 0x6000), (0x1000, 0x3000), (0x2000, 0x4800)] {
            memory
                .reserved
                .push(Region::new(start, end).unwrap())
                .unwrap();
        }
        resolve_dynamic(&mut memory).unwrap();
        assert_eq!(
            &memory.reserved.as_slice()[3..],
            &[
                Region {
                    start: 0x6000,
                    end: 0x8000
                },
                Region {
                    start: 0x8000,
                    end: 0xa000
                },
            ]
        );
    }

    #[test]
    fn failure_in_later_pool_keeps_all_dynamic_reservations_uncommitted() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x10000]);
        builder.reserved_parent(1, 1);
        builder.pool("first", 0x2000, 0x1000, &[0x1000, 0x4000]);
        builder.pool("second", 0x4000, 0x1000, &[0x1000, 0x4000]);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        memory
            .reserved
            .push(Region::new(0, 0x1000).unwrap())
            .unwrap();
        let original = memory.reserved.as_slice().to_vec();
        assert_eq!(
            resolve_dynamic(&mut memory),
            Err(DtbError::NoReservationSpace)
        );
        assert_eq!(memory.reserved.as_slice(), original);
        assert!(!memory.reservations_ready());
        // Resolution failure is transactional; correcting a boot-time request
        // before any allocator exists can be retried without leaked ranges.
        memory.dynamic[1].size = 0x1000;
        resolve_dynamic(&mut memory).unwrap();
        assert_eq!(
            &memory.reserved.as_slice()[1..],
            &[
                Region {
                    start: 0x1000,
                    end: 0x3000
                },
                Region {
                    start: 0x3000,
                    end: 0x4000
                },
            ]
        );
    }

    #[test]
    fn dynamic_requests_must_fit_real_ram_even_if_allowed_range_is_large() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@1000", &[0x1000, 0x1000, 0x4000, 0x1000]);
        builder.reserved_parent(1, 1);
        builder.pool("pool", 0x2000, 0x1000, &[0, 0x10000]);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        assert_eq!(
            resolve_dynamic(&mut memory),
            Err(DtbError::NoReservationSpace)
        );
        assert!(memory.reserved.as_slice().is_empty());
    }

    #[test]
    fn bounds_dynamic_pool_and_allowed_range_capacity() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.reserved_parent(1, 1);
        for index in 0..=MAX_DYNAMIC_RESERVATIONS {
            builder.pool(&std::format!("pool{index}"), 0x1000, 0x1000, &[0, 0x100000]);
        }
        builder.token(END_NODE);
        assert_error(&builder.finish(), DtbError::Capacity);

        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.reserved_parent(1, 1);
        let mut ranges = Vec::new();
        for index in 0..17 {
            ranges.extend_from_slice(&[index * 0x1000, 0x1000]);
        }
        builder.pool("pool", 0x1000, 0x1000, &ranges);
        builder.token(END_NODE);
        assert_error(&builder.finish(), DtbError::Capacity);
    }

    #[test]
    fn reservation_capacity_failure_is_transactional_and_empty_resolution_is_once_only() {
        let mut builder = Builder::new(1, 1);
        builder.memory("memory@0", &[0, 0x100000]);
        builder.reserved_parent(1, 1);
        builder.pool("pool", 0x1000, 0x1000, &[0, 0x100000]);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        for index in 0..64 {
            memory
                .reserved
                .push(Region::new(index * 0x1000, (index + 1) * 0x1000).unwrap())
                .unwrap();
        }
        let original = memory.reserved.as_slice().to_vec();
        assert_eq!(resolve_dynamic(&mut memory), Err(DtbError::Capacity));
        assert_eq!(memory.reserved.as_slice(), original);
        assert!(!memory.reservations_ready());

        let mut memory = parse(&basic()).unwrap();
        resolve_dynamic(&mut memory).unwrap();
        assert!(memory.reservations_ready());
        assert_eq!(resolve_dynamic(&mut memory), Err(DtbError::AlreadyResolved));
        assert!(memory.reserved.as_slice().is_empty());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn dynamic_alignment_overflow_skips_window_without_wrapping() {
        let mut builder = Builder::new(2, 2);
        builder.memory(
            "memory@ffffffffffffefff",
            &[u32::MAX, 0xffff_efff, 0, 0x1000],
        );
        builder.reserved_parent(2, 2);
        builder.begin("pool");
        builder.prop("size", &words(&[0, 1]));
        builder.prop("alignment", &words(&[0, 0x1000]));
        builder.prop("alloc-ranges", &words(&[u32::MAX, 0xffff_f001, 0, 0x100]));
        builder.token(END_NODE);
        builder.token(END_NODE);
        let mut memory = parse(&builder.finish()).unwrap();
        assert_eq!(
            resolve_dynamic(&mut memory),
            Err(DtbError::NoReservationSpace)
        );
        assert!(memory.reserved.as_slice().is_empty());
    }
}
