/* AriCodeOS user program: static ELF64 at 0x400000, with separate,
 * page-aligned segments so the kernel can enforce W^X. */
ENTRY(_start)

USER_BASE = 0x400000;

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R X */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS
{
    . = USER_BASE;
    .text : ALIGN(4K) { *(.text._start) *(.text .text.*) } :text
    .rodata : ALIGN(4K) { *(.rodata .rodata.*) } :rodata
    .data : ALIGN(4K) { *(.data .data.*) *(.got .got.*) } :data
    .bss : ALIGN(4K) { *(.bss .bss.*) *(COMMON) . = ALIGN(4K); } :data
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) *(.note .note.*) *(.comment) }
}
