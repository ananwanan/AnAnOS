//! Host-testable no_std memory, userspace ABI, ELF, process and VFS algorithms.
#![no_std]

#[cfg(test)]
extern crate std;

// Hardware bootstrap remains in the kernel binary. These modules are shared
// with it so host tests exercise the same parser and allocator implementation.
#[path = "memory/fdt.rs"]
pub mod fdt;
pub mod fs;
#[path = "memory/heap.rs"]
pub mod heap;
#[path = "memory/mapping.rs"]
pub mod mapping;
#[path = "memory/page.rs"]
pub mod page;
#[path = "memory/paging.rs"]
pub mod paging;
#[path = "memory/regions.rs"]
pub mod regions;

// The binary and host tests use identical userspace permission/ABI algorithms.
pub mod arch {
    pub mod context;
}
pub mod memory {
    pub use crate::paging;
}
pub mod userspace {
    pub mod abi;
    pub mod copy;
    pub mod elf;
    pub mod fault;
    pub mod files;
    pub mod preflight;
    pub mod process;
    pub mod space;
    pub mod syscall;
    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
#[path = "memory/mmu_tests.rs"]
mod mmu_tests;
