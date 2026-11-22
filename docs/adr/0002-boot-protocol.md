# ADR 0002 — Own UEFI loader and a versioned, validated boot protocol

**Status:** accepted (M1)

## Decision
* `coco-boot` is a native UEFI application (`x86_64-unknown-uefi`, PE32+)
  installed as `\EFI\BOOT\BOOTX64.EFI`. It reads `\coco\kernel.elf` and an
  optional `\coco\cmdline.txt` from its own volume.
* The kernel is a static, non-PIE ELF64 (`x86_64-unknown-none`,
  `relocation-model=static`, `code-model=kernel`) linked at
  `0xFFFFFFFF80000000`. The loader applies no relocations.
* Handoff data is `coco_abi::BootInfo` (`#[repr(C)]`, magic `cocoBOOT`,
  version, header size). Enums cross the boundary as raw integers and are
  decoded with checked conversions. The kernel copies the header, then
  validates every field (`validate_header`, `validate_regions`,
  `validate_cmdline`) before using any pointer in it.
* The loader enters the kernel with interrupts disabled, `rdi` = boot-info
  virtual address, on a guard-paged stack, using `call` so the System V
  stack alignment holds.
* The loader allocates its own memory with OS-reserved UEFI memory types
  (`0x8000C0C0..`) so the final memory map identifies kernel image, stack,
  page tables and boot info exactly.

## Consequences
+ The kernel never trusts loader output blindly; boot-protocol parsing is
  host-tested and fuzzed.
+ Loader and kernel can evolve independently behind `BOOT_INFO_VERSION`.
− Only GOP RGB/BGR 32-bit framebuffers are supported (bitmask/BLT-only
  modes boot serial-only).
