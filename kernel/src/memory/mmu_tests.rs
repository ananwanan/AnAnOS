//! Host integration checks from firmware policy through physical table ownership.
//! No simulated address is dereferenced: descriptors live in host-owned arrays.

use super::fdt;
use super::mapping::{KernelSections, MappingError, MappingPlan};
use super::page::{PAGE_SIZE, PageAllocator, PageError, PhysicalPages};
use super::paging::{MappingAttrs, MemoryType, PageTables, PagingError, TableMemory};
use super::regions::Region;
use std::boxed::Box;
use std::collections::BTreeMap;
use std::vec::Vec;

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
const RODATA: MappingAttrs = MappingAttrs {
    memory: MemoryType::NormalNonCacheable,
    writable: false,
    executable: false,
};
const DEVICE: MappingAttrs = MappingAttrs {
    memory: MemoryType::Device,
    writable: true,
    executable: false,
};
const FRAMEBUFFER: Region = Region {
    start: 0x3C10_0001,
    end: 0x3C90_0000,
};
const SECTIONS: KernelSections = KernelSections {
    text: Region {
        start: 0x0020_0000,
        end: 0x0020_8000,
    },
    rodata: Region {
        start: 0x0020_8000,
        end: 0x0020_A000,
    },
    data: Region {
        start: 0x0020_A000,
        end: 0x0040_0000,
    },
};

/// Nominal 1 GiB/4 GiB fixtures keep a 64 MiB VideoCore carveout below 1 GiB.
/// The larger fixture also has a physical MMIO gap below 4 GiB and RAM above it.
/// These are deterministic firmware fixtures, not guessed board RAM detection.
fn firmware_ram(four_gib: bool) -> Vec<Region> {
    let mut banks = std::vec![Region {
        start: 0,
        end: 0x3C00_0000,
    }];
    if four_gib {
        banks.extend([
            Region {
                start: 0x4000_0000,
                end: 0xFC00_0000,
            },
            Region {
                start: 0x1_0000_0000,
                end: 0x1_0400_0000,
            },
        ]);
    }
    banks
}

/// A tiny real DTB encoder deliberately supplies 64-bit reg tuples and keeps
/// ordinary firmware reservations distinct from reserved-memory/no-map nodes.
struct FirmwareDtb {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl FirmwareDtb {
    fn new() -> Self {
        Self {
            structure: Vec::new(),
            strings: Vec::new(),
        }
    }

    fn word(&mut self, word: u32) {
        self.structure.extend_from_slice(&word.to_be_bytes());
    }

    fn begin(&mut self, name: &str) {
        self.word(1);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        while self.structure.len() % 4 != 0 {
            self.structure.push(0);
        }
    }

    fn end(&mut self) {
        self.word(2);
    }

    fn property(&mut self, name: &str, value: &[u8]) {
        let offset = self.strings.len();
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.word(3);
        self.word(value.len() as u32);
        self.word(offset as u32);
        self.structure.extend_from_slice(value);
        while self.structure.len() % 4 != 0 {
            self.structure.push(0);
        }
    }

    fn reg(&mut self, region: Region) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(region.start as u64).to_be_bytes());
        bytes.extend_from_slice(&((region.end - region.start) as u64).to_be_bytes());
        self.property("reg", &bytes);
    }

    fn finish(mut self) -> Vec<u8> {
        self.end();
        self.word(9);
        let reserve_offset = 40usize;
        // The UART bootloader/relocated DTB are protected but remain readable.
        let reserve_size = 32usize;
        let structure_offset = reserve_offset + reserve_size;
        let strings_offset = structure_offset + self.structure.len();
        let total_size = strings_offset + self.strings.len();
        let mut bytes = Vec::new();
        for word in [
            0xD00D_FEED,
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
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        bytes.extend_from_slice(&0x0008_0000u64.to_be_bytes());
        bytes.extend_from_slice(&0x0008_0000u64.to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&self.structure);
        bytes.extend_from_slice(&self.strings);
        bytes
    }
}

fn parse_firmware(four_gib: bool, holes: &[Region]) -> fdt::MemoryLayout {
    let mut dtb = FirmwareDtb::new();
    dtb.begin("");
    dtb.property("#address-cells", &2u32.to_be_bytes());
    dtb.property("#size-cells", &2u32.to_be_bytes());
    for bank in firmware_ram(four_gib) {
        dtb.begin("memory");
        dtb.property("device_type", b"memory\0");
        dtb.reg(bank);
        dtb.end();
    }
    dtb.begin("reserved-memory");
    dtb.property("#address-cells", &2u32.to_be_bytes());
    dtb.property("#size-cells", &2u32.to_be_bytes());
    dtb.property("ranges", &[]);
    dtb.begin("firmware-buffer");
    dtb.reg(Region {
        start: 0x0018_0000,
        end: 0x0018_2000,
    });
    dtb.end();
    for hole in holes {
        dtb.begin("protected-pool");
        dtb.reg(*hole);
        dtb.property("no-map", &[]);
        dtb.end();
    }
    dtb.end();
    fdt::parse(&dtb.finish()).unwrap()
}

struct HostTable {
    owner: PhysicalPages,
    entries: Box<[u64; 512]>,
}

struct PhysicalTableStore {
    // Boxing fixes the page allocator's address while its owned tokens exist.
    allocator: Box<PageAllocator<1>>,
    tables: BTreeMap<u64, HostTable>,
    live_limit: usize,
}

impl PhysicalTableStore {
    fn new(live_limit: usize) -> Self {
        let mut allocator = Box::new(PageAllocator::<1>::new());
        // Restrict the physical allocator to one actual free-RAM table pool;
        // descriptor policy still covers every bank from the parsed DTB.
        allocator
            .init(
                &[Region {
                    start: 0x0080_0000,
                    end: 0x0080_0000 + PAGE_SIZE * 64,
                }],
                &[],
            )
            .unwrap();
        Self {
            allocator,
            tables: BTreeMap::new(),
            live_limit,
        }
    }

    fn assert_accounting(&self) {
        let stats = self.allocator.stats();
        assert_eq!(stats.allocated_pages, self.tables.len());
        assert_eq!(stats.free_pages + stats.allocated_pages, 64);
        assert_eq!(stats.reserved_pages, 0);
    }

    fn assert_reclaimed(&self) {
        self.assert_accounting();
        assert!(self.tables.is_empty());
        assert_eq!(self.allocator.stats().free_pages, 64);
    }
}

impl TableMemory for PhysicalTableStore {
    fn allocate_table(&mut self) -> Result<u64, PagingError> {
        if self.tables.len() == self.live_limit {
            return Err(PagingError::OutOfMemory);
        }
        let owner = match self.allocator.alloc_pages(1, 1) {
            Ok(owner) => owner,
            Err(PageError::OutOfMemory) => return Err(PagingError::OutOfMemory),
            Err(error) => panic!("unexpected table allocator error: {error:?}"),
        };
        let address = owner.start_address() as u64;
        assert!(
            self.tables
                .insert(
                    address,
                    HostTable {
                        owner,
                        entries: Box::new([u64::MAX; 512]),
                    },
                )
                .is_none()
        );
        self.assert_accounting();
        Ok(address)
    }

    fn read_entry(&self, table: u64, index: usize) -> u64 {
        self.tables[&table].entries[index]
    }

    fn write_entry(&mut self, table: u64, index: usize, entry: u64) {
        self.tables.get_mut(&table).unwrap().entries[index] = entry;
    }

    fn release_table(&mut self, table: u64) {
        let owned = self.tables.remove(&table).unwrap();
        self.allocator.free_pages(owned.owner).unwrap();
        self.assert_accounting();
    }
}

fn install_plan(
    plan: &MappingPlan,
    tables: &mut PageTables,
    store: &mut PhysicalTableStore,
) -> Result<(), PagingError> {
    for mapping in plan.as_slice() {
        let start = mapping.region.start as u64;
        tables.map_range(
            store,
            start,
            start,
            (mapping.region.end - mapping.region.start) as u64,
            mapping.attrs,
        )?;
    }
    Ok(())
}

fn expect_mapping(
    tables: &PageTables,
    store: &PhysicalTableStore,
    address: usize,
    attrs: MappingAttrs,
) {
    let translation = tables.lookup(store, address as u64).unwrap();
    assert_eq!(translation.physical_address, address as u64);
    assert_eq!(translation.attrs, attrs);
}

#[test]
fn firmware_to_descriptors_preserves_pi_memory_policy_and_table_ownership() {
    for four_gib in [false, true] {
        let holes = [
            Region {
                start: 0x0010_0001,
                end: 0x0010_1FFF,
            },
            Region {
                start: 0x1000_0000,
                end: 0x1000_3000,
            },
        ];
        let firmware = parse_firmware(four_gib, &holes);
        assert_eq!(firmware.ram.as_slice(), firmware_ram(four_gib));
        assert_eq!(firmware.unmapped.as_slice(), holes);
        let plan = MappingPlan::build(
            firmware.ram.as_slice(),
            firmware.unmapped.as_slice(),
            SECTIONS,
            Some(FRAMEBUFFER),
        )
        .unwrap();
        let mut store = PhysicalTableStore::new(64);
        let mut tables = PageTables::new(&mut store).unwrap();
        install_plan(&plan, &mut tables, &mut store).unwrap();
        store.assert_accounting();

        for address in [SECTIONS.text.start, SECTIONS.text.end - 1] {
            expect_mapping(&tables, &store, address, TEXT);
        }
        for address in [SECTIONS.rodata.start, SECTIONS.rodata.end - 1] {
            expect_mapping(&tables, &store, address, RODATA);
        }
        for address in [
            PAGE_SIZE,
            0x0008_0000,
            0x0018_0000,
            SECTIONS.data.start,
            SECTIONS.data.end - 1,
            FRAMEBUFFER.start - 1,
            FRAMEBUFFER.end - 1,
            0x3BFF_FFFF,
        ] {
            expect_mapping(&tables, &store, address, DATA);
        }
        for address in [
            0,
            PAGE_SIZE - 1,
            0x0010_0000,
            0x0010_1FFF,
            0x1000_0000,
            0x1000_2FFF,
            0x3C00_0000,
            FRAMEBUFFER.end,
            0x3FFF_FFFF,
        ] {
            assert_eq!(tables.lookup(&store, address as u64), None);
        }
        for address in [
            0xFC00_0000,
            0xFE00_0000,
            0xFF84_1000,
            0xFF84_2000,
            0xFFFF_FFFF,
        ] {
            expect_mapping(&tables, &store, address, DEVICE);
        }
        if four_gib {
            for address in [0x4000_0000, 0xFBFF_FFFF, 0x1_0000_0000, 0x1_03FF_FFFF] {
                expect_mapping(&tables, &store, address, DATA);
            }
            assert_eq!(tables.lookup(&store, 0x1_0400_0000), None);
        } else {
            assert_eq!(tables.lookup(&store, 0x4000_0000), None);
            assert_eq!(tables.lookup(&store, 0x1_0000_0000), None);
        }
        // Page-walk storage itself must remain accessible with Normal NC data
        // permissions, and every backing page has one allocator ownership token.
        for address in store.tables.keys() {
            expect_mapping(&tables, &store, *address as usize, DATA);
        }
        tables.destroy(&mut store);
        store.assert_reclaimed();
    }
}

#[test]
fn every_table_budget_failure_reclaims_all_physical_ownership() {
    let firmware = parse_firmware(
        true,
        &[Region {
            start: 0x1000_0001,
            end: 0x1000_1FFF,
        }],
    );
    let plan = MappingPlan::build(
        firmware.ram.as_slice(),
        firmware.unmapped.as_slice(),
        SECTIONS,
        Some(FRAMEBUFFER),
    )
    .unwrap();
    let mut baseline = PhysicalTableStore::new(64);
    let mut complete = PageTables::new(&mut baseline).unwrap();
    install_plan(&plan, &mut complete, &mut baseline).unwrap();
    let required = baseline.tables.len();
    assert!(required > 3);
    complete.destroy(&mut baseline);
    baseline.assert_reclaimed();

    for budget in 0..required {
        let mut store = PhysicalTableStore::new(budget);
        match PageTables::new(&mut store) {
            Err(error) => {
                assert_eq!(budget, 0);
                assert_eq!(error, PagingError::OutOfMemory);
            }
            Ok(mut tables) => {
                assert_eq!(
                    install_plan(&plan, &mut tables, &mut store),
                    Err(PagingError::OutOfMemory)
                );
                store.assert_accounting();
                assert_eq!(store.tables.len(), budget);
                tables.destroy(&mut store);
            }
        }
        store.assert_reclaimed();
    }
}

#[test]
fn firmware_no_map_conflicts_stop_before_any_table_page_is_owned() {
    let mut store = PhysicalTableStore::new(64);
    for (hole, expected) in [
        (
            Region {
                start: SECTIONS.text.start - 1,
                end: SECTIONS.text.start + 1,
            },
            MappingError::KernelUnmapped,
        ),
        (
            Region {
                start: FRAMEBUFFER.start + PAGE_SIZE,
                end: FRAMEBUFFER.start + PAGE_SIZE + 1,
            },
            MappingError::FramebufferUnmapped,
        ),
    ] {
        let firmware = parse_firmware(true, &[hole]);
        let plan = MappingPlan::build(
            firmware.ram.as_slice(),
            firmware.unmapped.as_slice(),
            SECTIONS,
            Some(FRAMEBUFFER),
        );
        assert_eq!(plan.unwrap_err(), expected);
        store.assert_reclaimed();
    }
    // Reuse the same allocator after both policy failures.
    let firmware = parse_firmware(true, &[]);
    let plan =
        MappingPlan::build(firmware.ram.as_slice(), &[], SECTIONS, Some(FRAMEBUFFER)).unwrap();
    let mut tables = PageTables::new(&mut store).unwrap();
    install_plan(&plan, &mut tables, &mut store).unwrap();
    tables.destroy(&mut store);
    store.assert_reclaimed();
}

#[test]
fn fragmented_firmware_plans_rebuild_and_reclaim_with_deterministic_stress() {
    let mut store = PhysicalTableStore::new(64);
    let mut random = 0x71A9_C053u32;
    for iteration in 0..64 {
        let mut holes = Vec::new();
        for index in 0..8 {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            let base = if index % 2 == 0 {
                0x0800_0000
            } else {
                0x1_0000_0000
            };
            let start = base + ((random as usize % 8192) * PAGE_SIZE) + 1;
            holes.push(Region {
                start,
                end: start + PAGE_SIZE + (random as usize % 2048),
            });
        }
        let firmware = parse_firmware(true, &holes);
        let plan = MappingPlan::build(
            firmware.ram.as_slice(),
            firmware.unmapped.as_slice(),
            SECTIONS,
            Some(FRAMEBUFFER),
        )
        .unwrap();
        let mut tables = PageTables::new(&mut store).unwrap();
        install_plan(&plan, &mut tables, &mut store).unwrap();
        for hole in &holes {
            // Firmware ownership covers every touched page; neighboring data
            // may only be absent when another reservation touches it as well.
            let first = hole.start & !(PAGE_SIZE - 1);
            let end = (hole.end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            for address in (first..end).step_by(PAGE_SIZE) {
                assert_eq!(tables.lookup(&store, address as u64), None);
            }
            for address in [first - PAGE_SIZE, end] {
                if !holes.iter().any(|other| {
                    address < (other.end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
                        && other.start & !(PAGE_SIZE - 1) < address + PAGE_SIZE
                }) {
                    expect_mapping(&tables, &store, address, DATA);
                }
            }
        }
        expect_mapping(&tables, &store, SECTIONS.text.start + iteration, TEXT);
        expect_mapping(&tables, &store, FRAMEBUFFER.start + iteration, DATA);
        expect_mapping(&tables, &store, 0xFF84_1000 + iteration, DEVICE);
        store.assert_accounting();
        tables.destroy(&mut store);
        store.assert_reclaimed();
    }
}
