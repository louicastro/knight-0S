OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(_start)

KERNEL_VIRT_BASE = 0xFFFFFFFF80000000;

PHDRS
{
    text   PT_LOAD FLAGS(5); /* PF_R | PF_X */
    rodata PT_LOAD FLAGS(4); /* PF_R */
    data   PT_LOAD FLAGS(6); /* PF_R | PF_W */
}

SECTIONS
{
    . = KERNEL_VIRT_BASE;
    __kernel_start = .;

    .text ALIGN(4K) :
    {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata ALIGN(4K) :
    {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4K);
    .data ALIGN(4K) :
    {
        *(.data .data.*)
    } :data

    .bss ALIGN(4K) :
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    } :data

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame*)
        *(.note*)
        *(.comment*)
    }
}
