# Kernel safety invariants

Every `unsafe` block in coco carries a `// SAFETY:` justification; this is
enforced by `clippy::undocumented_unsafe_blocks = deny` and each block may
contain only one unsafe operation (`clippy::multiple_unsafe_ops_per_block =
deny`). `unsafe_op_in_unsafe_fn` is denied, so unsafe functions also need
explicit blocks. This document lists the invariants those justifications
rely on.

## Inventory (M4, revision 99e8620)

| Crate | `unsafe {}` blocks | `unsafe fn` | `unsafe impl` | Purpose |
|---|---:|---:|---:|---|
| `coco-arch-x86_64` | 31 | 17 | 0 | privileged instructions, port I/O, UART, CR4, interrupt flag, FXSAVE |
| `coco-boot` | 46 | 1 | 1 | UEFI FFI, page-table construction, handoff, init image copy |
| `coco-kernel` | 119 | 1 | 6 | descriptor tables, APIC/HPET/PM-timer/CMOS/PIC I/O, framebuffer, boot-info read, allocator glue, context switch, ring-3 entry, syscall entry and user copies, self-tests |
| `coco-init` (user) | 6 | 0 | 0 | `syscall`/`rdtsc` instructions, deliberate faults for isolation tests |
| `coco-memory` | 21 | 4 | 1 (+1 unsafe trait) | page-table entries, frame tokens, heap free lists |
| `coco-sync` | 4 | 0 | 2 | lock guard dereference |
| `coco-abi`, `coco-acpi`, `coco-capability`, `coco-elf`, `coco-ipc` | **0** | 0 | 0 | parsers, capability tables and channel state are 100% safe Rust |

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
* **C4 — CPL 0 only.** `coco_arch::cpu` register accesses run only in the
  kernel; user code reaches the kernel only through SYSCALL and exceptions.
* **C5 — Per-CPU block / TSS.RSP0.** Written only during a context switch
  (interrupts disabled); read by the syscall stub with IF cleared by FMASK.
  C1 is amended accordingly: RSP0 is the one TSS field written after init.
* **C6 — GS.** The kernel never uses GS; `swapgs` is executed only around
  the two per-CPU accesses in the syscall stub, so GS is always the user
  value while kernel code runs (including in interrupt handlers).

## Locks and interrupt context

* **L1 — IRQ safety.** Kernel-global locks are `IrqSpinLock`s: interrupts
  are disabled while held and restored afterwards, so an interrupt can never
  spin on a lock held by the code it interrupted.
* **L2 — Trap paths never block.** Exception and panic handlers use
  `try_lock` and fall back to the lock-free UART path
  (`SerialPort::assume_init`). Verified by the `fault-with-console-locked`
  QEMU test.
* **L3 — Lock order.** `PROCESSES` → `CHANNEL`; `GRAPH` → `CHANNEL`;
  `CHANNEL` → `GRAVEYARD` → `SCHED` → `HEAP` → `MEMORY` → `CONSOLE`.
  `PROCESSES` and `GRAPH` are never held together. The heap grows by taking
  `MEMORY`; code holding `MEMORY` never allocates (address-space teardown
  frees frames from callbacks for this reason); the timer path takes only
  `SCHED` and never allocates.
* **L5 — No lock across a context switch.** `switch_away` computes raw
  pointers to the old/new thread records under `SCHED`, releases it, then
  switches with interrupts disabled; nothing else can run (single CPU)
  until the switch completes, so the pointers stay valid.

## Threads, processes and user memory

* **T1 — Stacks and reaping.** Each thread stack is owned by exactly one
  thread record. A dead thread is claimed by exactly one reaper (removed
  from the dead list and marked `Reaping` under `SCHED`); the running
  thread is never reaped. The record is removed from the table and dropped
  inside one `SCHED` critical section after its stack is freed, so a thread
  is visible to `state`/`join` until all of its resources are released.
* **T2 — Fresh threads** start from a prepared frame (`coco_thread_start`)
  with interrupts disabled; kernel bodies enable them first; user threads
  enter ring 3 via `iretq` with IF set and all other GPRs zeroed.
* **P1 — Address spaces.** A process PML4's user half is touched only by
  `map_user_page`, `translate_in` and `teardown_user` under `MEMORY`; it is
  torn down only after the process's thread is dead (so it is not live).
* **U1 — User memory.** The kernel reads user memory only through
  `copy_from_user`: range check (`validate_user_range`), present+USER
  check of every page in the caller's tables, copy inside `stac`/`clac`.
  Processes are single-threaded, so the checked mappings cannot change
  during the copy.
* **U2 — Return to user.** Every caller-saved register is restored from the
  syscall frame; RCX must be below `USER_TOP` or the process is killed.
* **I1 — Capabilities.** Kernel objects are reached from user space only
  through the caller's handle table; every syscall checks the generation
  and the rights it needs. Rights never grow (duplicate = subset).
* **I2 — Channel references.** Each side's open count equals the number of
  `ChannelRef`s to it (in tables, in flight, or held by the kernel).
  Messages drained from a closed side are dropped outside channel locks,
  through the graveyard worklist.
* **I3 — Acyclic in-flight graph.** A send carrying endpoints is checked and
  enqueued under `GRAPH`; it is refused if the endpoint's channel already
  reaches the destination channel. Hence no channel can keep itself alive.
  The reachability search runs with interrupts disabled, so it has a fixed
  budget (`REACH_BUDGET` = 4,096 node + edge examinations) and only
  fallible allocations: if the answer cannot be computed, the send is
  refused with `NoResources` — a cycle is never admitted.
* **I4 — Process exit.** A finished or killed process's handle table is
  drained before its thread switches away; objects are dropped outside
  `PROCESSES`.
* **U3 — Fault containment.** Exceptions with CPL 3 in the saved CS kill the
  current process; exceptions at CPL 0 remain fatal kernel errors.
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

## Portable bring-up kernel (`ports/coco-port`, ADR 0015)

* **R1 — Physical = virtual.** The MMU is off, so every pointer is a
  physical address. Raw memory is touched only in (a) firmware-reported
  usable RAM minus the kernel image, the device tree / Multiboot
  structures and firmware reservations (`usable_ranges`), (b) device
  registers of the platform (UART, GIC, CLINT, test/exit devices, legacy
  x86 ports). The bitmaps and the 1 MiB heap are carved out of (a) and
  never handed to the frame allocator.
* **R2 — Boot blobs.** Device trees are trusted only after the magic and a
  bounded declared size (40 B – 2 MiB) are checked; all further parsing is
  safe code in `coco-fdt`. Multiboot memory-map walking stops on an entry
  size below 20 or on overflow.
* **R3 — Traps.** Every vector saves all general registers the compiled
  code may use: 31 GPRs + PC on RISC-V and AArch64; `pushal` plus FXSAVE
  of SSE state on 32-bit x86. The FPU is off on RISC-V 64 and absent from
  the AArch64 soft-float and RV32IMAC targets, so no unsaved FP state
  exists. Handlers never allocate; the heap lock is therefore never
  re-entered from a trap.
* **R4 — Unexpected traps are fatal.** Anything other than the breakpoint
  and timer (and spurious PIC IRQs) prints a diagnostic and stops the
  machine; nothing resumes after an unknown fault (negative test
  `inject-fault` on every port).
* **R5 — 32-bit pointers.** RAM above the addressable limit (4 GiB on 32-bit
  CPUs) is clamped away, never truncated into a low address.
