/* coco user program: linked at USER_BASE (4 MiB), one PT_LOAD per
 * permission class (W^X), page-aligned sections. */
ENTRY(_start)
PHDRS { text PT_LOAD FLAGS(5); rodata PT_LOAD FLAGS(4); data PT_LOAD FLAGS(6); }
SECTIONS
{
    . = 0x400000;
    .text : ALIGN(4096) { *(.text .text.*) } :text
    . = ALIGN(4096);
    .rodata : ALIGN(4096) { *(.rodata .rodata.*) } :rodata
    . = ALIGN(4096);
    .data : ALIGN(4096) { *(.data .data.*) *(.got .got.*) } :data
    .bss : ALIGN(16) { *(.bss .bss.*) *(COMMON) } :data
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) *(.comment) *(.note .note.*) }
}
