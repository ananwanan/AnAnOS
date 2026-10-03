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
    address x1, child_token
    mov x2, #child_token_size
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
    call SYS_GETPID
    cmp x0, #0
    b.le failed
    mov x0, #1
    address x1, greeting
    mov x2, #greeting_size
    call SYS_WRITE
    expect greeting_size
    mov x25, #2
    address x0, exec_path
    mov x1, #exec_path_size
    address x2, exec_arguments
    mov x3, #1
    address x4, exec_environment
    mov x5, #1
    call SYS_EXEC
    /* Successful exec never returns to this image. */
    b failed
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
    add x0, x25, #110
    call SYS_EXIT
    b .
.size _start, .-_start
.section .rodata
child_token: .ascii "child-token"
.equ child_token_size, .-child_token
environment: .ascii "X=1"
.equ environment_size, .-environment
greeting: .ascii "[ELF child] inherited argv/envp; exec next\n"
.equ greeting_size, .-greeting
exec_path: .ascii "/init/bin/exec"
.equ exec_path_size, .-exec_path
exec_token: .ascii "exec-token"
.equ exec_token_size, .-exec_token
.balign 8
exec_arguments: .quad exec_token, exec_token_size
exec_environment: .quad environment, environment_size
.section .data
.quad 0x5678
.section .bss
.skip 16
