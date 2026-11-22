/* Shared section layout: one PT_LOAD per permission class (W^X). */
PHDRS { text PT_LOAD FLAGS(5); rodata PT_LOAD FLAGS(4); data PT_LOAD FLAGS(6); }
SECTIONS
{
    . = KERNEL_BASE;
    __kernel_start = .;
    .text : ALIGN(4096) {
        KEEP(*(.multiboot))
        *(.text.entry)
        *(.text .text.*)
    } :text
    . = ALIGN(4096);
    .rodata : ALIGN(4096) { *(.rodata .rodata.* .srodata .srodata.*) } :rodata
    . = ALIGN(4096);
    .data : ALIGN(4096) { *(.data .data.* .sdata .sdata.* .got .got.*) } :data
    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.* COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } :data
    . = ALIGN(4096);
    . += 0x10000;           /* 64 KiB boot stack */
    __stack_top = .;
    __kernel_end = .;
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr .comment .note .note.* .riscv.attributes) }
}
