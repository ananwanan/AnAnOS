//! Saved AArch64 exception state shared by vectors, handlers and host checks.
//! This is a kernel-private layout, never a userspace ABI structure.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ExceptionContext {
    pub registers: [u64; 31],
    pub elr_el1: u64,
    pub spsr_el1: u64,
    pub esr_el1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, align(16))]
pub struct ExceptionFrame {
    pub context: ExceptionContext,
    pub simd: [[u64; 2]; 32],
    pub fpcr: u64,
    pub fpsr: u64,
    pub sp_el0: u64,
    pub far_el1: u64,
}

// Keep every offset synchronized with boot/vectors.S, including the extension
// that Rust handlers leave untouched on a returning syscall or interrupt.
const _: () = {
    assert!(core::mem::size_of::<ExceptionContext>() == 272);
    assert!(core::mem::offset_of!(ExceptionContext, registers) == 0);
    assert!(core::mem::offset_of!(ExceptionContext, elr_el1) == 248);
    assert!(core::mem::offset_of!(ExceptionContext, spsr_el1) == 256);
    assert!(core::mem::offset_of!(ExceptionContext, esr_el1) == 264);
    assert!(core::mem::size_of::<ExceptionFrame>() == 816);
    assert!(core::mem::align_of::<ExceptionFrame>() == 16);
    assert!(core::mem::offset_of!(ExceptionFrame, context) == 0);
    assert!(core::mem::offset_of!(ExceptionFrame, simd) == 272);
    assert!(core::mem::offset_of!(ExceptionFrame, fpcr) == 784);
    assert!(core::mem::offset_of!(ExceptionFrame, fpsr) == 792);
    assert!(core::mem::offset_of!(ExceptionFrame, sp_el0) == 800);
    assert!(core::mem::offset_of!(ExceptionFrame, far_el1) == 808);
};
