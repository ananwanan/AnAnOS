.section .text.vectors, "ax"

.align 11
.global __exception_vectors
.type __exception_vectors, %function

.extern rust_sync_exception
.extern rust_irq_exception
.ifdef ANANOS_USERSPACE
.extern rust_user_sync_exception
.extern rust_user_irq_exception
.extern arch_user_return
.endif

/*
 * The first 272 bytes match Rust's repr(C) ExceptionContext; arch/context.rs
 * checks the full ExceptionFrame layout. Assembly saves/restores q0..q31 at
 * 272..783, FPCR at 784 and FPSR at 792; returning Rust handlers retain them.
 * An exception can interrupt any instruction, so the ordinary AAPCS64
 * callee-saved subset is insufficient; preserve every 128-bit SIMD register.
 * SP_EL0 and FAR_EL1 follow at 800 and 808. The full 816-byte frame preserves
 * interrupted stack alignment for EL1h and lower-EL AArch64 exceptions alike.
 */
.equ CONTEXT_SIZE,   816
.equ SIMD_OFFSET,    272
.equ FPCR_OFFSET,    784
.equ FPSR_OFFSET,    792
.equ USER_SP_OFFSET, 800
.equ FAR_OFFSET,     808

.if (CONTEXT_SIZE % 16) != 0
    .error "exception frame must preserve AAPCS64 stack alignment"
.endif
.if SIMD_OFFSET != (34 * 8)
    .error "SIMD state must follow the Rust ExceptionContext prefix"
.endif
.if FPCR_OFFSET != (SIMD_OFFSET + 32 * 16)
    .error "FPCR slot overlaps SIMD state"
.endif
.if USER_SP_OFFSET != (FPSR_OFFSET + 8)
    .error "SP_EL0 must follow the saved floating-point state"
.endif
.if FAR_OFFSET != (USER_SP_OFFSET + 8)
    .error "FAR_EL1 must follow SP_EL0"
.endif
.if CONTEXT_SIZE != (FAR_OFFSET + 8)
    .error "exception frame tail does not match its allocation"
.endif

.macro vector_entry handler
    b \handler
    .space 124
.endm


.macro SAVE_CONTEXT
    sub sp, sp, #CONTEXT_SIZE

    stp x0,  x1,  [sp, #(0  * 8)]
    stp x2,  x3,  [sp, #(2  * 8)]
    stp x4,  x5,  [sp, #(4  * 8)]
    stp x6,  x7,  [sp, #(6  * 8)]
    stp x8,  x9,  [sp, #(8  * 8)]
    stp x10, x11, [sp, #(10 * 8)]
    stp x12, x13, [sp, #(12 * 8)]
    stp x14, x15, [sp, #(14 * 8)]
    stp x16, x17, [sp, #(16 * 8)]
    stp x18, x19, [sp, #(18 * 8)]
    stp x20, x21, [sp, #(20 * 8)]
    stp x22, x23, [sp, #(22 * 8)]
    stp x24, x25, [sp, #(24 * 8)]
    stp x26, x27, [sp, #(26 * 8)]
    stp x28, x29, [sp, #(28 * 8)]

    str x30, [sp, #(30 * 8)]

    mrs x0, elr_el1
    str x0, [sp, #(31 * 8)]

    mrs x0, spsr_el1
    str x0, [sp, #(32 * 8)]

    mrs x0, esr_el1
    str x0, [sp, #(33 * 8)]

    stp q0,  q1,  [sp, #(SIMD_OFFSET + 0)]
    stp q2,  q3,  [sp, #(SIMD_OFFSET + 32)]
    stp q4,  q5,  [sp, #(SIMD_OFFSET + 64)]
    stp q6,  q7,  [sp, #(SIMD_OFFSET + 96)]
    stp q8,  q9,  [sp, #(SIMD_OFFSET + 128)]
    stp q10, q11, [sp, #(SIMD_OFFSET + 160)]
    stp q12, q13, [sp, #(SIMD_OFFSET + 192)]
    stp q14, q15, [sp, #(SIMD_OFFSET + 224)]
    stp q16, q17, [sp, #(SIMD_OFFSET + 256)]
    stp q18, q19, [sp, #(SIMD_OFFSET + 288)]
    stp q20, q21, [sp, #(SIMD_OFFSET + 320)]
    stp q22, q23, [sp, #(SIMD_OFFSET + 352)]
    stp q24, q25, [sp, #(SIMD_OFFSET + 384)]
    stp q26, q27, [sp, #(SIMD_OFFSET + 416)]
    stp q28, q29, [sp, #(SIMD_OFFSET + 448)]
    stp q30, q31, [sp, #(SIMD_OFFSET + 480)]

    mrs x0, fpcr
    str x0, [sp, #FPCR_OFFSET]
    mrs x0, fpsr
    str x0, [sp, #FPSR_OFFSET]
    mrs x0, sp_el0
    str x0, [sp, #USER_SP_OFFSET]
    mrs x0, far_el1
    str x0, [sp, #FAR_OFFSET]
.endm


.macro RESTORE_CONTEXT
    ldr x0, [sp, #(31 * 8)]
    msr elr_el1, x0

    ldr x0, [sp, #(32 * 8)]
    msr spsr_el1, x0

    ldp q0,  q1,  [sp, #(SIMD_OFFSET + 0)]
    ldp q2,  q3,  [sp, #(SIMD_OFFSET + 32)]
    ldp q4,  q5,  [sp, #(SIMD_OFFSET + 64)]
    ldp q6,  q7,  [sp, #(SIMD_OFFSET + 96)]
    ldp q8,  q9,  [sp, #(SIMD_OFFSET + 128)]
    ldp q10, q11, [sp, #(SIMD_OFFSET + 160)]
    ldp q12, q13, [sp, #(SIMD_OFFSET + 192)]
    ldp q14, q15, [sp, #(SIMD_OFFSET + 224)]
    ldp q16, q17, [sp, #(SIMD_OFFSET + 256)]
    ldp q18, q19, [sp, #(SIMD_OFFSET + 288)]
    ldp q20, q21, [sp, #(SIMD_OFFSET + 320)]
    ldp q22, q23, [sp, #(SIMD_OFFSET + 352)]
    ldp q24, q25, [sp, #(SIMD_OFFSET + 384)]
    ldp q26, q27, [sp, #(SIMD_OFFSET + 416)]
    ldp q28, q29, [sp, #(SIMD_OFFSET + 448)]
    ldp q30, q31, [sp, #(SIMD_OFFSET + 480)]

    ldr x0, [sp, #FPCR_OFFSET]
    msr fpcr, x0
    ldr x0, [sp, #FPSR_OFFSET]
    msr fpsr, x0
    ldr x0, [sp, #USER_SP_OFFSET]
    msr sp_el0, x0

    ldp x0,  x1,  [sp, #(0  * 8)]
    ldp x2,  x3,  [sp, #(2  * 8)]
    ldp x4,  x5,  [sp, #(4  * 8)]
    ldp x6,  x7,  [sp, #(6  * 8)]
    ldp x8,  x9,  [sp, #(8  * 8)]
    ldp x10, x11, [sp, #(10 * 8)]
    ldp x12, x13, [sp, #(12 * 8)]
    ldp x14, x15, [sp, #(14 * 8)]
    ldp x16, x17, [sp, #(16 * 8)]
    ldp x18, x19, [sp, #(18 * 8)]
    ldp x20, x21, [sp, #(20 * 8)]
    ldp x22, x23, [sp, #(22 * 8)]
    ldp x24, x25, [sp, #(24 * 8)]
    ldp x26, x27, [sp, #(26 * 8)]
    ldp x28, x29, [sp, #(28 * 8)]

    ldr x30, [sp, #(30 * 8)]

    add sp, sp, #CONTEXT_SIZE
.endm


/*
 * AArch64 向量表共 16 个入口。
 * 每个入口占 128 字节，整个表共 2048 字节。
 */
__exception_vectors:

/* Current EL, using SP_EL0 */
vector_entry current_sp0_sync
vector_entry current_sp0_irq
vector_entry current_sp0_fiq
vector_entry current_sp0_serror

/* Current EL, using SP_ELx */
vector_entry current_spx_sync
vector_entry current_spx_irq
vector_entry current_spx_fiq
vector_entry current_spx_serror

/* Lower EL, AArch64 */
vector_entry lower_a64_sync
vector_entry lower_a64_irq
vector_entry lower_a64_fiq
vector_entry lower_a64_serror

/* Lower EL, AArch32 */
vector_entry lower_a32_sync
vector_entry lower_a32_irq
vector_entry lower_a32_fiq
vector_entry lower_a32_serror


current_sp0_sync:
    b exception_unhandled

current_sp0_irq:
    b exception_unhandled

current_sp0_fiq:
    b exception_unhandled

current_sp0_serror:
    b exception_unhandled


/*
 * 我们当前运行在 EL1h，使用 SP_EL1。
 * 因此 BRK 会进入 current_spx_sync。
 */
current_spx_sync:
    SAVE_CONTEXT

    mov x0, sp
    bl rust_sync_exception

    RESTORE_CONTEXT
    eret


current_spx_irq:
    SAVE_CONTEXT

    mov x0, sp
    bl rust_irq_exception

    RESTORE_CONTEXT
    eret


current_spx_fiq:
    b exception_unhandled

current_spx_serror:
    b exception_unhandled


lower_a64_sync:
.ifdef ANANOS_USERSPACE
    SAVE_CONTEXT

    /* Rust kernel code must not inherit an EL0-controlled FP environment. */
    msr fpcr, xzr
    msr fpsr, xzr

    mov x0, sp
    bl rust_user_sync_exception
    cbnz x0, user_exception_return

    RESTORE_CONTEXT
    eret
.else
    b exception_unhandled
.endif

lower_a64_irq:
.ifdef ANANOS_USERSPACE
    SAVE_CONTEXT

    msr fpcr, xzr
    msr fpsr, xzr

    mov x0, sp
    bl rust_user_irq_exception
    cbnz x0, user_exception_return

    RESTORE_CONTEXT
    eret
.else
    b exception_unhandled
.endif

.ifdef ANANOS_USERSPACE
/* The CPU is in EL1h and SP_EL1 now points at the suspended runner frame. */
user_exception_return:
    add sp, sp, #CONTEXT_SIZE
    b arch_user_return
.endif

lower_a64_fiq:
    b exception_unhandled

lower_a64_serror:
    b exception_unhandled


lower_a32_sync:
    b exception_unhandled

lower_a32_irq:
    b exception_unhandled

lower_a32_fiq:
    b exception_unhandled

lower_a32_serror:
    b exception_unhandled


exception_unhandled:
    wfe
    b exception_unhandled

.size __exception_vectors, . - __exception_vectors
