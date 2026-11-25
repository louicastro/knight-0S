ENTRY(_start)

USER_BASE = 0x0000000000400000;

PHDRS
{
    text   PT_LOAD FLAGS(5); /* PF_R | PF_X */
    rodata PT_LOAD FLAGS(4); /* PF_R */
    data   PT_LOAD FLAGS(6); /* PF_R | PF_W */
}

SECTIONS
{
    . = USER_BASE;

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
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ :
    {
        *(.eh_frame*)
        *(.note*)
        *(.comment*)
    }
}
