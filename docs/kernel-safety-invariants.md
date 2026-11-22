# Kernel safety invariants

Every `unsafe` block in coco carries a `// SAFETY:` justification; this is
enforced by `clippy::undocumented_unsafe_blocks = deny` and each block may
contain only one unsafe operation (`clippy::multiple_unsafe_ops_per_block =
deny`). `unsafe_op_in_unsafe_fn` is denied, so unsafe functions also need
explicit blocks. This document lists the invariants those justifications
rely on.

## Inventory (M1)

| Crate | `unsafe {}` blocks | `unsafe fn` | `unsafe impl` | Purpose |
|---|---:|---:|---:|---|
| `coco-arch-x86_64` | 21 | 9 | 0 | privileged instructions, port I/O, UART |
| `coco-boot` | 45 | 1 | 1 | UEFI FFI, page-table construction, handoff |
| `coco-kernel` | 37 | 1 | 4 | descriptor tables, framebuffer, boot-info read, self-tests |
| `coco-memory` | 2 | 3 | 0 (1 unsafe trait) | page-table entry read/write, frame token re-wrap |
| `coco-sync` | 4 | 0 | 2 | lock guard dereference |
| `coco-abi`, `coco-elf` | **0** | 0 | 0 | untrusted-input parsers are 100% safe Rust |

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
* **M7 — Stacks.** The boot stack has an unmapped guard page directly below
  it; IST stacks (#DF, NMI, #MC; 16 KiB each, `.bss`) have no guard pages
  yet (known limitation).
* **M8 — W^X.** No mapping is both writable and executable once the
  kernel has dropped the identity alias.

## CPU state

* **C1 — Descriptor tables** (`cpu.rs` `TablesCell`): mutated only inside
  `cpu::init`, which runs once (guarded by an atomic flag) on the boot CPU
  with interrupts disabled; afterwards Rust never forms references to the
  tables (the CPU writes the TSS busy bit).
* **C2 — IST stacks** (`StacksCell`): Rust only computes their addresses.
* **C3 — Interrupts** stay disabled for the whole of M1 (no IRQ sources are
  configured). NMI and #MC can still arrive and run on IST stacks.
* **C4 — CPL 0 only.** All `coco_arch::cpu` register reads assume ring 0;
  there is no user mode yet.

## Locks and interrupt context

* **L1 — IRQ safety.** A `SpinLock` that may be taken in an interrupt
  handler must only be acquired with interrupts disabled. (M1 has no IRQ
  handlers; the rule takes effect with the M2 timer.)
* **L2 — Trap paths never block.** Exception and panic handlers use
  `try_lock` and fall back to the lock-free UART path
  (`SerialPort::assume_init`). Verified by the `fault-with-console-locked`
  QEMU test.
* **L3 — Lock order.** Only one lock exists (`CONSOLE`). New locks must be
  added to an explicit order in this section.
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
