/* Provisional ABI numbers. Regeneration verifies these against kernel abi.rs. */
.equ SYS_WRITE, 1
.equ SYS_EXIT, 2
.equ SYS_YIELD, 3
.equ SYS_READ, 4
.equ SYS_OPEN, 5
.equ SYS_CLOSE, 6
.equ SYS_LSEEK, 7
.equ SYS_FSTAT, 8
.equ SYS_GETDENTS, 9
.equ SYS_MKDIR, 10
.equ SYS_UNLINK, 11
.equ SYS_RENAME, 12
.equ SYS_CHDIR, 13
.equ SYS_GETCWD, 14
.equ SYS_DUP, 15
.equ SYS_DUP2, 16
.equ SYS_GETPID, 17
.equ SYS_SPAWN, 18
.equ SYS_EXEC, 19
.equ SYS_WAITPID, 20
.equ SYS_CLOCK_GETTIME, 21

.macro address register, symbol
    adrp \register, \symbol
    add \register, \register, :lo12:\symbol
.endm
.macro call number
    mov x8, #\number
    svc #0
.endm
.macro expect value
    mov x16, #\value
    cmp x0, x16
    b.ne failed
.endm
.macro require_nonnegative
    tbnz x0, #63, failed
.endm
