/* coco kernel linker script.
 * One PT_LOAD per permission class so the loader can enforce W^X:
 * text = R+X, rodata = R, data/bss = R+W. Sections are page aligned so no
 * two permission classes share a page. */
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0xFFFFFFFF80000000;
    __kernel_start = .;

    .text : ALIGN(4096) {
        *(.text.entry)
        *(.text .text.*)
    } :text

    . = ALIGN(4096);
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4096);
    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.got .got.*)
    } :data

    .bss : ALIGN(16) {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
