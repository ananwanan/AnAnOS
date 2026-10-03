.section .text.boot

.global _start
.type _start, %function

.extern bootloader_main
.extern __bss_start
.extern __bss_end
.extern __stack_top
.extern __boot_dtb_start
.extern __boot_dtb_end

_start:
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbnz    x1, .Lsecondary_core

    /*
     * 固件仅知道 bootloader 的大小，不知道之后 UART 内核会占用的空间。
     * 必须在设置栈/清空 BSS/接收内核之前保留 DTB 内容，而不只是指针。
     *
     * 固件传入的物理指针是可信启动输入；下列范围检查排除空指针、
     * 非对齐地址、MMIO/高地址和越界大小，但不能证明任意 RAM 地址可读。
     * 当前 Pi 固件 DTB 位于低 1 GiB、8 字节对齐，支持 v17 的 40 字节头。
     * 整段代码不使用栈；临时寄存器 x1-x6 被覆盖，x20 保存重定位结果。
     */
    mov     x20, x0
    cbz     x20, .Lmissing_dtb
    tst     x20, #7
    b.ne    .Lmissing_dtb
    mov     x3, #0x40000000
    sub     x4, x3, #40
    cmp     x20, x4
    b.hi    .Lmissing_dtb

    ldr     w1, [x20]
    rev     w1, w1
    ldr     w2, =0xd00dfeed
    cmp     w1, w2
    b.ne    .Lmissing_dtb

    ldr     w1, [x20, #4]
    rev     w1, w1
    cmp     x1, #40
    b.lo    .Lmissing_dtb
    ldr     x2, =__boot_dtb_start
    ldr     x4, =__boot_dtb_end
    sub     x4, x4, x2
    cmp     x1, x4
    b.hi    .Lmissing_dtb
    adds    x4, x20, x1
    b.cs    .Lmissing_dtb
    cmp     x4, x3
    b.hi    .Lmissing_dtb

    /* memmove 方向使源 DTB 和保留缓冲区重叠时也能正确复制。 */
    mov     x5, x20
    mov     x20, x2
    cmp     x2, x5
    b.ls    .Lcopy_dtb_forward
    add     x2, x2, x1
    add     x5, x5, x1

.Lcopy_dtb_backward:
    ldrb    w6, [x5, #-1]!
    strb    w6, [x2, #-1]!
    subs    x1, x1, #1
    b.ne    .Lcopy_dtb_backward
    b       .Ldtb_ready

.Lcopy_dtb_forward:
    ldrb    w6, [x5], #1
    strb    w6, [x2], #1
    subs    x1, x1, #1
    b.ne    .Lcopy_dtb_forward
    b       .Ldtb_ready

.Lmissing_dtb:
    /* 仍进入 Rust/UART；内核以缺失 DTB 的错误显式拒绝启用分配器。 */
    mov     x20, xzr

.Ldtb_ready:
    dsb     sy

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
