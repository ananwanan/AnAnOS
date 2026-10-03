.include "abi.inc"
.section .text
.global _start
.type _start, %function
_start:
    mov x25, #1
    ldr x0, [sp]
    cmp x0, #1
    b.ne failed
    ldr x0, [sp, #8]
    address x1, exec_token
    mov x2, #exec_token_size
    bl check_string
    cbnz x0, failed
    ldr x0, [sp, #16]
    cbnz x0, failed
    ldr x0, [sp, #24]
    address x1, environment
    mov x2, #environment_size
    bl check_string
    cbnz x0, failed
    ldr x0, [sp, #32]
    cbnz x0, failed
    address x1, zero_area
    ldr x0, [x1]
    cbnz x0, failed
    address x1, data_marker
    ldr x0, [x1]
    expect 0x2345
    call SYS_GETPID
    cmp x0, #0
    b.le failed
    mov x0, #1
    address x1, success
    mov x2, #success_size
    call SYS_WRITE
    expect success_size
    mov x0, #7
    call SYS_EXIT
    b .
check_string:
1:
    ldrb w3, [x0], #1
    ldrb w4, [x1], #1
    cmp w3, w4
    b.ne 3f
    subs x2, x2, #1
    b.ne 1b
    ldrb w3, [x0]
    cbnz w3, 3f
    mov x0, #0
    ret
3:
    mov x0, #1
    ret
failed:
    add x0, x25, #120
    call SYS_EXIT
    b .
.size _start, .-_start
.section .rodata
exec_token: .ascii "exec-token"
.equ exec_token_size, .-exec_token
environment: .ascii "X=1"
.equ environment_size, .-environment
success: .ascii "[ELF exec] replaced image, fresh data/BSS; exit 7\n"
.equ success_size, .-success
.section .data
.balign 8
data_marker: .quad 0x2345
.section .bss
.balign 8
zero_area: .skip 16
