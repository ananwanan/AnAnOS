.section .text.boot

.global _start
.type _start, %function

.extern kernel_main
.extern __bss_start
.extern __bss_end
.extern __stack_top

_start:
    /*
     * x0 中通常保存 Device Tree Blob 地址。
     * 先保存起来，后面传给 Rust 内核。
     */
    mov     x20, x0

    /*
     * Raspberry Pi 4 有四个 CPU 核心。
     * MPIDR_EL1 的低 8 位是当前核心编号。
     */
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff

    /*
     * 暂时只让 CPU0 继续执行。
     * CPU1～CPU3 进入等待状态。
     */
    cbnz    x0, .Lsecondary_core

    /*
     * 设置栈顶。
     *
     * AArch64 的栈向低地址增长，因此 SP 应指向
     * __stack_top，而不是 __stack_bottom。
     */
    adrp    x0, __stack_top
    add     x0, x0, :lo12:__stack_top
    mov     sp, x0

    /*
     * 清空 .bss。
     *
     * Rust/C 语言要求未初始化的静态变量初始值为 0。
     * kernel8.img 中不会真正保存 .bss 内容，因此需要
     * 启动代码手动清零。
     */
    adrp    x0, __bss_start
    add     x0, x0, :lo12:__bss_start

    adrp    x1, __bss_end
    add     x1, x1, :lo12:__bss_end

.Lclear_bss:
    cmp     x0, x1
    b.hs    .Lbss_done

    str     xzr, [x0], #8
    b       .Lclear_bss

.Lbss_done:
    /*
     * 确保清零操作已经完成。
     */
    dsb     sy
    isb

    /*
     * 将 Device Tree 地址作为第一个参数传给 Rust。
     *
     * AArch64 调用约定中，第一个参数通过 x0 传递。
     */
    mov     x0, x20
    bl      kernel_main

    /*
     * kernel_main 返回类型为 !，理论上永远不会返回。
     */
.Lkernel_returned:
    wfe
    b       .Lkernel_returned

.Lsecondary_core:
    /*
     * 暂时停住其他核心。
     * 后面实现 SMP 时，会在这里加入多核唤醒流程。
     */
    wfe
    b       .Lsecondary_core

.size _start, . - _start