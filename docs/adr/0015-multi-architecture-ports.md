# ADR 0015 — Multi-architecture support: portable core + bring-up ports

**Status:** accepted (post-M4)

## Context
coco targets x86_64 UEFI laptops first, but the project goal now includes
32-bit x86, AArch64, and 32- and 64-bit RISC-V. The x86_64 kernel
(`kernel/`) depends on x86_64 paging, SYSCALL/SYSRET, the APIC and UEFI.
Rewriting it for four more architectures at once would produce four
untested kernels. A layered approach keeps every step testable.

## Decision
1. **Portable core crates** carry all architecture-independent logic and
   must build and pass their tests on every architecture: `coco-abi`,
   `coco-acpi`, `coco-capability`, `coco-elf`, `coco-fdt`, `coco-ipc`,
   `coco-memory`, `coco-sync`. They contain no inline assembly, and sizes
   and indices go through checked conversions, never `as` truncation
   (`words_for` returns `None` on 32-bit instead of truncating).
   `cargo xtask cross-test` runs their tests on i686 (native), AArch64 and
   RISC-V 64 (QEMU user mode) and builds them for six bare-metal targets.
2. **One portable bring-up kernel** (`ports/coco-port`) runs the same
   kernel code on four backends (`src/arch/`). Each backend supplies only:
   boot entry, console, trap vector, periodic timer, a free-running
   counter, firmware memory discovery and machine exit.

   | Port | Target | Mode | Firmware / boot | Memory map | Timer | Console |
   |---|---|---|---|---|---|---|
   | RISC-V 64 | `riscv64gc-unknown-none-elf` | S-mode | OpenSBI (`-bios default`) | device tree (`a1`) | SBI TIME + `rdtime` | ns16550a |
   | RISC-V 32 | `riscv32imac-unknown-none-elf` | M-mode | none (`-bios none`) | device tree (`a1`) | CLINT `mtime`/`mtimecmp` | ns16550a |
   | AArch64 | `aarch64-unknown-none-softfloat` | EL1 | QEMU direct boot | device tree (`x0` or start of RAM) | generic timer via GICv2 | PL011 |
   | x86 (32-bit) | `i686-unknown-linux-gnu` (freestanding) | ring 0, protected mode | Multiboot v1 | Multiboot memory map | 8259 PIC + PIT; TSC calibrated on PIT ch. 2 | COM1 |

3. **`coco-fdt`**: our own flattened device-tree reader (no `unsafe`, no
   allocation, bounded depth and range counts), fuzzed and tested on real
   QEMU blobs. It replaces ACPI on RISC-V and AArch64.
4. **Verification before features:** `cargo xtask port-test` boots every
   port and also runs two negative builds per port (an injected failing
   check and an injected illegal instruction), which must be reported as
   failures, so a pass is never vacuous.

## Consequences
* x86_64 stays the only full kernel (paging, processes, user mode,
  syscalls). The ports run with the **MMU off** (physical = virtual).
  Per-architecture paging, user mode and the scheduler are the next port
  milestones. The shared parts (frame allocator, heap, capabilities, IPC)
  already run unchanged on all five architectures.
* RISC-V 32 runs in M-mode because no RV32 SBI firmware is available in
  the build environment (GitHub downloads are blocked). An S-mode RV32
  port needs our own minimal SBI or a firmware package.
* The 32-bit x86 port uses the hosted `i686-unknown-linux-gnu` target
  freestanding (`panic=abort`, static, own linker script, own
  `memcpy`/`memset`/`memmove`/`memcmp`) because stable Rust has no
  `i686-unknown-none` target and custom target JSON needs nightly. Its
  trap stubs save SSE state (FXSAVE), because Rust code on that target
  may use XMM registers.
* On RISC-V 64 the floating-point unit is switched **off** at entry
  (`sstatus.FS = Off`), so any compiler-generated FP instruction traps
  visibly instead of silently corrupting state that the trap handler does
  not save (the passing self-tests show none is executed). RV32IMAC has no
  FPU. AArch64 uses the soft-float target for the same reason.
* AArch64 failure is signalled by PSCI SYSTEM_RESET; under `-no-reboot`
  QEMU exits with status 0 either way, so the harness judges AArch64
  failures by the serial log alone.
