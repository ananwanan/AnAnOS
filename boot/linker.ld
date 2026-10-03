ENTRY(_start)

SECTIONS
{
    . = 0x200000;
    __kernel_start = .;

    .text : ALIGN(4096)
    {
        __text_start = .;
        KEEP(*(.text.boot))
        
        . = ALIGN(2048);
        __vectors_start = .;
        KEEP(*(.text.vectors))
        __vectors_end = .;
        
        *(.text*)
        . = ALIGN(4096);
        __text_end = .;
    }

    .rodata : ALIGN(4096)
    {
        __rodata_start = .;
        *(.rodata*)
        . = ALIGN(4096);
        __rodata_end = .;
    }

    .data : ALIGN(4096)
    {
        __data_start = .;
        *(.data*)
        . = ALIGN(4096);
        __data_end = .;
    }

    .bss (NOLOAD) : ALIGN(4096)
    {
        __bss_start = .;

        *(.bss*)
        *(COMMON)

        . = ALIGN(4096);
        __bss_end = .;
    }

    .stack (NOLOAD) : ALIGN(4096)
    {
        __stack_bottom = .;

        /* 64 KiB 内核栈 */
        . += 64K;

        . = ALIGN(4096);
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
