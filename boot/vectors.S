.section .text.vectors, "ax"

.align 11
.global __exception_vectors
.type __exception_vectors, %function

.extern rust_sync_exception
.extern rust_irq_exception

.macro vector_entry handler
    b \handler
    .space 124
.endm


.macro SAVE_CONTEXT
    sub sp, sp, #(34 * 8)

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
.endm


.macro RESTORE_CONTEXT
    ldr x0, [sp, #(31 * 8)]
    msr elr_el1, x0

    ldr x0, [sp, #(32 * 8)]
    msr spsr_el1, x0

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

    add sp, sp, #(34 * 8)
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
    b exception_unhandled

lower_a64_irq:
    b exception_unhandled

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
