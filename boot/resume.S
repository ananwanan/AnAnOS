/* Resume a complete, kernel-owned EL0 context after a cooperative/IRQ switch.
 * x0 = ExceptionFrame; the Rust layout checks cover all offsets below.
 * The suspended EL1 runner has the same 256-byte layout as boot/user.S.
 */
.section .text.user, "ax"
.align 4
.global arch_resume_user
.type arch_resume_user, %function
arch_resume_user:
    sub sp, sp, #256
    stp x19, x20, [sp, #0]
    stp x21, x22, [sp, #16]
    stp x23, x24, [sp, #32]
    stp x25, x26, [sp, #48]
    stp x27, x28, [sp, #64]
    stp x29, x30, [sp, #80]
    stp q8, q9, [sp, #96]
    stp q10, q11, [sp, #128]
    stp q12, q13, [sp, #160]
    stp q14, q15, [sp, #192]
    mrs x1, fpcr
    str x1, [sp, #224]
    mrs x1, fpsr
    str x1, [sp, #232]
    mrs x1, daif
    str x1, [sp, #240]
    mrs x1, sp_el0
    str x1, [sp, #248]
    msr daifset, #0xf
    mov x17, x0
    ldr x0, [x17, #248]
    msr elr_el1, x0
    ldr x0, [x17, #256]
    msr spsr_el1, x0
    ldr x0, [x17, #800]
    msr sp_el0, x0
    ldr x0, [x17, #784]
    msr fpcr, x0
    ldr x0, [x17, #792]
    msr fpsr, x0
    /* A64 pair-load offsets for SIMD are limited to +/- 1024 bytes. */
    ldp q0, q1, [x17, #272]
    ldp q2, q3, [x17, #304]
    ldp q4, q5, [x17, #336]
    ldp q6, q7, [x17, #368]
    ldp q8, q9, [x17, #400]
    ldp q10, q11, [x17, #432]
    ldp q12, q13, [x17, #464]
    ldp q14, q15, [x17, #496]
    ldp q16, q17, [x17, #528]
    ldp q18, q19, [x17, #560]
    ldp q20, q21, [x17, #592]
    ldp q22, q23, [x17, #624]
    ldp q24, q25, [x17, #656]
    ldp q26, q27, [x17, #688]
    ldp q28, q29, [x17, #720]
    ldp q30, q31, [x17, #752]
    ldp x0, x1, [x17, #0]
    ldp x2, x3, [x17, #16]
    ldp x4, x5, [x17, #32]
    ldp x6, x7, [x17, #48]
    ldp x8, x9, [x17, #64]
    ldp x10, x11, [x17, #80]
    ldp x12, x13, [x17, #96]
    ldp x14, x15, [x17, #112]
    ldp x18, x19, [x17, #144]
    ldp x20, x21, [x17, #160]
    ldp x22, x23, [x17, #176]
    ldp x24, x25, [x17, #192]
    ldp x26, x27, [x17, #208]
    ldp x28, x29, [x17, #224]
    ldr x30, [x17, #240]
    dsb sy
    isb
    clrex
    ldp x16, x17, [x17, #128]
    eret
.size arch_resume_user, . - arch_resume_user
