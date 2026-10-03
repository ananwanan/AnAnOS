.include "abi.inc"

.section .text
.global _start
.type _start, %function
_start:
    /* argc/argv/envp are on SP, not in private kernel argument registers. */
    mov x25, #1
    ldr x19, [sp]
    cmp x19, #1
    b.lt failed
    cmp x19, #8
    b.gt failed
    ldr x20, [sp, #8]
    ldrb w0, [x20]
    cbz w0, failed
    add x20, sp, #8
    ldr x0, [x20, x19, lsl #3]
    cbnz x0, failed
    add x20, x20, x19, lsl #3
    ldr x0, [x20, #8]
    cbz x0, failed
    ldrb w0, [x0]
    cbz w0, failed
    address x20, data_marker
    ldr x0, [x20]
    expect 0x1234
    address x20, buffer
    ldr x0, [x20]
    cbnz x0, failed

    mov x25, #2
    call SYS_GETPID
    cmp x0, #0
    b.le failed
    mov x26, x0
    mov x27, sp
    /* Exercise the suspended-context resume path before touching the VFS.
     * Retain these markers through the later blocking wait and child exec. */
    mov x9, #0x1234
    mov x18, #0xabcd
    mov x10, #0x1122
    mov v0.d[0], x10
    mov x10, #0x3344
    mov v0.d[1], x10
    mov x10, #0x5566
    mov v31.d[0], x10
    mov x10, #0x7788
    mov v31.d[1], x10
    mov x10, #0x00400000
    msr fpcr, x10
    mov x10, #0x10
    msr fpsr, x10
    mov x10, #0x60000000
    msr nzcv, x10
    call SYS_YIELD
    mrs x22, nzcv
    expect 0
    mov x16, #0x60000000
    cmp x22, x16
    b.ne failed
    bl verify_context
    mov x0, #1
    address x1, stat_buffer
    call SYS_CLOCK_GETTIME
    expect 0
    address x1, stat_buffer
    ldr x0, [x1, #8]
    mov x16, #0xca00
    movk x16, #0x3b9a, lsl #16
    cmp x0, x16
    b.hs failed
    mov x0, #1
    address x1, greeting
    mov x2, #greeting_size
    call SYS_WRITE
    expect greeting_size

    mov x25, #3
    address x0, directory
    mov x1, #directory_size
    call SYS_MKDIR
    expect 0

    mov x25, #4
    address x0, note_path
    mov x1, #note_path_size
    mov x2, #(2 | 0x40 | 0x200)
    call SYS_OPEN
    require_nonnegative
    mov x19, x0
    mov x0, x19
    address x1, file_contents
    mov x2, #file_contents_size
    call SYS_WRITE
    expect file_contents_size
    mov x0, x19
    mov x1, #0
    mov x2, #0
    call SYS_LSEEK
    expect 0

    mov x25, #5
    mov x0, x19
    call SYS_DUP
    require_nonnegative
    mov x20, x0
    mov x0, x20
    address x1, buffer
    mov x2, #file_contents_size
    call SYS_READ
    expect file_contents_size
    address x0, file_contents
    address x1, buffer
    mov x2, #file_contents_size
    bl equal_bytes
    cbnz x0, failed
    /* dup shares the open-file offset: original descriptor now sees EOF. */
    mov x0, x19
    address x1, buffer
    mov x2, #1
    call SYS_READ
    expect 0
    mov x0, x20
    mov x1, #0
    mov x2, #0
    call SYS_LSEEK
    expect 0
    mov x0, x19
    address x1, buffer
    mov x2, #file_contents_size
    call SYS_READ
    expect file_contents_size
    mov x0, x19
    address x1, stat_buffer
    call SYS_FSTAT
    expect 0
    address x1, stat_buffer
    ldr x0, [x1, #8]
    expect file_contents_size
    mov x0, x19
    call SYS_CLOSE
    expect 0
    mov x0, x20
    call SYS_CLOSE
    expect 0

    mov x25, #6
    address x0, directory
    mov x1, #directory_size
    call SYS_CHDIR
    expect 0
    address x0, buffer
    mov x1, #256
    call SYS_GETCWD
    require_nonnegative
    address x0, directory
    address x1, buffer
    mov x2, #directory_size
    bl equal_bytes
    cbnz x0, failed
    address x0, directory
    mov x1, #directory_size
    mov x2, #0x10000
    call SYS_OPEN
    require_nonnegative
    mov x19, x0
    address x1, buffer
    mov x2, #256
    call SYS_GETDENTS
    cmp x0, #0
    b.le failed
    mov x0, x19
    call SYS_CLOSE
    expect 0

    mov x25, #7
    address x0, note_path
    mov x1, #note_path_size
    address x2, renamed_path
    mov x3, #renamed_path_size
    call SYS_RENAME
    expect 0
    address x0, renamed_path
    mov x1, #renamed_path_size
    call SYS_UNLINK
    expect 0
    address x0, init_message_path
    mov x1, #init_message_path_size
    mov x2, #0
    call SYS_OPEN
    require_nonnegative
    mov x19, x0
    address x1, buffer
    mov x2, #256
    call SYS_READ
    cmp x0, #0
    b.le failed
    mov x2, x0
    mov x0, #1
    address x1, buffer
    call SYS_WRITE
    require_nonnegative
    mov x0, x19
    call SYS_CLOSE
    expect 0

    mov x25, #8
    call SYS_GETPID
    cmp x0, #0
    b.le failed
    address x0, child_path
    mov x1, #child_path_size
    address x2, child_arguments
    mov x3, #1
    address x4, child_environment
    mov x5, #1
    call SYS_SPAWN
    cmp x0, #0
    b.le failed
    mov x21, x0
    address x1, wait_status
    mov x2, #0
    mov x10, #0x60000000
    msr nzcv, x10
    call SYS_WAITPID
    mrs x22, nzcv
    cmp x0, x21
    b.ne failed
    mov x16, #0x60000000
    cmp x22, x16
    b.ne failed
    address x1, wait_status
    ldr w0, [x1]
    cmp w0, #(7 << 8)
    b.ne failed
    bl verify_context
    msr fpcr, xzr
    msr fpsr, xzr

    mov x25, #9
    mov x0, #1
    address x1, success
    mov x2, #success_size
    call SYS_WRITE
    expect success_size
    mov x0, #0
    call SYS_EXIT
    b .

/* Every register class and the stable PID/SP survive yield and blocking wait.
 * x0/x10/x16 are scratch; no kernel-private frame layout leaks into this ELF. */
verify_context:
    mov x0, x9
    expect 0x1234
    mov x0, x18
    expect 0xabcd
    mov x0, sp
    cmp x0, x27
    b.ne failed
    mov x0, v0.d[0]
    expect 0x1122
    mov x0, v0.d[1]
    expect 0x3344
    mov x0, v31.d[0]
    expect 0x5566
    mov x0, v31.d[1]
    expect 0x7788
    mrs x0, fpcr
    expect 0x00400000
    mrs x0, fpsr
    expect 0x10
    call SYS_GETPID
    cmp x0, x26
    b.ne failed
    ret

/* x0/x1 byte spans, x2 length; result x0=0 if equal. No hosted runtime. */
equal_bytes:
    cbz x2, 2f
1:
    ldrb w3, [x0], #1
    ldrb w4, [x1], #1
    cmp w3, w4
    b.ne 3f
    subs x2, x2, #1
    b.ne 1b
2:
    mov x0, #0
    ret
3:
    mov x0, #1
    ret
failed:
    mov x0, #2
    address x1, failure
    mov x2, #failure_size
    call SYS_WRITE
    add x0, x25, #100
    call SYS_EXIT
    b .
.size _start, .-_start

.section .rodata
greeting: .ascii "[ELF init] argc/envp, BSS, yield context and clock OK\n"
.equ greeting_size, .-greeting
file_contents: .ascii "AnanOS file roundtrip\n"
.equ file_contents_size, .-file_contents
directory: .ascii "/tmp/m4"
.equ directory_size, .-directory
note_path: .ascii "/tmp/m4/note"
.equ note_path_size, .-note_path
renamed_path: .ascii "/tmp/m4/renamed"
.equ renamed_path_size, .-renamed_path
init_message_path: .ascii "/init/etc/message"
.equ init_message_path_size, .-init_message_path
child_path: .ascii "/init/bin/child"
.equ child_path_size, .-child_path
child_token: .ascii "child-token"
.equ child_token_size, .-child_token
environment: .ascii "X=1"
.equ environment_size, .-environment
.balign 8
child_arguments: .quad child_token, child_token_size
child_environment: .quad environment, environment_size
success: .ascii "[ELF init] files, dirs, spawn/exec/wait context OK\n"
.equ success_size, .-success
failure: .ascii "[ELF init] FAILED (exit status identifies stage)\n"
.equ failure_size, .-failure

.section .data
.balign 8
data_marker: .quad 0x1234
.section .bss
.balign 8
buffer: .skip 256
stat_buffer: .skip 32
wait_status: .skip 8
