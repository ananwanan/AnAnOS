//! Strict syndrome checks for the three M3 EL0 faulting programs.
//!
//! ESR fields follow Arm's AArch64 Data Abort encoding (DDI0487 and the
//! Cortex-A72 TRM, section 4.3.50). The caller separately validates SPSR_EL1
//! as EL0t/AArch64; the lower-EL Data Abort class alone also covers AArch32.

const LOWER_EL_DATA_ABORT: u64 = 0x24;
const INSTRUCTION_32_BIT: u64 = 1 << 25;
const FAR_NOT_VALID: u64 = 1 << 10;
const EXTERNAL_ABORT_TYPE: u64 = 1 << 9;
const CACHE_MAINTENANCE: u64 = 1 << 8;
const STAGE1_TABLE_WALK: u64 = 1 << 7;
const WRITE_NOT_READ: u64 = 1 << 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortKind {
    Translation,
    Permission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataAbort {
    pub dfsc: u8,
    /// Only the translation/permission categories used by these demos.
    pub kind: Option<AbortKind>,
    /// The walk level, when `kind` describes a translation/permission fault.
    pub level: Option<u8>,
    pub write: bool,
    pub instruction_32bit: bool,
    /// FnV must be clear. It is RES0 for the expected page faults.
    pub far_valid: bool,
    pub stage1_walk: bool,
    pub cache_maintenance: bool,
    /// EA classifies External aborts; it must be zero for our page faults.
    pub external_abort_type: bool,
}

impl DataAbort {
    /// Decode a lower-EL Data Abort without treating other FSCs as page faults.
    pub fn decode(esr: u64) -> Option<Self> {
        if (esr >> 26) & 0x3f != LOWER_EL_DATA_ABORT {
            return None;
        }
        let dfsc = (esr & 0x3f) as u8;
        let kind = match dfsc {
            0x04..=0x07 => Some(AbortKind::Translation),
            // Cortex-A72 has no level-0 permission fault encoding.
            0x0d..=0x0f => Some(AbortKind::Permission),
            _ => None,
        };
        Some(Self {
            dfsc,
            kind,
            level: kind.map(|_| dfsc & 3),
            write: esr & WRITE_NOT_READ != 0,
            instruction_32bit: esr & INSTRUCTION_32_BIT != 0,
            far_valid: esr & FAR_NOT_VALID == 0,
            stage1_walk: esr & STAGE1_TABLE_WALK != 0,
            cache_maintenance: esr & CACHE_MAINTENANCE != 0,
            external_abort_type: esr & EXTERNAL_ABORT_TYPE != 0,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedFault {
    pub kind: AbortKind,
    pub level: u8,
    pub write: bool,
    pub address: u64,
    pub pc: u64,
    pub stack: u64,
}

impl ExpectedFault {
    /// The caller supplies the level from the frozen mapping (or a guard's
    /// adjacent L3 table). Cortex-A72 bootstrap walks use levels 1 through 3.
    /// ELR must point to the labeled load/store itself, not its next instruction.
    pub fn matches(self, esr: u64, pc: u64, address: u64, stack: u64) -> bool {
        let Some(abort) = DataAbort::decode(esr) else {
            return false;
        };
        (1..=3).contains(&self.level)
            && abort.kind == Some(self.kind)
            && abort.level == Some(self.level)
            && abort.write == self.write
            && abort.instruction_32bit
            && abort.far_valid
            && !abort.stage1_walk
            && !abort.cache_maintenance
            && !abort.external_abort_type
            && pc == self.pc
            && address == self.address
            && stack == self.stack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EC: u64 = LOWER_EL_DATA_ABORT << 26;
    const CODE: u64 = 0x40_0000_0000;
    const STACK_BASE: u64 = CODE + 0x20_1000;
    const STACK_TOP: u64 = STACK_BASE + 0x4000;

    const EXPECTED: [(ExpectedFault, u64); 3] = [
        (
            ExpectedFault {
                kind: AbortKind::Permission,
                level: 3,
                write: false,
                address: 0x20_0000,
                pc: CODE,
                stack: STACK_TOP,
            },
            EC | INSTRUCTION_32_BIT | 0x0f,
        ),
        (
            ExpectedFault {
                kind: AbortKind::Permission,
                level: 3,
                write: true,
                address: CODE,
                pc: CODE + 4,
                stack: STACK_TOP,
            },
            EC | INSTRUCTION_32_BIT | WRITE_NOT_READ | 0x0f,
        ),
        (
            ExpectedFault {
                kind: AbortKind::Translation,
                level: 3,
                write: true,
                address: STACK_BASE - 8,
                pc: CODE + 8,
                stack: STACK_TOP,
            },
            EC | INSTRUCTION_32_BIT | WRITE_NOT_READ | 0x07,
        ),
    ];

    #[test]
    fn expected_accesses_require_their_exact_syndrome_and_saved_context() {
        for (expected, esr) in EXPECTED {
            assert!(expected.matches(esr, expected.pc, expected.address, expected.stack));
            assert!(!expected.matches(esr, expected.pc + 4, expected.address, expected.stack));
            assert!(!expected.matches(esr, expected.pc - 4, expected.address, expected.stack));
            assert!(!expected.matches(esr, expected.pc, expected.address + 1, expected.stack));
            assert!(!expected.matches(esr, expected.pc, expected.address, expected.stack - 16));
        }
    }

    #[test]
    fn other_exception_classes_never_decode_or_pass() {
        // Includes same-EL Data Abort, instruction abort, SVC and SP alignment.
        for class in 0..=0x3f {
            if class == LOWER_EL_DATA_ABORT {
                continue;
            }
            for (expected, esr) in EXPECTED {
                let different = (esr & !(0x3f << 26)) | (class << 26);
                assert_eq!(DataAbort::decode(different), None);
                assert!(!expected.matches(
                    different,
                    expected.pc,
                    expected.address,
                    expected.stack
                ));
            }
        }
    }

    #[test]
    fn kernel_permission_fault_accepts_only_its_actual_leaf_level() {
        let (page_fault, esr) = EXPECTED[0];
        for level in 1..=3 {
            let expected = ExpectedFault {
                level,
                ..page_fault
            };
            for actual_level in 1..=3 {
                let candidate = (esr & !3) | actual_level as u64;
                assert_eq!(
                    expected.matches(candidate, expected.pc, expected.address, expected.stack),
                    level == actual_level,
                    "expected level={level}, actual level={actual_level}"
                );
            }
        }
        // Invalid expected levels cannot authorize an otherwise valid page fault.
        for level in [0, 4, u8::MAX] {
            let expected = ExpectedFault {
                level,
                ..page_fault
            };
            assert!(!expected.matches(esr, expected.pc, expected.address, expected.stack));
        }
    }

    #[test]
    fn other_fault_categories_and_levels_never_pass_as_the_expected_fault() {
        // Enumerating every FSC also covers access-flag, alignment, external,
        // parity/ECC and reserved faults, including those ending in level bits 3.
        for (expected, esr) in EXPECTED {
            for dfsc in 0..=0x3f {
                let candidate = (esr & !0x3f) | dfsc;
                let should_match = dfsc == esr & 0x3f;
                assert_eq!(
                    expected.matches(candidate, expected.pc, expected.address, expected.stack),
                    should_match,
                    "expected {:?}, dfsc={dfsc:#x}",
                    expected.kind
                );
            }
        }
    }

    #[test]
    fn wrong_access_direction_or_indirect_invalid_address_flags_never_pass() {
        for (expected, esr) in EXPECTED {
            for candidate in [
                esr ^ WRITE_NOT_READ,
                esr & !INSTRUCTION_32_BIT,
                esr | FAR_NOT_VALID,
                esr | STAGE1_TABLE_WALK,
                esr | CACHE_MAINTENANCE,
                esr | EXTERNAL_ABORT_TYPE,
            ] {
                assert!(!expected.matches(
                    candidate,
                    expected.pc,
                    expected.address,
                    expected.stack
                ));
            }
        }
    }

    #[test]
    fn decoder_preserves_fault_detail_without_reclassifying_other_fscs() {
        let flags = INSTRUCTION_32_BIT
            | WRITE_NOT_READ
            | FAR_NOT_VALID
            | STAGE1_TABLE_WALK
            | CACHE_MAINTENANCE
            | EXTERNAL_ABORT_TYPE;
        for dfsc in 0..=0x3f {
            let abort = DataAbort::decode(EC | flags | dfsc).unwrap();
            let kind = match dfsc {
                0x04..=0x07 => Some(AbortKind::Translation),
                0x0d..=0x0f => Some(AbortKind::Permission),
                _ => None,
            };
            assert_eq!(abort.dfsc, dfsc as u8);
            assert_eq!(abort.kind, kind);
            assert_eq!(abort.level, kind.map(|_| dfsc as u8 & 3));
            assert!(abort.write);
            assert!(abort.instruction_32bit);
            assert!(!abort.far_valid);
            assert!(abort.stage1_walk);
            assert!(abort.cache_maintenance);
            assert!(abort.external_abort_type);
        }
    }
}
