//! Bounded, allocation-free DTB parsing for physical-memory discovery.
//!
//! Format: https://devicetree-specification.readthedocs.io/en/stable/flattened-format.html
//! RAM is taken only from root-level memory nodes. Reservations include the
//! reserve map, /reserved-memory and /chosen's initial ramdisk. The caller must
//! additionally protect the DTB itself, the kernel/stack and platform MMIO.

use super::regions::{Region, RegionError, RegionSet};

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const MAX_DEPTH: usize = 32;
const MAX_DYNAMIC_RESERVATIONS: usize = 16;
const DEFAULT_DYNAMIC_ALIGNMENT: usize = 4096;

#[derive(Debug)]
pub struct MemoryLayout {
    pub ram: RegionSet,
    pub reserved: RegionSet,
    /// Enabled reserved-memory nodes carrying `no-map`. These bytes remain
    /// reserved and must additionally stay outside the kernel's direct map.
    pub unmapped: RegionSet,
}

impl MemoryLayout {
    pub const fn new() -> Self {
        Self {
            ram: RegionSet::new(),
            reserved: RegionSet::new(),
            unmapped: RegionSet::new(),
        }
    }

    pub fn clear(&mut self) {
        self.ram.clear();
        self.reserved.clear();
        self.unmapped.clear();
    }
}

impl Default for MemoryLayout {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FdtError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    BadHeader,
    MalformedStructure,
    BadString,
    DepthExceeded,
    DuplicateProperty,
    UnsupportedCells,
    MissingRam,
    MissingReg,
    InvalidReg,
    InvalidProperty,
    UnsupportedMemory,
    UnsupportedTranslation,
    InvalidDynamicReservation,
    DynamicReservationCapacity,
    DynamicReservationUnavailable,
    InvalidInitrd,
    AddressOverflow,
    Regions(RegionError),
}

impl From<RegionError> for FdtError {
    fn from(error: RegionError) -> Self {
        Self::Regions(error)
    }
}

struct Header {
    total_size: usize,
    reserve_offset: usize,
    structure_offset: usize,
    structure_end: usize,
    strings_offset: usize,
    strings_end: usize,
    version: u32,
}

impl Header {
    fn read(blob: &[u8]) -> Result<Self, FdtError> {
        if be_u32(blob, 0)? != FDT_MAGIC {
            return Err(FdtError::BadMagic);
        }
        let total_size = be_u32(blob, 4)? as usize;
        let structure_offset = be_u32(blob, 8)? as usize;
        let strings_offset = be_u32(blob, 12)? as usize;
        let reserve_offset = be_u32(blob, 16)? as usize;
        let version = be_u32(blob, 20)?;
        let compatible = be_u32(blob, 24)?;
        if version < 16 || compatible > 17 || compatible > version {
            return Err(FdtError::UnsupportedVersion);
        }
        let header_size = if version == 16 { 36 } else { 40 };
        let strings_size = be_u32(blob, 32)? as usize;
        let structure_size = if version == 16 {
            // Version 16 has no size_dt_struct field. A compatible v16 blob
            // needs its strings block after the structure block to bound it.
            strings_offset
                .checked_sub(structure_offset)
                .ok_or(FdtError::BadHeader)?
        } else {
            be_u32(blob, 36)? as usize
        };
        if total_size < header_size || total_size > blob.len() {
            return Err(FdtError::Truncated);
        }
        let structure_end = structure_offset
            .checked_add(structure_size)
            .ok_or(FdtError::BadHeader)?;
        let strings_end = strings_offset
            .checked_add(strings_size)
            .ok_or(FdtError::BadHeader)?;
        if reserve_offset < header_size
            || reserve_offset % 8 != 0
            || reserve_offset >= total_size
            || structure_offset < header_size
            || structure_offset % 4 != 0
            || structure_size < 12
            || structure_end > total_size
            || strings_offset < header_size
            || strings_end > total_size
            || overlaps(structure_offset, structure_end, strings_offset, strings_end)
            || (structure_offset..structure_end).contains(&reserve_offset)
            || (strings_offset..strings_end).contains(&reserve_offset)
        {
            return Err(FdtError::BadHeader);
        }
        Ok(Self {
            total_size,
            reserve_offset,
            structure_offset,
            structure_end,
            strings_offset,
            strings_end,
            version,
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Root,
    Other,
    Memory,
    ReservedRoot,
    ReservedChild,
    Chosen,
}

#[derive(Clone, Copy)]
struct Node<'a> {
    kind: Kind,
    parent_address_cells: u32,
    parent_size_cells: u32,
    address_cells: u32,
    size_cells: u32,
    has_address_cells: bool,
    has_size_cells: bool,
    enabled: bool,
    children_started: bool,
    status_seen: bool,
    device_type: Option<&'a [u8]>,
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
        parent_address_cells: 2,
        parent_size_cells: 1,
        // Cell counts are NOT inherited. The generic DT defaults are 2/1;
        // reg always uses the immediate parent's counts.
        address_cells: 2,
        size_cells: 1,
        has_address_cells: false,
        has_size_cells: false,
        enabled: true,
        children_started: false,
        status_seen: false,
        device_type: None,
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

    fn property(&mut self, name: &[u8], value: &'a [u8]) -> Result<(), FdtError> {
        match name {
            b"#address-cells" => {
                if self.has_address_cells {
                    return Err(FdtError::DuplicateProperty);
                }
                self.address_cells = scalar_u32(value)?;
                self.has_address_cells = true;
            }
            b"#size-cells" => {
                if self.has_size_cells {
                    return Err(FdtError::DuplicateProperty);
                }
                self.size_cells = scalar_u32(value)?;
                self.has_size_cells = true;
            }
            b"status" => {
                if self.status_seen {
                    return Err(FdtError::DuplicateProperty);
                }
                let status = property_string(value)?;
                self.enabled &= status == b"okay" || status == b"ok";
                self.status_seen = true;
            }
            b"device_type" => set_once(&mut self.device_type, value)?,
            b"reg" => set_once(&mut self.reg, value)?,
            b"ranges" => set_once(&mut self.ranges, value)?,
            b"size" => set_once(&mut self.size, value)?,
            b"alignment" => set_once(&mut self.alignment, value)?,
            b"alloc-ranges" => set_once(&mut self.alloc_ranges, value)?,
            b"linux,initrd-start" => set_once(&mut self.initrd_start, value)?,
            b"linux,initrd-end" => set_once(&mut self.initrd_end, value)?,
            b"no-map" | b"reusable" if self.kind == Kind::ReservedChild => {
                if !value.is_empty() {
                    return Err(FdtError::InvalidProperty);
                }
                let flag = if name == b"no-map" {
                    &mut self.no_map
                } else {
                    &mut self.reusable
                };
                if *flag {
                    return Err(FdtError::DuplicateProperty);
                }
                *flag = true;
            }
            b"linux,usable-memory" | b"linux,usable-memory-range" => {
                // Crash-kernel RAM restrictions require an explicit policy;
                // ignoring them would expose memory outside the usable range.
                self.usable_memory = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_reserved_bus(&self) -> Result<(), FdtError> {
        if !self.enabled {
            return Ok(());
        }
        if !self.has_address_cells
            || !self.has_size_cells
            || self.address_cells != self.parent_address_cells
            || self.size_cells != self.parent_size_cells
            || self.ranges != Some(&[][..])
        {
            return Err(FdtError::UnsupportedTranslation);
        }
        validate_cells(self.address_cells, self.size_cells)
    }

    fn finish(
        &self,
        layout: &mut MemoryLayout,
        dynamic: &mut DynamicRequests<'a>,
    ) -> Result<(), FdtError> {
        if !self.enabled {
            return Ok(());
        }
        if self.usable_memory && matches!(self.kind, Kind::Memory | Kind::Chosen) {
            return Err(FdtError::UnsupportedMemory);
        }
        if let Some(device_type) = self.device_type {
            let device_type = property_string(device_type)?;
            if device_type == b"memory" && self.kind != Kind::Memory {
                // Generic bus address translation is outside this physical
                // parser's contract; never mistake a bus address for RAM.
                return Err(FdtError::UnsupportedTranslation);
            }
            if self.kind == Kind::Memory && device_type != b"memory" {
                return Err(FdtError::InvalidReg);
            }
        }
        match self.kind {
            Kind::Memory => parse_regions(
                self.reg.ok_or(FdtError::MissingReg)?,
                self.parent_address_cells,
                self.parent_size_cells,
                &mut layout.ram,
            ),
            Kind::ReservedRoot => self.validate_reserved_bus(),
            Kind::ReservedChild => {
                if self.no_map && self.reusable {
                    return Err(FdtError::InvalidProperty);
                }
                if let Some(reg) = self.reg {
                    // Static reg takes precedence over size. Even reusable
                    // reservations stay excluded until there is an explicit
                    // owner/reclaim mechanism in the kernel.
                    parse_regions(
                        reg,
                        self.parent_address_cells,
                        self.parent_size_cells,
                        &mut layout.reserved,
                    )?;
                    if self.no_map {
                        parse_regions(
                            reg,
                            self.parent_address_cells,
                            self.parent_size_cells,
                            &mut layout.unmapped,
                        )?;
                    }
                    Ok(())
                } else if let Some(size) = self.size {
                    validate_cells(self.parent_address_cells, self.parent_size_cells)?;
                    if size.len() != self.parent_size_cells as usize * 4 {
                        return Err(FdtError::InvalidDynamicReservation);
                    }
                    let size = usize::try_from(cells(size, self.parent_size_cells)?)
                        .map_err(|_| FdtError::AddressOverflow)?;
                    if size == 0 {
                        return Err(FdtError::InvalidDynamicReservation);
                    }
                    let alignment = if let Some(value) = self.alignment {
                        if value.len() != self.parent_size_cells as usize * 4 {
                            return Err(FdtError::InvalidDynamicReservation);
                        }
                        let alignment = usize::try_from(cells(value, self.parent_size_cells)?)
                            .map_err(|_| FdtError::AddressOverflow)?;
                        if !alignment.is_power_of_two() {
                            return Err(FdtError::InvalidDynamicReservation);
                        }
                        alignment.max(DEFAULT_DYNAMIC_ALIGNMENT)
                    } else {
                        DEFAULT_DYNAMIC_ALIGNMENT
                    };
                    dynamic.push(DynamicRequest {
                        size,
                        alignment,
                        ranges: self.alloc_ranges,
                        address_cells: self.parent_address_cells,
                        size_cells: self.parent_size_cells,
                        no_map: self.no_map,
                    })
                } else {
                    Err(FdtError::MissingReg)
                }
            }
            Kind::Chosen => match (self.initrd_start, self.initrd_end) {
                (None, None) => Ok(()),
                (Some(start), Some(end)) => {
                    let start = initrd_address(start)?;
                    let end = initrd_address(end)?;
                    if end <= start {
                        return Err(FdtError::InvalidInitrd);
                    }
                    layout.reserved.insert(Region { start, end })?;
                    Ok(())
                }
                _ => Err(FdtError::InvalidInitrd),
            },
            _ => Ok(()),
        }
    }
}

/// Parse a complete DTB; errors never yield a partially usable memory map.
pub fn parse(blob: &[u8]) -> Result<MemoryLayout, FdtError> {
    parse_with_reserved(blob, &[])
}

/// Include runtime-owned ranges before satisfying dynamic DT reservations.
///
/// Dynamic requests receive a first-fit aligned physical span. This resolves
/// the DT allocation request; it does not initialize a CMA pool or its driver.
pub fn parse_with_reserved(blob: &[u8], initial: &[Region]) -> Result<MemoryLayout, FdtError> {
    let mut layout = MemoryLayout::new();
    parse_into(blob, initial, &mut layout)?;
    Ok(layout)
}

/// Parse into caller-owned storage without copying the memory map on the boot
/// stack. Any error clears all three sets, including a previous successful map.
pub fn parse_into(
    blob: &[u8],
    initial: &[Region],
    output: &mut MemoryLayout,
) -> Result<(), FdtError> {
    output.clear();
    let result = parse_into_inner(blob, initial, output);
    if result.is_err() {
        output.clear();
    }
    result
}

fn parse_into_inner(
    blob: &[u8],
    initial: &[Region],
    layout: &mut MemoryLayout,
) -> Result<(), FdtError> {
    let header = Header::read(blob)?;
    let blob = &blob[..header.total_size];
    for region in initial {
        layout.reserved.insert(*region)?;
    }
    parse_reserve_map(blob, &header, &mut layout.reserved)?;
    let mut dynamic = DynamicRequests::new();

    let structure = &blob[header.structure_offset..header.structure_end];
    let strings = &blob[header.strings_offset..header.strings_end];
    let mut cursor = 0;
    let mut nodes = [Node::EMPTY; MAX_DEPTH];
    let mut depth = 0;
    let mut root_seen = false;
    let mut reserved_seen = false;
    let mut chosen_seen = false;
    loop {
        let token = be_u32(structure, cursor)?;
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                if depth == MAX_DEPTH {
                    return Err(FdtError::DepthExceeded);
                }
                let (name, end) = c_string(structure, cursor)?;
                cursor = padded_end(structure, end + 1)?;
                if name.contains(&b'/') || (depth != 0 && name.is_empty()) {
                    return Err(FdtError::BadString);
                }
                let mut node = Node::EMPTY;
                if depth == 0 {
                    if root_seen || !name.is_empty() {
                        return Err(FdtError::MalformedStructure);
                    }
                    node.kind = Kind::Root;
                    root_seen = true;
                } else {
                    let parent = &mut nodes[depth - 1];
                    parent.children_started = true;
                    node.parent_address_cells = parent.address_cells;
                    node.parent_size_cells = parent.size_cells;
                    node.enabled = parent.enabled;
                    let base_name = name.split(|byte| *byte == b'@').next().unwrap_or(name);
                    if parent.kind == Kind::Root {
                        node.kind = match base_name {
                            b"memory" => Kind::Memory,
                            b"reserved-memory" => {
                                if reserved_seen {
                                    return Err(FdtError::MalformedStructure);
                                }
                                reserved_seen = true;
                                Kind::ReservedRoot
                            }
                            b"chosen" => {
                                if chosen_seen {
                                    return Err(FdtError::MalformedStructure);
                                }
                                chosen_seen = true;
                                Kind::Chosen
                            }
                            _ => Kind::Other,
                        };
                    } else if parent.kind == Kind::ReservedRoot {
                        parent.validate_reserved_bus()?;
                        node.kind = Kind::ReservedChild;
                    } else if parent.kind == Kind::ReservedChild && node.enabled {
                        return Err(FdtError::UnsupportedTranslation);
                    }
                }
                nodes[depth] = node;
                depth += 1;
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return Err(FdtError::MalformedStructure);
                }
                depth -= 1;
                nodes[depth].finish(layout, &mut dynamic)?;
            }
            FDT_PROP => {
                if depth == 0 || nodes[depth - 1].children_started {
                    return Err(FdtError::MalformedStructure);
                }
                let length = be_u32(structure, cursor)? as usize;
                let name_offset = be_u32(structure, cursor + 4)? as usize;
                cursor += 8;
                let value = take(structure, cursor, length)?;
                let (name, _) = c_string(strings, name_offset)?;
                if name.is_empty() {
                    return Err(FdtError::BadString);
                }
                nodes[depth - 1].property(name, value)?;
                cursor = padded_end(
                    structure,
                    cursor.checked_add(length).ok_or(FdtError::Truncated)?,
                )?;
            }
            FDT_NOP => {}
            FDT_END => {
                if !root_seen || depth != 0 {
                    return Err(FdtError::MalformedStructure);
                }
                if header.version == 16 {
                    if structure[cursor..].iter().any(|byte| *byte != 0) {
                        return Err(FdtError::MalformedStructure);
                    }
                } else if cursor != structure.len() {
                    return Err(FdtError::MalformedStructure);
                }
                if layout.ram.as_slice().is_empty() {
                    return Err(FdtError::MissingRam);
                }
                dynamic.resolve(layout)?;
                return Ok(());
            }
            _ => return Err(FdtError::MalformedStructure),
        }
    }
}

#[derive(Clone, Copy)]
struct DynamicRequest<'a> {
    size: usize,
    alignment: usize,
    ranges: Option<&'a [u8]>,
    address_cells: u32,
    size_cells: u32,
    no_map: bool,
}

struct DynamicRequests<'a> {
    entries: [DynamicRequest<'a>; MAX_DYNAMIC_RESERVATIONS],
    len: usize,
}

impl<'a> DynamicRequests<'a> {
    fn new() -> Self {
        Self {
            entries: [DynamicRequest {
                size: 0,
                alignment: DEFAULT_DYNAMIC_ALIGNMENT,
                ranges: None,
                address_cells: 2,
                size_cells: 1,
                no_map: false,
            }; MAX_DYNAMIC_RESERVATIONS],
            len: 0,
        }
    }

    fn push(&mut self, request: DynamicRequest<'a>) -> Result<(), FdtError> {
        if self.len == MAX_DYNAMIC_RESERVATIONS {
            return Err(FdtError::DynamicReservationCapacity);
        }
        self.entries[self.len] = request;
        self.len += 1;
        Ok(())
    }

    fn resolve(&self, layout: &mut MemoryLayout) -> Result<(), FdtError> {
        for request in &self.entries[..self.len] {
            let mut ranges = RegionSet::new();
            if let Some(value) = request.ranges {
                parse_regions(
                    value,
                    request.address_cells,
                    request.size_cells,
                    &mut ranges,
                )?;
            }
            let mut chosen = None;
            'ram: for ram in layout.ram.as_slice() {
                if request.ranges.is_some() {
                    for allowed in ranges.as_slice() {
                        let start = ram.start.max(allowed.start);
                        let end = ram.end.min(allowed.end);
                        if let Some(region) =
                            find_dynamic_span(start, end, request, &layout.reserved)
                        {
                            chosen = Some(region);
                            break 'ram;
                        }
                    }
                } else if let Some(region) =
                    find_dynamic_span(ram.start, ram.end, request, &layout.reserved)
                {
                    chosen = Some(region);
                    break;
                }
            }
            let chosen = chosen.ok_or(FdtError::DynamicReservationUnavailable)?;
            layout.reserved.insert(chosen)?;
            if request.no_map {
                layout.unmapped.insert(chosen)?;
            }
        }
        Ok(())
    }
}

fn find_dynamic_span(
    start: usize,
    end: usize,
    request: &DynamicRequest<'_>,
    reserved: &RegionSet,
) -> Option<Region> {
    let align = |address: usize| {
        address
            .checked_add(request.alignment - 1)
            .map(|value| value & !(request.alignment - 1))
    };
    let mut start = align(start)?;
    let mut candidate_end = start.checked_add(request.size)?;
    if candidate_end > end {
        return None;
    }
    for excluded in reserved.as_slice() {
        if excluded.end <= start {
            continue;
        }
        if excluded.start >= candidate_end {
            break;
        }
        start = align(excluded.end)?;
        candidate_end = start.checked_add(request.size)?;
        if candidate_end > end {
            return None;
        }
    }
    Some(Region {
        start,
        end: candidate_end,
    })
}

fn parse_reserve_map(
    blob: &[u8],
    header: &Header,
    reserved: &mut RegionSet,
) -> Result<(), FdtError> {
    let mut limit = header.total_size;
    for start in [header.structure_offset, header.strings_offset] {
        if start > header.reserve_offset {
            limit = limit.min(start);
        }
    }
    let mut cursor = header.reserve_offset;
    loop {
        let entry = take(&blob[..limit], cursor, 16)?;
        let address = be_u64(entry, 0)?;
        let size = be_u64(entry, 8)?;
        if address == 0 && size == 0 {
            return Ok(());
        }
        insert_nonempty(reserved, address, size)?;
        cursor += 16;
    }
}

fn parse_regions(
    value: &[u8],
    address_cells: u32,
    size_cells: u32,
    regions: &mut RegionSet,
) -> Result<(), FdtError> {
    validate_cells(address_cells, size_cells)?;
    let address_bytes = address_cells as usize * 4;
    let entry_bytes = (address_cells + size_cells) as usize * 4;
    if value.is_empty() || value.len() % entry_bytes != 0 {
        return Err(FdtError::InvalidReg);
    }
    for entry in value.chunks_exact(entry_bytes) {
        insert_nonempty(
            regions,
            cells(entry, address_cells)?,
            cells(&entry[address_bytes..], size_cells)?,
        )?;
    }
    Ok(())
}

fn validate_cells(address_cells: u32, size_cells: u32) -> Result<(), FdtError> {
    if !(1..=2).contains(&address_cells) || !(1..=2).contains(&size_cells) {
        return Err(FdtError::UnsupportedCells);
    }
    Ok(())
}

fn cells(value: &[u8], count: u32) -> Result<u64, FdtError> {
    match count {
        1 => Ok(u64::from(be_u32(value, 0)?)),
        2 => be_u64(value, 0),
        _ => Err(FdtError::UnsupportedCells),
    }
}

fn insert_nonempty(regions: &mut RegionSet, address: u64, size: u64) -> Result<(), FdtError> {
    if size == 0 {
        return Ok(());
    }
    let end = address.checked_add(size).ok_or(FdtError::AddressOverflow)?;
    let start = usize::try_from(address).map_err(|_| FdtError::AddressOverflow)?;
    let end = usize::try_from(end).map_err(|_| FdtError::AddressOverflow)?;
    regions.insert(Region { start, end })?;
    Ok(())
}

fn initrd_address(value: &[u8]) -> Result<usize, FdtError> {
    let address = match value.len() {
        4 => u64::from(be_u32(value, 0)?),
        8 => be_u64(value, 0)?,
        _ => return Err(FdtError::InvalidInitrd),
    };
    usize::try_from(address).map_err(|_| FdtError::AddressOverflow)
}

fn scalar_u32(value: &[u8]) -> Result<u32, FdtError> {
    if value.len() != 4 {
        return Err(FdtError::MalformedStructure);
    }
    be_u32(value, 0)
}

fn set_once<'a>(slot: &mut Option<&'a [u8]>, value: &'a [u8]) -> Result<(), FdtError> {
    if slot.is_some() {
        return Err(FdtError::DuplicateProperty);
    }
    *slot = Some(value);
    Ok(())
}

fn property_string(value: &[u8]) -> Result<&[u8], FdtError> {
    let (string, end) = c_string(value, 0)?;
    if end + 1 != value.len() {
        return Err(FdtError::BadString);
    }
    Ok(string)
}

fn c_string(value: &[u8], offset: usize) -> Result<(&[u8], usize), FdtError> {
    let remaining = value.get(offset..).ok_or(FdtError::BadString)?;
    let length = remaining
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(FdtError::BadString)?;
    Ok((&remaining[..length], offset + length))
}

fn padded_end(value: &[u8], end: usize) -> Result<usize, FdtError> {
    let padded = end.checked_add(3).ok_or(FdtError::Truncated)? & !3;
    if take(value, end, padded - end)?
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(FdtError::MalformedStructure);
    }
    Ok(padded)
}

fn take(value: &[u8], offset: usize, length: usize) -> Result<&[u8], FdtError> {
    let end = offset.checked_add(length).ok_or(FdtError::Truncated)?;
    value.get(offset..end).ok_or(FdtError::Truncated)
}

fn be_u32(value: &[u8], offset: usize) -> Result<u32, FdtError> {
    Ok(u32::from_be_bytes(
        take(value, offset, 4)?
            .try_into()
            .map_err(|_| FdtError::Truncated)?,
    ))
}

fn be_u64(value: &[u8], offset: usize) -> Result<u64, FdtError> {
    Ok(u64::from_be_bytes(
        take(value, offset, 8)?
            .try_into()
            .map_err(|_| FdtError::Truncated)?,
    ))
}

fn overlaps(start: usize, end: usize, other_start: usize, other_end: usize) -> bool {
    start < other_end && other_start < end
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    struct Dtb {
        structure: Vec<u8>,
        strings: Vec<u8>,
        reservations: Vec<(u64, u64)>,
    }

    impl Dtb {
        fn new() -> Self {
            Self {
                structure: Vec::new(),
                strings: Vec::new(),
                reservations: Vec::new(),
            }
        }

        fn word(&mut self, value: u32) {
            self.structure.extend_from_slice(&value.to_be_bytes());
        }

        fn begin(&mut self, name: &str) {
            self.word(FDT_BEGIN_NODE);
            self.structure.extend_from_slice(name.as_bytes());
            self.structure.push(0);
            self.pad();
        }

        fn end(&mut self) {
            self.word(FDT_END_NODE);
        }

        fn pad(&mut self) {
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }

        fn property(&mut self, name: &str, value: &[u8]) {
            let offset = self.strings.len();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.word(FDT_PROP);
            self.word(value.len() as u32);
            self.word(offset as u32);
            self.structure.extend_from_slice(value);
            self.pad();
        }

        fn cells(&mut self, name: &str, values: &[u32]) {
            let value: Vec<u8> = values
                .iter()
                .flat_map(|value| value.to_be_bytes())
                .collect();
            self.property(name, &value);
        }

        fn root(&mut self, address_cells: u32, size_cells: u32) {
            self.begin("");
            self.cells("#address-cells", &[address_cells]);
            self.cells("#size-cells", &[size_cells]);
        }

        fn memory32(&mut self, start: u32, size: u32) {
            self.begin("memory@0");
            self.property("device_type", b"memory\0");
            self.cells("reg", &[start, size]);
            self.end();
        }

        fn reserved32(&mut self) {
            self.begin("reserved-memory");
            self.cells("#address-cells", &[1]);
            self.cells("#size-cells", &[1]);
            self.property("ranges", &[]);
        }

        fn finish(mut self) -> Vec<u8> {
            self.end();
            self.word(FDT_END);
            let reserve_offset = 40;
            let reserve_size = (self.reservations.len() + 1) * 16;
            let structure_offset = reserve_offset + reserve_size;
            let strings_offset = structure_offset + self.structure.len();
            let total_size = strings_offset + self.strings.len();
            let mut blob = Vec::new();
            for word in [
                FDT_MAGIC,
                total_size as u32,
                structure_offset as u32,
                strings_offset as u32,
                reserve_offset as u32,
                17,
                16,
                0,
                self.strings.len() as u32,
                self.structure.len() as u32,
            ] {
                blob.extend_from_slice(&word.to_be_bytes());
            }
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

    fn sample32() -> Vec<u8> {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.finish()
    }

    fn expect_error(blob: &[u8], expected: FdtError) {
        assert_eq!(parse(blob).unwrap_err(), expected);
    }

    #[test]
    fn rejects_unsupported_usable_memory_restrictions() {
        for property in ["linux,usable-memory", "linux,usable-memory-range"] {
            for node in ["memory@0", "chosen"] {
                let mut dtb = Dtb::new();
                dtb.root(1, 1);
                if node == "chosen" {
                    dtb.memory32(0, 0x10000);
                }
                dtb.begin(node);
                if node == "memory@0" {
                    dtb.property("device_type", b"memory\0");
                    dtb.cells("reg", &[0, 0x10000]);
                }
                dtb.cells(property, &[0x4000, 0x4000]);
                dtb.end();
                expect_error(&dtb.finish(), FdtError::UnsupportedMemory);
            }
        }
    }

    #[test]
    fn disabled_usable_memory_restrictions_do_not_contribute_ram() {
        for property in ["linux,usable-memory", "linux,usable-memory-range"] {
            for node in ["memory@10000", "chosen"] {
                let mut dtb = Dtb::new();
                dtb.root(1, 1);
                dtb.memory32(0, 0x10000);
                dtb.begin(node);
                dtb.property("status", b"disabled\0");
                if node == "memory@10000" {
                    dtb.property("device_type", b"memory\0");
                    dtb.cells("reg", &[0x10000, 0x10000]);
                }
                dtb.cells(property, &[0x14000, 0x4000]);
                dtb.end();
                let layout = parse(&dtb.finish()).unwrap();
                assert_eq!(
                    layout.ram.as_slice(),
                    &[Region {
                        start: 0,
                        end: 0x10000
                    }]
                );
            }
        }
    }

    #[test]
    fn reservation_flags_reject_values_and_duplicates() {
        for flag in ["no-map", "reusable"] {
            for duplicate in [false, true] {
                let mut dtb = Dtb::new();
                dtb.root(1, 1);
                dtb.memory32(0, 0x10000);
                dtb.reserved32();
                dtb.begin("pool@4000");
                dtb.cells("reg", &[0x4000, 0x1000]);
                if duplicate {
                    dtb.property(flag, &[]);
                    dtb.property(flag, &[]);
                } else {
                    dtb.cells(flag, &[1]);
                }
                dtb.end();
                dtb.end();
                expect_error(
                    &dtb.finish(),
                    if duplicate {
                        FdtError::DuplicateProperty
                    } else {
                        FdtError::InvalidProperty
                    },
                );
            }
        }
    }

    #[test]
    fn no_map_and_reusable_are_mutually_exclusive_for_static_and_dynamic_pools() {
        for dynamic in [false, true] {
            let mut dtb = Dtb::new();
            dtb.root(1, 1);
            dtb.memory32(0, 0x10000);
            dtb.reserved32();
            dtb.begin("pool");
            if dynamic {
                dtb.cells("size", &[0x1000]);
            } else {
                dtb.cells("reg", &[0x4000, 0x1000]);
            }
            dtb.property("no-map", &[]);
            dtb.property("reusable", &[]);
            dtb.end();
            dtb.end();
            expect_error(&dtb.finish(), FdtError::InvalidProperty);
        }
    }

    #[test]
    fn valid_no_map_and_reusable_pools_both_remain_reserved() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.reserved32();
        for (flag, start) in [("no-map", 0x4000), ("reusable", 0x8000)] {
            dtb.begin("pool");
            dtb.cells("reg", &[start, 0x1000]);
            dtb.property(flag, &[]);
            dtb.end();
        }
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.reserved.as_slice(),
            &[
                Region {
                    start: 0x4000,
                    end: 0x5000
                },
                Region {
                    start: 0x8000,
                    end: 0x9000
                }
            ]
        );
        assert_eq!(
            layout.unmapped.as_slice(),
            &[Region {
                start: 0x4000,
                end: 0x5000,
            }]
        );
    }

    #[test]
    fn static_no_map_retains_all_reg_spans_but_not_other_reservations() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x20000);
        dtb.reservations.push((0x6000, 0x1000));
        dtb.reserved32();
        dtb.begin("hidden");
        dtb.cells("reg", &[0x4001, 0x1234, 0x8000, 0x2000]);
        dtb.property("no-map", &[]);
        dtb.end();
        dtb.begin("ordinary");
        dtb.cells("reg", &[0xA000, 0x1000]);
        dtb.end();
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.unmapped.as_slice(),
            &[
                Region {
                    start: 0x4001,
                    end: 0x5235,
                },
                Region {
                    start: 0x8000,
                    end: 0xA000,
                }
            ]
        );
        assert_eq!(
            layout.reserved.as_slice(),
            &[
                Region {
                    start: 0x4001,
                    end: 0x5235,
                },
                Region {
                    start: 0x6000,
                    end: 0x7000,
                },
                Region {
                    start: 0x8000,
                    end: 0xB000,
                }
            ]
        );
    }

    #[test]
    fn dynamic_no_map_retains_only_the_selected_physical_span() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x20000);
        dtb.reserved32();
        dtb.begin("hidden");
        dtb.cells("size", &[0x1234]);
        dtb.cells("alloc-ranges", &[0x1000, 0xF000]);
        dtb.property("no-map", &[]);
        dtb.end();
        dtb.end();
        let initial = [Region {
            start: 0x1000,
            end: 0x4000,
        }];
        let layout = parse_with_reserved(&dtb.finish(), &initial).unwrap();
        assert_eq!(
            layout.unmapped.as_slice(),
            &[Region {
                start: 0x4000,
                end: 0x5234,
            }]
        );
        assert_eq!(
            layout.reserved.as_slice(),
            &[Region {
                start: 0x1000,
                end: 0x5234,
            }]
        );
    }

    #[test]
    fn disabled_no_map_nodes_and_subtrees_contribute_no_spans() {
        for dynamic in [false, true] {
            for disabled_parent in [false, true] {
                let mut dtb = Dtb::new();
                dtb.root(1, 1);
                dtb.memory32(0, 0x10000);
                dtb.reserved32();
                if disabled_parent {
                    dtb.property("status", b"disabled\0");
                }
                dtb.begin("hidden");
                if !disabled_parent {
                    dtb.property("status", b"disabled\0");
                }
                if dynamic {
                    dtb.cells("size", &[0x1000]);
                } else {
                    dtb.cells("reg", &[0x4000, 0x1000]);
                }
                dtb.property("no-map", &[]);
                dtb.end();
                dtb.end();
                let layout = parse(&dtb.finish()).unwrap();
                assert!(layout.reserved.as_slice().is_empty());
                assert!(layout.unmapped.as_slice().is_empty());
            }
        }
    }

    #[test]
    fn no_map_dtb_truncation_and_byte_mutations_preserve_reservation_invariants() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x20000);
        dtb.reserved32();
        dtb.begin("fixed");
        dtb.cells("reg", &[0x4001, 0x1234]);
        dtb.property("no-map", &[]);
        dtb.end();
        dtb.begin("dynamic");
        dtb.cells("size", &[0x2345]);
        dtb.cells("alignment", &[0x1000]);
        dtb.cells("alloc-ranges", &[0x1000, 0xF000]);
        dtb.property("no-map", &[]);
        dtb.end();
        dtb.end();
        let blob = dtb.finish();
        for length in 0..blob.len() {
            assert!(parse(&blob[..length]).is_err(), "accepted prefix {length}");
        }
        // Mutations may describe another valid DTB; any accepted no-map range
        // must still have a reservation owner, and malformed input must not panic.
        for index in 0..blob.len() {
            for mask in [1, 0x80, 0xFF] {
                let mut mutated = blob.clone();
                mutated[index] ^= mask;
                if let Ok(layout) = parse(&mutated) {
                    for hole in layout.unmapped.as_slice() {
                        assert!(layout.reserved.as_slice().iter().any(|reserved| {
                            reserved.start <= hole.start && hole.end <= reserved.end
                        }));
                    }
                }
            }
        }
    }

    #[test]
    fn parse_into_clears_previous_and_partially_parsed_maps_on_every_error() {
        let mut output = MemoryLayout::new();
        parse_into(&sample32(), &[], &mut output).unwrap();
        assert!(!output.ram.as_slice().is_empty());

        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.reserved32();
        dtb.begin("hidden");
        dtb.cells("reg", &[0x4000, 0x1000]);
        dtb.property("no-map", &[]);
        dtb.end();
        dtb.end();
        dtb.begin("chosen");
        // RAM, reserved and unmapped have all been populated before this node
        // fails. The parser must not expose any of those partial results.
        dtb.cells("linux,initrd-start", &[0x6000]);
        dtb.end();
        assert_eq!(
            parse_into(&dtb.finish(), &[], &mut output),
            Err(FdtError::InvalidInitrd)
        );
        assert!(output.ram.as_slice().is_empty());
        assert!(output.reserved.as_slice().is_empty());
        assert!(output.unmapped.as_slice().is_empty());

        parse_into(&sample32(), &[], &mut output).unwrap();
        assert_eq!(parse_into(&[], &[], &mut output), Err(FdtError::Truncated));
        assert!(output.ram.as_slice().is_empty());
        assert!(output.reserved.as_slice().is_empty());
        assert!(output.unmapped.as_slice().is_empty());

        assert_eq!(
            parse_into(&sample32(), &[Region { start: 2, end: 1 }], &mut output,),
            Err(FdtError::Regions(RegionError::Empty))
        );
        assert!(output.ram.as_slice().is_empty());
        assert!(output.reserved.as_slice().is_empty());
        assert!(output.unmapped.as_slice().is_empty());
    }

    #[test]
    fn parse_64_bit_ram_banks_reserve_map_and_static_reservations() {
        let mut dtb = Dtb::new();
        dtb.root(2, 2);
        dtb.begin("memory@0");
        dtb.property("device_type", b"memory\0");
        dtb.cells("reg", &[0, 0, 0, 0x10000, 1, 0, 0, 0x20000]);
        dtb.end();
        dtb.reservations.push((0x8000, 0x1000));
        dtb.begin("reserved-memory");
        dtb.cells("#address-cells", &[2]);
        dtb.cells("#size-cells", &[2]);
        dtb.property("ranges", &[]);
        dtb.begin("dma@9000");
        dtb.cells("reg", &[0, 0x9000, 0, 0x1000]);
        dtb.property("reusable", &[]);
        dtb.end();
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.ram.as_slice(),
            &[
                Region {
                    start: 0,
                    end: 0x10000
                },
                Region {
                    start: 0x1_0000_0000,
                    end: 0x1_0002_0000
                }
            ]
        );
        assert_eq!(
            layout.reserved.as_slice(),
            &[Region {
                start: 0x8000,
                end: 0xa000
            }]
        );
    }

    #[test]
    fn default_cell_counts_are_two_address_one_size() {
        let mut dtb = Dtb::new();
        dtb.begin("");
        dtb.begin("memory@0");
        dtb.cells("reg", &[0, 0x1000, 0x2000]);
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.ram.as_slice(),
            &[Region {
                start: 0x1000,
                end: 0x3000
            }]
        );
    }

    #[test]
    fn disabled_nodes_and_subtrees_do_not_claim_memory() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.begin("memory@10000");
        dtb.property("status", b"disabled\0");
        dtb.cells("reg", &[0x10000, 0x10000]);
        dtb.end();
        dtb.begin("reserved-memory");
        dtb.property("status", b"disabled\0");
        // Disabled buses need no address translation or active child request.
        dtb.begin("pool");
        dtb.cells("size", &[0x2000]);
        dtb.end();
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.ram.as_slice(),
            &[Region {
                start: 0,
                end: 0x10000
            }]
        );
        assert!(layout.reserved.as_slice().is_empty());
    }

    #[test]
    fn chosen_initrd_uses_exclusive_end_and_requires_both_addresses() {
        for wide in [false, true] {
            let mut dtb = Dtb::new();
            dtb.root(1, 1);
            dtb.memory32(0, 0x10000);
            dtb.begin("chosen");
            if wide {
                dtb.cells("linux,initrd-start", &[0, 0x4001]);
                dtb.cells("linux,initrd-end", &[0, 0x5007]);
            } else {
                dtb.cells("linux,initrd-start", &[0x4001]);
                dtb.cells("linux,initrd-end", &[0x5007]);
            }
            dtb.end();
            let layout = parse(&dtb.finish()).unwrap();
            assert_eq!(
                layout.reserved.as_slice(),
                &[Region {
                    start: 0x4001,
                    end: 0x5007
                }]
            );
        }
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.begin("chosen");
        dtb.cells("linux,initrd-start", &[0x4000]);
        dtb.end();
        expect_error(&dtb.finish(), FdtError::InvalidInitrd);
    }

    #[test]
    fn dynamic_reservation_honors_bounds_alignment_and_runtime_ownership() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x20000);
        dtb.reserved32();
        dtb.begin("pool");
        dtb.cells("size", &[0x2000]);
        dtb.cells("alignment", &[0x4000]);
        dtb.cells("alloc-ranges", &[0x3000, 0xc000]);
        dtb.property("reusable", &[]);
        dtb.end();
        dtb.end();
        let initial = [Region {
            start: 0x4000,
            end: 0x5000,
        }];
        let layout = parse_with_reserved(&dtb.finish(), &initial).unwrap();
        assert_eq!(
            layout.reserved.as_slice(),
            &[
                initial[0],
                Region {
                    start: 0x8000,
                    end: 0xa000
                }
            ]
        );
    }

    #[test]
    fn dynamic_without_alloc_ranges_uses_ram_and_avoids_static_nodes_seen_later() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0x1000, 0x9000);
        dtb.reserved32();
        dtb.begin("pool");
        dtb.cells("size", &[0x2000]);
        dtb.end();
        dtb.begin("fixed@1000");
        dtb.cells("reg", &[0x1000, 0x1000]);
        dtb.end();
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.reserved.as_slice(),
            &[Region {
                start: 0x1000,
                end: 0x4000
            }]
        );
    }

    #[test]
    fn dynamic_request_fails_when_bound_has_no_usable_aligned_span() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.reserved32();
        dtb.begin("pool");
        dtb.cells("size", &[0x3000]);
        dtb.cells("alignment", &[0x4000]);
        dtb.cells("alloc-ranges", &[0x3000, 0x2000]);
        dtb.end();
        dtb.end();
        expect_error(&dtb.finish(), FdtError::DynamicReservationUnavailable);
    }

    #[test]
    fn dynamic_alignment_must_be_nonzero_power_of_two() {
        for alignment in [0, 3, 0x3000] {
            let mut dtb = Dtb::new();
            dtb.root(1, 1);
            dtb.memory32(0, 0x10000);
            dtb.reserved32();
            dtb.begin("pool");
            dtb.cells("size", &[0x1000]);
            dtb.cells("alignment", &[alignment]);
            dtb.end();
            dtb.end();
            expect_error(&dtb.finish(), FdtError::InvalidDynamicReservation);
        }
    }

    #[test]
    fn static_reg_takes_precedence_over_dynamic_size() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.reserved32();
        dtb.begin("pool@6000");
        dtb.cells("reg", &[0x6000, 0x1000]);
        dtb.cells("size", &[0x8000]);
        dtb.end();
        dtb.end();
        let layout = parse(&dtb.finish()).unwrap();
        assert_eq!(
            layout.reserved.as_slice(),
            &[Region {
                start: 0x6000,
                end: 0x7000
            }]
        );
    }

    #[test]
    fn translated_or_incomplete_reserved_memory_bus_is_rejected() {
        for ranges in [None, Some(&[0u8, 0, 0, 0][..])] {
            let mut dtb = Dtb::new();
            dtb.root(1, 1);
            dtb.memory32(0, 0x10000);
            dtb.begin("reserved-memory");
            dtb.cells("#address-cells", &[1]);
            dtb.cells("#size-cells", &[1]);
            if let Some(ranges) = ranges {
                dtb.property("ranges", ranges);
            }
            dtb.end();
            expect_error(&dtb.finish(), FdtError::UnsupportedTranslation);
        }
    }

    #[test]
    fn unsupported_cells_and_truncated_reg_are_rejected() {
        let mut dtb = Dtb::new();
        dtb.root(3, 1);
        dtb.memory32(0, 0x10000);
        expect_error(&dtb.finish(), FdtError::UnsupportedCells);
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.begin("memory@0");
        dtb.cells("reg", &[0]);
        dtb.end();
        expect_error(&dtb.finish(), FdtError::InvalidReg);
    }

    #[test]
    fn overflowing_64_bit_ram_is_rejected() {
        let mut dtb = Dtb::new();
        dtb.root(2, 2);
        dtb.begin("memory@fffffffffffff000");
        dtb.cells("reg", &[u32::MAX, 0xffff_f000, 0, 0x2000]);
        dtb.end();
        expect_error(&dtb.finish(), FdtError::AddressOverflow);
    }

    #[test]
    fn all_truncated_prefixes_are_rejected_without_panicking() {
        let blob = sample32();
        for length in 0..blob.len() {
            assert!(parse(&blob[..length]).is_err(), "accepted prefix {length}");
        }
    }

    #[test]
    fn block_overlap_bad_strings_and_unterminated_reserve_map_are_rejected() {
        let mut blob = sample32();
        blob[12..16].copy_from_slice(&56u32.to_be_bytes());
        expect_error(&blob, FdtError::BadHeader);

        let mut blob = sample32();
        let strings_offset = be_u32(&blob, 12).unwrap() as usize;
        // Every property name is now missing its NUL terminator.
        blob[strings_offset..].fill(b'x');
        expect_error(&blob, FdtError::BadString);

        let mut blob = sample32();
        blob[40..48].copy_from_slice(&0x1000u64.to_be_bytes());
        blob[48..56].copy_from_slice(&0x1000u64.to_be_bytes());
        expect_error(&blob, FdtError::Truncated);
    }

    #[test]
    fn property_after_child_duplicate_cells_and_missing_ram_are_rejected() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x10000);
        dtb.property("bootargs", b"x\0");
        expect_error(&dtb.finish(), FdtError::MalformedStructure);

        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.cells("#address-cells", &[1]);
        dtb.memory32(0, 0x10000);
        expect_error(&dtb.finish(), FdtError::DuplicateProperty);

        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        expect_error(&dtb.finish(), FdtError::MissingRam);
    }

    #[test]
    fn version_16_and_newer_compatible_headers_are_supported() {
        let mut blob = sample32();
        blob[20..24].copy_from_slice(&16u32.to_be_bytes());
        // v16's nonexistent structure-size field is merely alignment padding.
        blob[36..40].fill(0);
        assert!(parse(&blob).is_ok());
        blob[20..24].copy_from_slice(&18u32.to_be_bytes());
        let structure_size = be_u32(&blob, 12).unwrap() - be_u32(&blob, 8).unwrap();
        blob[36..40].copy_from_slice(&structure_size.to_be_bytes());
        blob[24..28].copy_from_slice(&17u32.to_be_bytes());
        assert!(parse(&blob).is_ok());
        blob[24..28].copy_from_slice(&18u32.to_be_bytes());
        expect_error(&blob, FdtError::UnsupportedVersion);
    }

    #[test]
    fn node_stack_is_bounded() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        for _ in 0..MAX_DEPTH {
            dtb.begin("bus");
        }
        for _ in 0..MAX_DEPTH {
            dtb.end();
        }
        expect_error(&dtb.finish(), FdtError::DepthExceeded);
    }

    #[test]
    fn dynamic_request_list_is_bounded() {
        let mut dtb = Dtb::new();
        dtb.root(1, 1);
        dtb.memory32(0, 0x100000);
        dtb.reserved32();
        for _ in 0..=MAX_DYNAMIC_RESERVATIONS {
            dtb.begin("pool");
            dtb.cells("size", &[0x1000]);
            dtb.end();
        }
        dtb.end();
        expect_error(&dtb.finish(), FdtError::DynamicReservationCapacity);
    }
}
