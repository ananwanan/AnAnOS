//! Host-testable, allocation-free kernel memory algorithms.
#![no_std]

#[cfg(test)]
extern crate std;

// Hardware bootstrap remains in the kernel binary. These modules are shared
// with it so host tests exercise the same parser and allocator implementation.
#[path = "memory/fdt.rs"]
pub mod fdt;
#[path = "memory/heap.rs"]
pub mod heap;
#[path = "memory/page.rs"]
pub mod page;
#[path = "memory/regions.rs"]
pub mod regions;
