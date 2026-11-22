# Kernel safety invariants

Every `unsafe` block in coco carries a `// SAFETY:` justification; this is
enforced by `clippy::undocumented_unsafe_blocks = deny` and each block may
contain only one unsafe operation (`clippy::multiple_unsafe_ops_per_block =
deny`). `unsafe_op_in_unsafe_fn` is denied, so unsafe functions also need
explicit blocks. This document lists the invariants those justifications
rely on.

## Inventory (M2, revision b60418a)

| Crate | `unsafe {}` blocks | `unsafe fn` | `unsafe impl` | Purpose |
|---|---:|---:|---:|---|
| `coco-arch-x86_64` | 28 | 15 | 0 | privileged instructions, port I/O, UART, CR4, interrupt flag |
| `coco-boot` | 45 | 1 | 1 | UEFI FFI, page-table construction, handoff |
| `coco-kernel` | 77 | 1 | 5 | descriptor tables, APIC/HPET/PM-timer/CMOS/PIC I/O, framebuffer, boot-info read, allocator glue, self-tests |
| `coco-memory` | 21 | 4 | 1 (+1 unsafe trait) | page-table entries, frame tokens, heap free lists |
| `coco-sync` | 4 | 0 | 2 | lock guard dereference |
| `coco-abi`, `coco-acpi`, `coco-elf` | **0** | 0 | 0 | untrusted-input parsers are 100% safe Rust |

## Memory

* **M1 — Direct map.** For every physical address of RAM-like memory
  (usable, loader, kernel, page tables, boot info, ACPI, firmware runtime)
  and the framebuffer, `DIRECT_MAP_BASE + phys` is mapped RW+NX. MMIO,
  firmware-reserved and bad RAM are not mapped. (`DirectMap: PhysAccess`.)
* **M2 — Page-table ownership.** Page tables are reached only through a
  `coco_memory::paging::Mapper`, which is created with `unsafe Mapper::new`
  whose caller guarantees exclusive control. At most one mapper over the
  live CR3 exists at a time (`drop_identity_map` drops its temporary mapper
  before `mm::init` creates the long-lived one).
* **M3 — TLB.** Mapping changes on live tables return a `#[must_use]
  Flush` token; the kernel `invlpg`s each one. Whole-space changes reload
  CR3.
* **M4 — Frame ownership.** A frame is owned by exactly one of: the
  allocator (free bit set), a `PhysFrame` token, a page-table entry (after
  `into_raw`), or a self-test list. `PhysFrame::from_raw` is `unsafe` and
  only re-wraps a frame obtained from `into_raw`/`unmap` once.
* **M5 — Allocator storage.** The bitmap pages are Usable RAM carved out
  before any free range is added and never handed out; they live for the
  kernel's lifetime (`'static`).
* **M6 — Boot info.** `BOOT_INFO` memory (header, region array, cmdline) is
  never reclaimed or written by the kernel, which is what makes the
  `&'static` slices into it sound.
* **M7 — Stacks.** The boot stack and each IST stack (#DF, NMI, #MC;
  16 KiB each, `.bss`) have an unmapped guard page directly below them
  (IST guards since M2).
* **M8 — W^X.** No mapping is both writable and executable once the
  kernel has dropped the identity alias.
* **M9 — MMIO.** Device registers are mapped uncached (PCD|PWT) and NX in
  the direct map; `map_mmio` refuses a page already covered by a cached
  mapping, so RAM and MMIO never alias with conflicting cache types.
* **M10 — Heap.** The heap window (PML4 507) is used only by the heap;
  every byte in its carved range belongs to exactly one live allocation,
  free block or free run (see `coco_memory::heap`). Backing frames are
  zeroed before mapping. The `GlobalAlloc` impl relies on the host-tested
  guarantee that returned blocks are disjoint, aligned and large enough.

## CPU state

* **C1 — Descriptor tables** (`cpu.rs` `TablesCell`): mutated only inside
  `cpu::init`, which runs once (guarded by an atomic flag) on the boot CPU
  with interrupts disabled; afterwards Rust never forms references to the
  tables (the CPU writes the TSS busy bit). The IRQ stub layout (16-byte
  stride) is verified before the IDT is loaded.
* **C2 — IST stacks** (`StacksCell`): Rust only computes their addresses.
* **C3 — Interrupts** are enabled only after the IDT covers all 256
  vectors, the PICs are masked and the local APIC is configured. Handlers
  (vectors ≥ 32) run with `IN_INTERRUPT` raised, take no locks and never
  allocate (the allocator refuses and reports). Only the APIC timer is
  unmasked.
* **C4 — CPL 0 only.** All `coco_arch::cpu` register reads assume ring 0;
  there is no user mode yet.

## Locks and interrupt context

* **L1 — IRQ safety.** Kernel-global locks are `IrqSpinLock`s: interrupts
  are disabled while held and restored afterwards, so an interrupt can never
  spin on a lock held by the code it interrupted.
* **L2 — Trap paths never block.** Exception and panic handlers use
  `try_lock` and fall back to the lock-free UART path
  (`SerialPort::assume_init`). Verified by the `fault-with-console-locked`
  QEMU test.
* **L3 — Lock order.** `HEAP` → `MEMORY` → `CONSOLE`. The heap grows by
  taking `MEMORY`; code holding `MEMORY` never allocates; printing under
  either is allowed. New locks must be added here.
* **L4 — Ordering.** `SpinLock` acquires with `Acquire` and releases with
  `Release`; Loom verifies the protocol and detects a weakened variant
  (mutation test recorded in `docs/quality-report.md`).

## Firmware boundary (loader)

* **F1** — Boot-services pointers (`SystemTable`, `BootServices`, protocol
  instances) are validated (non-null, table signatures) and used only while
  the `Firmware` value exists; `exit_boot_services(self)` consumes it, so no
  firmware call can be made afterwards (typestate).
* **F2** — UEFI identity-maps all memory while boot services run; the
  loader's `Identity: PhysAccess` relies on this and only touches pages it
  allocated with its own memory types.
* **F3** — Memory descriptors are read with `read_unaligned` at the
  firmware-reported stride (`desc_size >= 40` is checked).
