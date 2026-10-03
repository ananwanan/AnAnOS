pub mod fdt;
pub mod heap;
pub mod page;
pub mod regions;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
mod runtime;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub use runtime::*;
