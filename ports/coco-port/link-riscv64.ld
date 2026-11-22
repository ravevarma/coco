OUTPUT_ARCH(riscv)
ENTRY(_start)
KERNEL_BASE = 0x80200000;
INCLUDE link-common.ld
