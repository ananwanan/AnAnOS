ENTRY(_start)

SECTIONS
{
    . = 0x80000;
    __kernel_start = .;

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

    .stack (NOLOAD) : ALIGN(16)
    {
        __stack_bottom = .;

        /* 64 KiB 内核栈 */
        . += 64K;

        . = ALIGN(16);
        __stack_top = .;
    }

    __kernel_end = .;

    /DISCARD/ :
    {
        *(.comment*)
        *(.eh_frame*)
        *(.note*)
    }
}