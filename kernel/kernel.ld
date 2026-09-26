/* AriKernel: ELF64 estático enlazado en la mitad alta.
 * Segmentos separados y alineados a página para poder aplicar W^X
 * desde el bootloader (texto RX, rodata R, data/bss RW). */
ENTRY(kernel_entry)

KERNEL_VIRT_BASE = 0xFFFFFFFF80000000;

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R X */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS
{
    . = KERNEL_VIRT_BASE;
    __kernel_start = .;

    .text : ALIGN(4K) {
        __text_start = .;
        *(.text .text.*)
        __text_end = .;
    } :text

    .rodata : ALIGN(4K) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        __rodata_end = .;
    } :rodata

    .data : ALIGN(4K) {
        __data_start = .;
        *(.data .data.*)
        *(.got .got.*)
        __data_end = .;
    } :data

    .bss : ALIGN(4K) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4K);
        __bss_end = .;
    } :data

    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
