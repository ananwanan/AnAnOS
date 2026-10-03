//! Allocation-free policy for the first EL1 identity map.
//!
//! RAM remains non-cacheable during MMU bring-up. Only the linked kernel text
//! is executable; device registers and writable memory are execute-never.
//! Firmware `no-map` reservations and the null page have no translation.

use super::page::PAGE_SIZE;
use super::paging::{MappingAttrs, MemoryType};
use super::regions::{Region, RegionError, RegionSet};

pub const MAX_MAPPINGS: usize = 512;
const MAX_BOUNDARIES: usize = 1024;

/// BCM2711's peripheral/PCIe/GIC windows. This overrides any RAM declaration
/// covering the same pages, so malformed firmware data cannot make MMIO Normal.
pub const MMIO_REGION: Region = Region {
    start: 0xFC00_0000,
    end: 0x1_0000_0000,
};
const NULL_PAGE: Region = Region {
    start: 0,
    end: PAGE_SIZE,
};
const RAM_ATTRS: MappingAttrs = MappingAttrs {
    memory: MemoryType::NormalNonCacheable,
    writable: true,
    executable: false,
};
const TEXT_ATTRS: MappingAttrs = MappingAttrs {
    memory: MemoryType::NormalNonCacheable,
    writable: false,
    executable: true,
};
const RODATA_ATTRS: MappingAttrs = MappingAttrs {
    memory: MemoryType::NormalNonCacheable,
    writable: false,
    executable: false,
};
const MMIO_ATTRS: MappingAttrs = MappingAttrs {
    memory: MemoryType::Device,
    writable: true,
    executable: false,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelSections {
    pub text: Region,
    pub rodata: Region,
    /// Writable kernel data, BSS, the boot stack and their linker padding.
    pub data: Region,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mapping {
    pub region: Region,
    pub attrs: MappingAttrs,
}

impl Mapping {
    const EMPTY: Self = Self {
        region: Region { start: 0, end: 0 },
        attrs: RAM_ATTRS,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingError {
    InvalidRegion,
    AddressOverflow,
    InvalidKernelSection,
    KernelOverlap,
    KernelOutsideRam,
    KernelUnmapped,
    KernelMmio,
    InvalidFramebuffer,
    FramebufferKernelOverlap,
    FramebufferUnmapped,
    FramebufferMmio,
    Capacity,
    Regions(RegionError),
}

impl From<RegionError> for MappingError {
    fn from(error: RegionError) -> Self {
        Self::Regions(error)
    }
}

/// Sorted, disjoint, page-aligned mappings with adjacent equal attributes merged.
/// Build this plan before installing any descriptor: permissions are never
/// changed by replacing a live block mapping with a finer mapping.
#[derive(Debug)]
pub struct MappingPlan {
    entries: [Mapping; MAX_MAPPINGS],
    len: usize,
}

impl MappingPlan {
    pub const fn new() -> Self {
        Self {
            entries: [Mapping::EMPTY; MAX_MAPPINGS],
            len: 0,
        }
    }

    pub fn as_slice(&self) -> &[Mapping] {
        &self.entries[..self.len]
    }

    /// The returned plan can be convenient in host tests. Runtime bring-up uses
    /// `build_in_place` with static storage to avoid copying this large array.
    pub fn build(
        ram: &[Region],
        unmapped: &[Region],
        sections: KernelSections,
        framebuffer: Option<Region>,
    ) -> Result<Self, MappingError> {
        let mut plan = Self::new();
        plan.build_in_place(ram, unmapped, sections, framebuffer)?;
        Ok(plan)
    }

    /// Failed construction leaves an empty plan, never a partially usable map.
    pub fn build_in_place(
        &mut self,
        ram: &[Region],
        unmapped: &[Region],
        sections: KernelSections,
        framebuffer: Option<Region>,
    ) -> Result<(), MappingError> {
        self.len = 0;
        let result = self.build_inner(ram, unmapped, sections, framebuffer);
        if result.is_err() {
            self.len = 0;
        }
        result
    }

    fn build_inner(
        &mut self,
        ram: &[Region],
        unmapped: &[Region],
        sections: KernelSections,
        framebuffer: Option<Region>,
    ) -> Result<(), MappingError> {
        let mut full_pages = RegionSet::new();
        for region in ram {
            if region.start >= region.end {
                return Err(MappingError::InvalidRegion);
            }
            let start = align_up(region.start)?;
            let end = region.end & !(PAGE_SIZE - 1);
            if start < end {
                full_pages.insert(Region { start, end })?;
            }
        }
        let mut holes = RegionSet::new();
        for region in unmapped {
            holes.insert(align_outward(*region)?)?;
        }
        let kernel = [sections.text, sections.rodata, sections.data];
        for (index, region) in kernel.iter().enumerate() {
            if region.start >= region.end
                || region.start % PAGE_SIZE != 0
                || region.end % PAGE_SIZE != 0
            {
                return Err(MappingError::InvalidKernelSection);
            }
            if kernel[..index]
                .iter()
                .any(|other| overlaps(*region, *other))
            {
                return Err(MappingError::KernelOverlap);
            }
            if overlaps(*region, MMIO_REGION) {
                return Err(MappingError::KernelMmio);
            }
            if overlaps(*region, NULL_PAGE) || intersects(holes.as_slice(), *region) {
                return Err(MappingError::KernelUnmapped);
            }
            if !contained(full_pages.as_slice(), *region) {
                return Err(MappingError::KernelOutsideRam);
            }
        }
        let framebuffer = framebuffer
            .map(|region| {
                if region.start >= region.end {
                    return Err(MappingError::InvalidFramebuffer);
                }
                let region = align_outward(region)?;
                if overlaps(region, MMIO_REGION) {
                    return Err(MappingError::FramebufferMmio);
                }
                if kernel.iter().any(|section| overlaps(region, *section)) {
                    return Err(MappingError::FramebufferKernelOverlap);
                }
                if overlaps(region, NULL_PAGE) || intersects(holes.as_slice(), region) {
                    return Err(MappingError::FramebufferUnmapped);
                }
                Ok(region)
            })
            .transpose()?;

        let mut boundaries = Boundaries::new();
        for region in full_pages
            .as_slice()
            .iter()
            .chain(holes.as_slice())
            .chain(kernel.iter())
        {
            boundaries.add_region(*region)?;
        }
        boundaries.add_region(NULL_PAGE)?;
        boundaries.add_region(MMIO_REGION)?;
        if let Some(framebuffer) = framebuffer {
            boundaries.add_region(framebuffer)?;
        }
        boundaries.sort_unique();
        for pair in boundaries.as_slice().windows(2) {
            let region = Region {
                start: pair[0],
                end: pair[1],
            };
            let attrs = if contains(NULL_PAGE, region) || contained(holes.as_slice(), region) {
                None
            } else if contains(MMIO_REGION, region) {
                Some(MMIO_ATTRS)
            } else if contains(sections.text, region) {
                Some(TEXT_ATTRS)
            } else if contains(sections.rodata, region) {
                Some(RODATA_ATTRS)
            } else if contains(sections.data, region)
                || framebuffer.is_some_and(|buffer| contains(buffer, region))
                || contained(full_pages.as_slice(), region)
            {
                Some(RAM_ATTRS)
            } else {
                None
            };
            if let Some(attrs) = attrs {
                self.push(Mapping { region, attrs })?;
            }
        }
        Ok(())
    }

    fn push(&mut self, mapping: Mapping) -> Result<(), MappingError> {
        if let Some(previous) = self.entries[..self.len].last_mut() {
            if previous.region.end == mapping.region.start && previous.attrs == mapping.attrs {
                previous.region.end = mapping.region.end;
                return Ok(());
            }
        }
        if self.len == MAX_MAPPINGS {
            return Err(MappingError::Capacity);
        }
        self.entries[self.len] = mapping;
        self.len += 1;
        Ok(())
    }
}

impl Default for MappingPlan {
    fn default() -> Self {
        Self::new()
    }
}

struct Boundaries {
    values: [usize; MAX_BOUNDARIES],
    len: usize,
}

impl Boundaries {
    fn new() -> Self {
        Self {
            values: [0; MAX_BOUNDARIES],
            len: 0,
        }
    }

    fn add_region(&mut self, region: Region) -> Result<(), MappingError> {
        if self.len > MAX_BOUNDARIES - 2 {
            return Err(MappingError::Capacity);
        }
        self.values[self.len] = region.start;
        self.values[self.len + 1] = region.end;
        self.len += 2;
        Ok(())
    }

    fn sort_unique(&mut self) {
        self.values[..self.len].sort_unstable();
        let mut unique = 0;
        for index in 0..self.len {
            if unique == 0 || self.values[index] != self.values[unique - 1] {
                self.values[unique] = self.values[index];
                unique += 1;
            }
        }
        self.len = unique;
    }

    fn as_slice(&self) -> &[usize] {
        &self.values[..self.len]
    }
}

fn align_up(address: usize) -> Result<usize, MappingError> {
    address
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
        .ok_or(MappingError::AddressOverflow)
}

fn align_outward(region: Region) -> Result<Region, MappingError> {
    if region.start >= region.end {
        return Err(MappingError::InvalidRegion);
    }
    Ok(Region {
        start: region.start & !(PAGE_SIZE - 1),
        end: align_up(region.end)?,
    })
}

fn contains(container: Region, region: Region) -> bool {
    container.start <= region.start && region.end <= container.end
}

fn overlaps(left: Region, right: Region) -> bool {
    left.start < right.end && right.start < left.end
}

fn contained(regions: &[Region], region: Region) -> bool {
    regions.iter().any(|container| contains(*container, region))
}

fn intersects(regions: &[Region], region: Region) -> bool {
    regions.iter().any(|other| overlaps(*other, region))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections() -> KernelSections {
        KernelSections {
            text: Region {
                start: 0x2000,
                end: 0x4000,
            },
            rodata: Region {
                start: 0x4000,
                end: 0x5000,
            },
            data: Region {
                start: 0x5000,
                end: 0x8000,
            },
        }
    }

    fn ram() -> [Region; 1] {
        [Region {
            start: 0,
            end: 0x20000,
        }]
    }

    fn at(plan: &MappingPlan, address: usize) -> Option<MappingAttrs> {
        plan.as_slice()
            .iter()
            .find(|mapping| (mapping.region.start..mapping.region.end).contains(&address))
            .map(|mapping| mapping.attrs)
    }

    fn assert_disjoint(plan: &MappingPlan) {
        for mapping in plan.as_slice() {
            assert!(mapping.region.start < mapping.region.end);
            assert_eq!(mapping.region.start % PAGE_SIZE, 0);
            assert_eq!(mapping.region.end % PAGE_SIZE, 0);
            assert!(!(mapping.attrs.writable && mapping.attrs.executable));
        }
        for pair in plan.as_slice().windows(2) {
            assert!(pair[0].region.end <= pair[1].region.start);
            assert!(pair[0].region.end != pair[1].region.start || pair[0].attrs != pair[1].attrs);
        }
    }

    #[test]
    fn text_is_read_only_executable_and_all_other_ram_is_execute_never() {
        let plan = MappingPlan::build(&ram(), &[], sections(), None).unwrap();
        assert_eq!(at(&plan, 0x2000), Some(TEXT_ATTRS));
        assert_eq!(at(&plan, 0x4000), Some(RODATA_ATTRS));
        assert_eq!(at(&plan, 0x5000), Some(RAM_ATTRS));
        assert_eq!(at(&plan, 0x10000), Some(RAM_ATTRS));
        assert_disjoint(&plan);
    }

    #[test]
    fn null_page_and_unknown_ram_are_unmapped() {
        let plan = MappingPlan::build(&ram(), &[], sections(), None).unwrap();
        assert_eq!(at(&plan, 0), None);
        assert_eq!(at(&plan, PAGE_SIZE - 1), None);
        assert_eq!(at(&plan, PAGE_SIZE), Some(RAM_ATTRS));
        assert_eq!(at(&plan, 0x20000), None);
        assert_eq!(at(&plan, 0xBFFF_F000), None);
    }

    #[test]
    fn no_map_rounds_both_partial_page_tails_outward() {
        let holes = [Region {
            start: 0x10001,
            end: 0x11FFF,
        }];
        let plan = MappingPlan::build(&ram(), &holes, sections(), None).unwrap();
        assert_eq!(at(&plan, 0xF000), Some(RAM_ATTRS));
        assert_eq!(at(&plan, 0x10000), None);
        assert_eq!(at(&plan, 0x11FFF), None);
        assert_eq!(at(&plan, 0x12000), Some(RAM_ATTRS));
        assert_disjoint(&plan);
    }

    #[test]
    fn ram_rounds_inward_and_disjoint_high_banks_preserve_their_holes() {
        let ram = [
            Region {
                start: 1,
                end: 0x20001,
            },
            Region {
                start: 0x1_0000_0001,
                end: 0x1_0000_5FFF,
            },
            Region {
                start: 0x1_0001_0000,
                end: 0x1_0002_0000,
            },
        ];
        let plan = MappingPlan::build(&ram, &[], sections(), None).unwrap();
        assert_eq!(at(&plan, 0x20000), None);
        assert_eq!(at(&plan, 0x1_0000_0000), None);
        assert_eq!(at(&plan, 0x1_0000_1000), Some(RAM_ATTRS));
        assert_eq!(at(&plan, 0x1_0000_5000), None);
        assert_eq!(at(&plan, 0x1_0001_0000), Some(RAM_ATTRS));
        assert_disjoint(&plan);
    }

    #[test]
    fn mmio_overrides_firmware_ram_and_is_execute_never() {
        let ram = [ram()[0], MMIO_REGION];
        let plan = MappingPlan::build(&ram, &[], sections(), None).unwrap();
        for address in [0xFC00_0000, 0xFE00_0000, 0xFF84_1000, 0xFFFF_FFFF] {
            assert_eq!(at(&plan, address), Some(MMIO_ATTRS));
        }
        assert_disjoint(&plan);
    }

    #[test]
    fn framebuffer_is_authorized_outside_ram_and_page_rounded() {
        let framebuffer = Region {
            start: 0x30001,
            end: 0x31FFF,
        };
        let plan = MappingPlan::build(&ram(), &[], sections(), Some(framebuffer)).unwrap();
        assert_eq!(at(&plan, 0x30000), Some(RAM_ATTRS));
        assert_eq!(at(&plan, 0x31FFF), Some(RAM_ATTRS));
        assert_eq!(at(&plan, 0x32000), None);
        assert_disjoint(&plan);
    }

    #[test]
    fn kernel_requires_whole_aligned_ram_pages_without_overlap() {
        let mut section = sections();
        section.text.start += 1;
        assert_eq!(
            MappingPlan::build(&ram(), &[], section, None).unwrap_err(),
            MappingError::InvalidKernelSection
        );
        let mut section = sections();
        section.rodata.start = 0x3000;
        assert_eq!(
            MappingPlan::build(&ram(), &[], section, None).unwrap_err(),
            MappingError::KernelOverlap
        );
        assert_eq!(
            MappingPlan::build(
                &[Region {
                    start: 0x3000,
                    end: 0x20000,
                }],
                &[],
                sections(),
                None,
            )
            .unwrap_err(),
            MappingError::KernelOutsideRam
        );
    }

    #[test]
    fn kernel_and_framebuffer_cannot_override_no_map_or_each_other() {
        let hole = Region {
            start: 0x7FFF,
            end: 0x8001,
        };
        assert_eq!(
            MappingPlan::build(&ram(), &[hole], sections(), None).unwrap_err(),
            MappingError::KernelUnmapped
        );
        let framebuffer = Region {
            start: 0x10001,
            end: 0x11000,
        };
        assert_eq!(
            MappingPlan::build(&ram(), &[framebuffer], sections(), Some(framebuffer)).unwrap_err(),
            MappingError::FramebufferUnmapped
        );
        assert_eq!(
            MappingPlan::build(&ram(), &[], sections(), Some(hole)).unwrap_err(),
            MappingError::FramebufferKernelOverlap
        );
        assert_eq!(
            MappingPlan::build(&ram(), &[], sections(), Some(MMIO_REGION)).unwrap_err(),
            MappingError::FramebufferMmio
        );
        assert_eq!(
            MappingPlan::build(&ram(), &[], sections(), Some(NULL_PAGE)).unwrap_err(),
            MappingError::FramebufferUnmapped
        );
    }

    #[test]
    fn failed_rebuild_discards_previous_and_partial_results() {
        let mut plan = MappingPlan::build(&ram(), &[], sections(), None).unwrap();
        assert!(!plan.as_slice().is_empty());
        assert_eq!(
            plan.build_in_place(&[], &[], sections(), None),
            Err(MappingError::KernelOutsideRam)
        );
        assert!(plan.as_slice().is_empty());
    }

    #[test]
    fn full_dtb_no_map_capacity_does_not_conflict_with_null_page_bookkeeping() {
        let mut holes = RegionSet::new();
        for index in 0..super::super::regions::MAX_REGIONS {
            let start = 0x10000 + index * 2 * PAGE_SIZE;
            holes
                .insert(Region {
                    start,
                    end: start + PAGE_SIZE,
                })
                .unwrap();
        }
        let ram = [Region {
            start: 0,
            end: 0x200000,
        }];
        let plan = MappingPlan::build(&ram, holes.as_slice(), sections(), None).unwrap();
        assert_disjoint(&plan);
        assert_eq!(at(&plan, 0), None);
        for hole in holes.as_slice() {
            assert_eq!(at(&plan, hole.start), None);
            assert_eq!(at(&plan, hole.end), Some(RAM_ATTRS));
        }
    }

    #[test]
    fn full_ram_and_no_map_capacity_produce_a_bounded_disjoint_plan() {
        let mut banks = RegionSet::new();
        let mut holes = RegionSet::new();
        banks.insert(ram()[0]).unwrap();
        holes
            .insert(Region {
                start: 0x10000,
                end: 0x11000,
            })
            .unwrap();
        for index in 1..super::super::regions::MAX_REGIONS {
            let start = 0x1_0000_0000 + index * 0x10000;
            banks
                .insert(Region {
                    start,
                    end: start + 0x8000,
                })
                .unwrap();
            holes
                .insert(Region {
                    start: start + PAGE_SIZE,
                    end: start + 2 * PAGE_SIZE,
                })
                .unwrap();
        }
        let plan =
            MappingPlan::build(banks.as_slice(), holes.as_slice(), sections(), None).unwrap();
        assert_disjoint(&plan);
        for hole in holes.as_slice() {
            assert_eq!(at(&plan, hole.start), None);
            assert_eq!(at(&plan, hole.end), Some(RAM_ATTRS));
        }
    }

    #[test]
    fn page_rounding_overflow_fails_without_exposing_a_partial_plan() {
        let overflow = Region {
            start: usize::MAX - 100,
            end: usize::MAX,
        };
        let mut plan = MappingPlan::new();
        assert_eq!(
            plan.build_in_place(&ram(), &[overflow], sections(), None),
            Err(MappingError::AddressOverflow)
        );
        assert!(plan.as_slice().is_empty());
        assert_eq!(
            plan.build_in_place(&ram(), &[], sections(), Some(overflow)),
            Err(MappingError::AddressOverflow)
        );
        assert!(plan.as_slice().is_empty());
    }
}
