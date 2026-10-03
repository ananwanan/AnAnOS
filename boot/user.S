/*
 * arch_enter_user(entry=x0, stack_top=x1, argument=x2) is an AAPCS64 call made
 * at EL1h. The synchronous CPU0 runner leaves its kernel call frame on SP_EL1
 * while EL0 uses SP_EL0. Lower-A64 vectors call Rust, then either ERET back to
 * the user or discard their own exception frame and branch arch_user_return.
 * The caller owns TTBR installation and all process memory/exit bookkeeping.
 */
.section .text.user_entry, "ax"
.align 4
.global arch_enter_user
.type arch_enter_user, %function
.global arch_user_return
.type arch_user_return, %function

.equ RUNNER_FRAME_SIZE, 256
.equ RUNNER_FPCR,       224
.equ RUNNER_FPSR,       232
.equ RUNNER_DAIF,       240
.equ RUNNER_SP_EL0,     248

.if RUNNER_FRAME_SIZE != (RUNNER_SP_EL0 + 8)
    .error "EL0 runner frame must be exactly 256 bytes and 16-byte aligned"
.endif

arch_enter_user:
    sub sp, sp, #RUNNER_FRAME_SIZE
    stp x19, x20, [sp, #0]
    stp x21, x22, [sp, #16]
    stp x23, x24, [sp, #32]
    stp x25, x26, [sp, #48]
    stp x27, x28, [sp, #64]
    stp x29, x30, [sp, #80]
    stp q8, q9,   [sp, #96]
    stp q10, q11, [sp, #128]
    stp q12, q13, [sp, #160]
    stp q14, q15, [sp, #192]
    mrs x3, fpcr
    str x3, [sp, #RUNNER_FPCR]
    mrs x3, fpsr
    str x3, [sp, #RUNNER_FPSR]
    mrs x3, sp_el0
    str x3, [sp, #RUNNER_SP_EL0]
    mrs x3, daif
    str x3, [sp, #RUNNER_DAIF]

    /* No IRQ may observe a partially installed EL0 entry register context. */
    msr daifset, #0xf
    msr elr_el1, x0
    msr sp_el0, x1
    and x3, x3, #0x80
    mov x4, #0x340
    orr x3, x3, x4
    /* M=0000 (EL0t), D/A/F=1, I copied from the original kernel DAIF. */
    msr spsr_el1, x3

    /* Do not expose live kernel register contents to the first EL0 entry. */
    msr fpcr, xzr
    msr fpsr, xzr
    movi v0.16b, #0
    movi v1.16b, #0
    movi v2.16b, #0
    movi v3.16b, #0
    movi v4.16b, #0
    movi v5.16b, #0
    movi v6.16b, #0
    movi v7.16b, #0
    movi v8.16b, #0
    movi v9.16b, #0
    movi v10.16b, #0
    movi v11.16b, #0
    movi v12.16b, #0
    movi v13.16b, #0
    movi v14.16b, #0
    movi v15.16b, #0
    movi v16.16b, #0
    movi v17.16b, #0
    movi v18.16b, #0
    movi v19.16b, #0
    movi v20.16b, #0
    movi v21.16b, #0
    movi v22.16b, #0
    movi v23.16b, #0
    movi v24.16b, #0
    movi v25.16b, #0
    movi v26.16b, #0
    movi v27.16b, #0
    movi v28.16b, #0
    movi v29.16b, #0
    movi v30.16b, #0
    movi v31.16b, #0
    mov x0, x2
    mov x1, xzr
    mov x2, xzr
    mov x3, xzr
    mov x4, xzr
    mov x5, xzr
    mov x6, xzr
    mov x7, xzr
    mov x8, xzr
    mov x9, xzr
    mov x10, xzr
    mov x11, xzr
    mov x12, xzr
    mov x13, xzr
    mov x14, xzr
    mov x15, xzr
    mov x16, xzr
    mov x17, xzr
    mov x18, xzr
    mov x19, xzr
    mov x20, xzr
    mov x21, xzr
    mov x22, xzr
    mov x23, xzr
    mov x24, xzr
    mov x25, xzr
    mov x26, xzr
    mov x27, xzr
    mov x28, xzr
    mov x29, xzr
    mov x30, xzr
    dsb sy
    isb
    clrex
    eret
.size arch_enter_user, . - arch_enter_user

arch_user_return:
    /* Entered only from a terminating EL0 exception; DAIF is still masked. */
    ldp q8, q9,   [sp, #96]
    ldp q10, q11, [sp, #128]
    ldp q12, q13, [sp, #160]
    ldp q14, q15, [sp, #192]
    ldr x0, [sp, #RUNNER_FPCR]
    msr fpcr, x0
    ldr x0, [sp, #RUNNER_FPSR]
    msr fpsr, x0
    ldr x0, [sp, #RUNNER_SP_EL0]
    msr sp_el0, x0
    ldp x19, x20, [sp, #0]
    ldp x21, x22, [sp, #16]
    ldp x23, x24, [sp, #32]
    ldp x25, x26, [sp, #48]
    ldp x27, x28, [sp, #64]
    ldp x29, x30, [sp, #80]
    ldr x16, [sp, #RUNNER_DAIF]
    add sp, sp, #RUNNER_FRAME_SIZE
    mov x0, xzr
    clrex
    msr daif, x16
    isb
    ret
.size arch_user_return, . - arch_user_return

/*
 * Copied as data to an independently owned RX user page. Every address inside
 * each image uses ADR or a local branch, so it remains position-independent.
 * No linker absolute address, kernel function call or host runtime is used.
 */
.section .rodata.user, "a"
.align 4
/* Supplied by Rust global_asm! from the single provisional ABI definition. */
.equ USER_SYS_WRITE,       {sys_write}
.equ USER_SYS_EXIT,        {sys_exit}
.equ USER_SYS_YIELD,       {sys_yield}
.equ USER_STDOUT,          {stdout}
.equ USER_STDERR,          {stderr}
.equ USER_MAX_WRITE_BYTES, {max_write_bytes}
.equ USER_SVC_IMMEDIATE,   {svc_immediate}
.equ USER_EBADF,           {errno_ebadf}
.equ USER_EFAULT,          {errno_efault}
.equ USER_EINVAL,          {errno_einval}
.equ USER_ENOSYS,          {errno_enosys}

/* Defined AArch64 FP bits: round toward +inf and cumulative inexact flag. */
.equ USER_FPCR, 0x00400000
.equ USER_FPSR, 0x10

.global __user_image_start
.global __user_image_end
__user_image_start:
    /* EL0 starts with an empty FP/SIMD context rather than kernel remnants. */
    mrs x4, fpcr
    cbnz x4, .Luser_failed
    mrs x4, fpsr
    cbnz x4, .Luser_failed
    umov x4, v0.d[0]
    cbnz x4, .Luser_failed
    umov x4, v0.d[1]
    cbnz x4, .Luser_failed
    umov x4, v31.d[0]
    cbnz x4, .Luser_failed
    umov x4, v31.d[1]
    cbnz x4, .Luser_failed

    /* Exercise a real private stack rather than relying only on registers. */
    sub sp, sp, #16
    stp x19, x20, [sp]
    mov x9, #0x999
    mov x18, #0x1818
    mov x19, #0x123
    mov x20, sp
    mov x29, #0x2929
    movi v0.16b, #0x5a
    movi v31.16b, #0xa5
    mov x4, #USER_FPCR
    msr fpcr, x4
    mov x4, #USER_FPSR
    msr fpsr, x4
    adr x1, .Luser_message
    mov x2, #(.Luser_message_end - .Luser_message)
    mov x0, #USER_STDOUT
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmp x0, #(.Luser_message_end - .Luser_message)
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* The syscall must reject an EL1-only pointer without reading it. */
    mov x1, #0x200000
    mov x2, #1
    mov x0, #USER_STDOUT
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmn x0, #USER_EFAULT
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* fd validation precedes the zero-length shortcut, even for fd 0. */
    mov x0, #0
    mov x1, #-1
    mov x2, #0
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmn x0, #USER_EBADF
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* Oversized writes must fail before looking at the invalid pointer. */
    mov x0, #USER_STDOUT
    mov x1, #-1
    mov x2, #(USER_MAX_WRITE_BYTES + 1)
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmn x0, #USER_EINVAL
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* An unsupported SVC immediate must not dispatch the EXIT number. */
    mov x0, #0
    mov x8, #USER_SYS_EXIT
    svc #(USER_SVC_IMMEDIATE + 1)
    cmn x0, #USER_EINVAL
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* A valid zero-byte write must not dereference this invalid pointer. */
    mov x0, #USER_STDOUT
    mov x1, #-1
    mov x2, #0
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cbnz x0, .Luser_failed
    bl .Lverify_user_state

    /* address + length overflows u64; no byte may be read or emitted. */
    mov x0, #USER_STDOUT
    mov x1, #-2
    mov x2, #4
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmn x0, #USER_EFAULT
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* x20 = initial stack top - 16; only the first two bytes are mapped. */
    mov x0, #USER_STDOUT
    add x1, x20, #14
    mov x2, #4
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmn x0, #USER_EFAULT
    b.ne .Luser_failed
    bl .Lverify_user_state

    /* Write seven bytes spanning the final two private stack pages. The
     * start is initial stack top - 4099, three bytes below their boundary.
     * Byte stores avoid requiring unaligned multi-byte data accesses. */
    sub x1, x20, #4083
    adr x6, .Luser_stack_message
    mov x7, x1
    mov x5, #(.Luser_stack_message_end - .Luser_stack_message)
.Lcopy_stack_message:
    ldrb w4, [x6], #1
    strb w4, [x7], #1
    subs x5, x5, #1
    b.ne .Lcopy_stack_message
    mov x0, #USER_STDERR
    mov x2, #(.Luser_stack_message_end - .Luser_stack_message)
    mov x8, #USER_SYS_WRITE
    svc #USER_SVC_IMMEDIATE
    cmp x0, #(.Luser_stack_message_end - .Luser_stack_message)
    b.ne .Luser_failed
    bl .Lverify_user_state

    mov x8, #0xffff
    svc #USER_SVC_IMMEDIATE
    cmn x0, #USER_ENOSYS
    b.ne .Luser_failed
    bl .Lverify_user_state

    mov x8, #USER_SYS_YIELD
    svc #USER_SVC_IMMEDIATE
    cbnz x0, .Luser_failed
    bl .Lverify_user_state
    ldp x19, x20, [sp]
    add sp, sp, #16
    mov x0, #0
    mov x8, #USER_SYS_EXIT
    svc #USER_SVC_IMMEDIATE
.Luser_finished:
    b .Luser_finished
.Luser_failed:
    mov x0, #1
    mov x8, #USER_SYS_EXIT
    svc #USER_SVC_IMMEDIATE
    b .Luser_finished

.Lverify_user_state:
    /* Check both 64-bit lanes of caller-clobbered vectors across each SVC. */
    mov x4, sp
    cmp x4, x20
    b.ne .Luser_failed
    cmp x9, #0x999
    b.ne .Luser_failed
    mov x4, #0x1818
    cmp x18, x4
    b.ne .Luser_failed
    cmp x19, #0x123
    b.ne .Luser_failed
    mov x4, #0x2929
    cmp x29, x4
    b.ne .Luser_failed
    ldr x4, .Luser_simd0
    umov x5, v0.d[0]
    cmp x4, x5
    b.ne .Luser_failed
    umov x5, v0.d[1]
    cmp x4, x5
    b.ne .Luser_failed
    ldr x4, .Luser_simd31
    umov x5, v31.d[0]
    cmp x4, x5
    b.ne .Luser_failed
    umov x5, v31.d[1]
    cmp x4, x5
    b.ne .Luser_failed
    mrs x4, fpcr
    mov x5, #USER_FPCR
    cmp x4, x5
    b.ne .Luser_failed
    mrs x4, fpsr
    cmp x4, #USER_FPSR
    b.ne .Luser_failed
    ret

.align 3
.Luser_simd0:
    .quad 0x5a5a5a5a5a5a5a5a
.Luser_simd31:
    .quad 0xa5a5a5a5a5a5a5a5
.Luser_message:
    .ascii "[EL0] hello through SVC\n"
.Luser_message_end:
.Luser_stack_message:
    .ascii "stack!\n"
.Luser_stack_message_end:
.align 2
__user_image_end:
.size __user_image_start, __user_image_end - __user_image_start

.align 4
.global __user_fault_start
.global __user_fault_end
.global __user_fault_pc
__user_fault_start:
__user_fault_pc:
    ldr x1, [x0]
    mov x0, #1
    mov x8, #USER_SYS_EXIT
    svc #USER_SVC_IMMEDIATE
.Lkernel_fault_unexpected:
    b .Lkernel_fault_unexpected
__user_fault_end:
.size __user_fault_start, __user_fault_end - __user_fault_start

.align 4
.global __user_readonly_start
.global __user_readonly_end
.global __user_readonly_pc
__user_readonly_start:
    adr x0, __user_readonly_start
__user_readonly_pc:
    str xzr, [x0]
    mov x0, #1
    mov x8, #USER_SYS_EXIT
    svc #USER_SVC_IMMEDIATE
.Lreadonly_fault_unexpected:
    b .Lreadonly_fault_unexpected
__user_readonly_end:
.size __user_readonly_start, __user_readonly_end - __user_readonly_start

.align 4
.global __user_guard_start
.global __user_guard_end
.global __user_guard_pc
__user_guard_start:
    /* Bring-up ABI: a 16 KiB stack with the preceding page left unmapped. */
    sub x0, sp, #4, lsl #12
    sub x0, x0, #8
__user_guard_pc:
    str xzr, [x0]
    mov x0, #1
    mov x8, #USER_SYS_EXIT
    svc #USER_SVC_IMMEDIATE
.Lguard_fault_unexpected:
    b .Lguard_fault_unexpected
__user_guard_end:
.size __user_guard_start, __user_guard_end - __user_guard_start

.align 4
.global __user_timeout_start
.global __user_timeout_end
__user_timeout_start:
    /* The kernel must regain control through lower-A64 timer IRQ entries. */
    b __user_timeout_start
__user_timeout_end:
.size __user_timeout_start, __user_timeout_end - __user_timeout_start
