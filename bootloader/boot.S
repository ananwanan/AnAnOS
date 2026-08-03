.section .text.boot

.global _start
.type _start, %function

.extern bootloader_main
.extern __bss_start
.extern __bss_end
.extern __stack_top

_start:
    mov     x20, x0

    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, .Lsecondary_core

    ldr     x0, =__stack_top
    mov     sp, x0

    ldr     x0, =__bss_start
    ldr     x1, =__bss_end

.Lclear_bss:
    cmp     x0, x1
    b.hs    .Lbss_done
    str     xzr, [x0], #8
    b       .Lclear_bss

.Lbss_done:
    dsb     sy
    isb

    mov     x0, x20
    bl      bootloader_main

.Lreturned:
    wfe
    b       .Lreturned

.Lsecondary_core:
    wfe
    b       .Lsecondary_core

.size _start, . - _start
