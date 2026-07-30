/*
 * SCTLR_EL1 位定义
 */
.equ SCTLR_EL1_M,       (1 << 0)
.equ SCTLR_EL1_C,       (1 << 2)
.equ SCTLR_EL1_I,       (1 << 12)
.equ SCTLR_EL1_E0E,     (1 << 24)
.equ SCTLR_EL1_EE,      (1 << 25)
.equ SCTLR_EL1_RES1,    0x30D00800

.section .text.boot

.global _start
.type _start, %function

.extern kernel_main
.extern __bss_start
.extern __bss_end
.extern __stack_top

_start:
    /*
     * x0 = Device Tree Blob 地址。
     * 保存到 x20，切换异常级不会破坏通用寄存器。
     */
    mov     x20, x0

    /*
     * 暂时只运行 CPU0。
     */
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, .Lsecondary_core

    /*
     * CurrentEL[3:2] 表示当前异常级。
     */
    mrs     x0, CurrentEL
    lsr     x0, x0, #2

    cmp     x0, #2
    b.eq    .Ldrop_from_el2

    cmp     x0, #1
    b.eq    .Lenter_el1

    b       .Lunsupported_el


/*
 * EL2 -> EL1
 */
.Ldrop_from_el2:
    /*
     * 设置 EL1 专用栈。
     */
    ldr     x0, =__stack_top
    msr     sp_el1, x0

    /*
     * 不保留固件的 HCR_EL2 内容。
     *
     * RW = 1：EL1 使用 AArch64。
     * VM = 0：关闭二阶段地址转换。
     * 其余陷阱位全部清零。
     */
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0

    /*
     * 清除可能存在的二阶段页表配置。
     */
    msr     vttbr_el2, xzr

    /*
     * 允许 EL1：
     * - 读取物理计数器 CNTPCT_EL0
     * - 使用物理定时器
     */
    mov     x0, #3
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr

    /*
     * 禁止 EL2 捕获 FP/SIMD 指令。
     */
    msr     cptr_el2, xzr

    /*
     * 允许 EL1 使用 FP/SIMD。
     *
     * CPACR_EL1.FPEN = 0b11
     */
    mrs     x0, cpacr_el1
    orr     x0, x0, #(3 << 20)
    msr     cpacr_el1, x0

    /*
     * 配置 SCTLR_EL1。
     *
     * 不要直接写 0，因为部分位要求保持为 1。
     * 先保留当前值，再补上 Cortex-A72/ARMv8-A 的 RES1 位。
     */
    mrs     x0, sctlr_el1

    /*
     * 0x30D00800:
     * 设置常见 ARMv8-A SCTLR_EL1 RES1 位(补齐要求为 1 的保留位)
     */
    ldr     x1, =SCTLR_EL1_RES1
    orr     x0, x0, x1

    /*
     * 目前关闭(当前阶段关闭 MMU 和缓存):
     * M  bit 0  MMU
     * C  bit 2  数据缓存
     * I  bit 12 指令缓存
     */
    bic     x0, x0, #SCTLR_EL1_M
    bic     x0, x0, #SCTLR_EL1_C
    bic     x0, x0, #SCTLR_EL1_I

    /*
     * EE/E0E = 0: EL1 和 EL0 使用小端序。
     */
    bic     x0, x0, #SCTLR_EL1_E0E
    bic     x0, x0, #SCTLR_EL1_EE

    msr     sctlr_el1, x0
    isb

    /*
     * eret 后从 .Lenter_el1 继续执行。
     */
    adr     x0, .Lenter_el1
    msr     elr_el2, x0

    /*
     * SPSR_EL2：
     *
     * D/A/I/F = 1：屏蔽 Debug、SError、IRQ、FIQ
     * M = 0101：进入 EL1h，使用 SP_EL1
     */
    mov     x0, #0x3c5
    msr     spsr_el2, x0

    dsb     sy
    isb
    eret


/*
 * 从这里开始必须位于 EL1。
 */
.Lenter_el1:
    /**
     * 检查是否位于 EL1。
     * 防御检查
     * 如果不是 EL1，直接进入无限循环。
     */
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #1
    b.ne    .Lunsupported_el

    /*
     * EL1h 应当已经选择 SP_EL1。
     * 再明确设置一次，方便排除栈问题。
     */
    msr     spsel, #1

    ldr     x0, =__stack_top
    mov     sp, x0

    isb

    /*
     * 清空 .bss。
     */
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

    /*
     * 第一个参数 x0 = DTB 地址。
     */
    mov     x0, x20
    bl      kernel_main


/*
 * Rust 内核不应该返回。
 */
.Lkernel_returned:
    wfe
    b       .Lkernel_returned


/*
 * CPU1～CPU3 暂停。
 */
.Lsecondary_core:
    wfe
    b       .Lsecondary_core


.Lunsupported_el:
    wfe
    b       .Lunsupported_el

.size _start, . - _start