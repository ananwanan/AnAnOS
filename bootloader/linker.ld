ENTRY(_start)

SECTIONS
{
    . = 0x80000;
    __bootloader_start = .;

    .text : ALIGN(16)
    {
        KEEP(*(.text.boot))
        *(.text*)
    }

    .rodata : ALIGN(16)
    {
        *(.rodata*)
    }

    .data : ALIGN(16)
    {
        *(.data*)
    }

    .bss (NOLOAD) : ALIGN(16)
    {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /*
     * 早期汇编在清空 BSS 之前复制固件 DTB。
     * 此缓冲区不属于 BSS，不写入 bootloader8.img，也不被 BSS 初始化覆盖。
     */
    .boot_dtb (NOLOAD) : ALIGN(16)
    {
        __boot_dtb_start = .;
        . += 256K;
        __boot_dtb_end = .;
    }

    .stack (NOLOAD) : ALIGN(16)
    {
        __stack_bottom = .;
        . += 32K;
        . = ALIGN(16);
        __stack_top = .;
    }

    __bootloader_end = .;
    ASSERT(__bootloader_end < 0x200000,
           "bootloader, preserved DTB and stack overlap UART kernel load address")

    /DISCARD/ :
    {
        *(.comment*)
        *(.eh_frame*)
        *(.note*)
    }
}
