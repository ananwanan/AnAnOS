//! Cortex-A72 EL1 identity translation. This first MMU stage leaves C/I clear.

use core::arch::asm;

const SCTLR_M: u64 = 1;
const SCTLR_C: u64 = 1 << 2;
const SCTLR_I: u64 = 1 << 12;
const SCTLR_WXN: u64 = 1 << 19;
const SCTLR_RES1: u64 = 0x30D0_0800;
const SCTLR_ENDIAN: u64 = (1 << 24) | (1 << 25);
// EL0 must not mask IRQs or perform cache maintenance; timer access is gated
// separately through CNTKCTL_EL1 below.
const SCTLR_EL0_CONTROL: u64 = (1 << 9) | (1 << 14) | (1 << 26); // UMA/DZE/UCI

// AttrIdx 0: Normal, inner/outer non-cacheable (0x44).
// AttrIdx 1: Device-nGnRnE (0x00). No cacheable RAM aliases in this milestone.
pub const MAIR_VALUE: u64 = 0x44;
// 39-bit TTBR0 VA, 4 KiB granule, inner-shareable non-cacheable table walks.
// TTBR1 walks disabled, with its supported 4 KiB TG1 encoding 0b10.
// IPS=0b010 chooses 40-bit physical addresses, checked against MMFR0 below.
pub const TCR_VALUE: u64 = 25 | (3 << 12) | (25 << 16) | (1 << 23) | (2 << 30) | (2 << 32);

#[derive(Debug, Clone, Copy)]
pub enum MmuError {
    WrongExecutionState,
    AlreadyEnabledOrCached,
    UnsupportedCpu,
    CoherencyDisabled,
    InvalidRoot,
    TranslationFault(u64),
    UnexpectedPhysicalAddress,
}

#[derive(Debug, Clone, Copy)]
pub struct Registers {
    pub sctlr: u64,
    pub tcr: u64,
    pub mair: u64,
    pub ttbr0: u64,
}

pub fn registers() -> Registers {
    let (sctlr, tcr, mair, ttbr0);
    unsafe {
        asm!(
            "mrs {sctlr}, sctlr_el1", "mrs {tcr}, tcr_el1",
            "mrs {mair}, mair_el1", "mrs {ttbr0}, ttbr0_el1",
            sctlr = out(reg) sctlr, tcr = out(reg) tcr,
            mair = out(reg) mair, ttbr0 = out(reg) ttbr0,
            options(nomem, nostack, preserves_flags),
        );
    }
    Registers {
        sctlr,
        tcr,
        mair,
        ttbr0,
    }
}

pub fn is_enabled() -> bool {
    registers().sctlr & SCTLR_M != 0
}

/// # Safety
/// CPU0 at EL1h, IRQ/FIQ masked. The owned root and all subordinate tables must
/// remain live forever. The caller must software-walk code, stack, vectors,
/// allocator state and devices before entry, and keep every live pointer valid
/// at the same VA/PA with consistent Normal/device attributes.
pub unsafe fn enable(root: u64) -> Result<Registers, MmuError> {
    let (el, mpidr, daif, mmfr0, midr, spsel): (u64, u64, u64, u64, u64, u64);
    unsafe {
        asm!(
            "mrs {el}, CurrentEL", "mrs {mpidr}, mpidr_el1",
            "mrs {daif}, daif", "mrs {mmfr0}, id_aa64mmfr0_el1",
            "mrs {midr}, midr_el1",
            "mrs {spsel}, spsel",
            el = out(reg) el, mpidr = out(reg) mpidr,
            daif = out(reg) daif, mmfr0 = out(reg) mmfr0,
            midr = out(reg) midr,
            spsel = out(reg) spsel,
            options(nomem, nostack, preserves_flags),
        );
    }
    if el != 4 || spsel != 1 || mpidr & 0xFF != 0 || daif & 0xC0 != 0xC0 {
        return Err(MmuError::WrongExecutionState);
    }
    let old = registers().sctlr;
    if old & (SCTLR_M | SCTLR_C | SCTLR_I) != 0 {
        return Err(MmuError::AlreadyEnabledOrCached);
    }
    if (midr >> 24) & 0xFF != 0x41
        || (midr >> 4) & 0xFFF != 0xD08
        || !(2..=5).contains(&(mmfr0 & 0xF))
        || (mmfr0 >> 28) & 0xF != 0
    {
        return Err(MmuError::UnsupportedCpu);
    }
    // Cortex-A72 TRM 5.5 requires SMPEN even on a single running CPU before
    // MMU enable or TLB maintenance. Firmware must establish it; EL1 writes
    // can be restricted by EL2/EL3 ACTLR, so never attempt a speculative write.
    let cpuectlr: u64;
    unsafe {
        asm!("mrs {value}, S3_1_C15_C2_1", value = out(reg) cpuectlr,
                  options(nomem, nostack, preserves_flags));
    }
    if cpuectlr & (1 << 6) == 0 {
        return Err(MmuError::CoherencyDisabled);
    }
    if root == 0 || root & 0xFFF != 0 || root >= (1 << 40) {
        return Err(MmuError::InvalidRoot);
    }
    let mut control = (old | SCTLR_RES1 | SCTLR_M | SCTLR_WXN) & !SCTLR_ENDIAN;
    #[cfg(feature = "userspace")]
    {
        control &= !SCTLR_EL0_CONTROL;
        // The physical timer belongs to EL1. Firmware may have permitted EL0
        // access; revoke it before running a task so it cannot defeat timeout.
        unsafe {
            asm!(
                "msr cntkctl_el1, xzr",
                "isb",
                options(nostack, preserves_flags)
            );
        }
    }
    // SAFETY: cache-off tables are already complete in RAM. Full barriers drain
    // writes and invalidate all CPU0 stage-1 translations before M takes effect.
    // Do not use nomem: table writes must remain before the enable sequence.
    unsafe {
        asm!(
            "dsb sy",
            "msr mair_el1, {mair}", "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {root}", "msr ttbr1_el1, xzr", "isb",
            "tlbi vmalle1", "dsb sy", "isb",
            "msr sctlr_el1, {control}", "isb",
            mair = in(reg) MAIR_VALUE, tcr = in(reg) TCR_VALUE,
            root = in(reg) root, control = in(reg) control,
            options(nostack, preserves_flags),
        );
    }
    Ok(registers())
}

/// Probe only after M=1; with M=0, AT cannot validate the software-built tables.
pub fn probe(address: usize, write: bool) -> Result<u64, MmuError> {
    let result: u64;
    unsafe {
        if write {
            asm!("at s1e1w, {address}", "isb", "mrs {result}, par_el1",
                 address = in(reg) address, result = out(reg) result,
                 options(nostack, preserves_flags));
        } else {
            asm!("at s1e1r, {address}", "isb", "mrs {result}, par_el1",
                 address = in(reg) address, result = out(reg) result,
                 options(nostack, preserves_flags));
        }
    }
    if result & 1 != 0 {
        return Err(MmuError::TranslationFault(result));
    }
    let physical = (result & 0x0000_00FF_FFFF_F000) | (address as u64 & 0xFFF);
    if physical != address as u64 {
        return Err(MmuError::UnexpectedPhysicalAddress);
    }
    Ok(result)
}

/// # Safety
/// EL1h CPU0, IRQ/FIQ masked; MMU active and unchanged TCR/MAIR. Both roots must
/// own complete tables and map all live kernel code/data/stack at identical VAs.
/// Keep the outgoing root live until this sequence's full TLBI completes.
pub unsafe fn switch_root(root: u64) -> Result<(), MmuError> {
    if !is_enabled() || root == 0 || root & 0xFFF != 0 || root >= (1 << 40) {
        return Err(MmuError::InvalidRoot);
    }
    let daif: u64;
    unsafe {
        asm!("mrs {value}, daif", value = out(reg) daif, options(nomem, nostack, preserves_flags));
    }
    if daif & 0xC0 != 0xC0 {
        return Err(MmuError::WrongExecutionState);
    }
    unsafe {
        asm!("dsb sy", "isb", "msr ttbr0_el1, {root}", "isb",
             "tlbi vmalle1", "dsb sy", "isb", root = in(reg) root,
             options(nostack, preserves_flags));
    }
    Ok(())
}

/// Return the raw PAR result of an EL0 access probe. Faults are encoded in F=1;
/// successful PA differs from VA for private user mappings.
pub fn probe_user(address: u64, write: bool) -> u64 {
    let result: u64;
    unsafe {
        if write {
            asm!("at s1e0w, {address}", "isb", "mrs {result}, par_el1",
                 address = in(reg) address, result = out(reg) result,
                 options(nostack, preserves_flags));
        } else {
            asm!("at s1e0r, {address}", "isb", "mrs {result}, par_el1",
                 address = in(reg) address, result = out(reg) result,
                 options(nostack, preserves_flags));
        }
    }
    result
}

/// Newly copied user text is Normal NC with caches disabled. Complete stores
/// and invalidate instruction state before fetching it at a different VA.
pub fn publish_user_code() {
    unsafe {
        asm!(
            "dsb sy",
            "ic iallu",
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}
