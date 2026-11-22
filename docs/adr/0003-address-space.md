# ADR 0003 — Kernel address-space layout and memory protection

**Status:** accepted (M1)

| Range | Use | Permissions |
|---|---|---|
| `0x0000_0000_0000_0000` – lower half | future user space; identity alias during handoff only | removed by the kernel at boot |
| `0xFFFF_8000_0000_0000` (PML4 256–319) | direct map of RAM + framebuffer, 2 MiB pages where aligned, 4 KiB at edges | RW, **NX at PML4 level** |
| `0xFFFF_FD00_0000_0000` (PML4 506) | self-test scratch | — |
| `0xFFFF_FE00_0000_0000` (PML4 508) | kernel stacks; guard page at the base, boot stack above it | RW, NX; guard unmapped |
| `0xFFFF_FFFF_8000_0000` (PML4 511) | kernel image | text R+X, rodata R+NX, data/bss RW+NX |

## Decisions
* **W^X** at three layers: the linker script emits one `PT_LOAD` per
  permission class; `coco-elf` rejects any W+X segment or page sharing; the
  loader maps each segment with exactly its permissions. EFER.NXE and
  CR0.WP are enabled before entering the kernel.
* The temporary identity alias shares the direct map's level-3 tables (no
  extra frames) and is executable only so the instruction after `mov cr3`
  resolves. The kernel clears PML4 0–255 right after loading its own GDT/IDT.
* MMIO and firmware-reserved ranges are **not** direct-mapped; drivers will
  map them with correct cache attributes later (ADR to follow with M5).
* Self-tests verify the live page tables against this policy and fault
  tests prove NX, WP, guard page and null-page protection on real CPU
  execution (QEMU).
