# Known limitations

* **QEMU only.** Nothing has run on physical hardware. Laptops commonly lack
  COM1; coco then runs framebuffer-only (handled, but untested on hardware).
* **Single CPU.** Application processors stay parked by the firmware.
* **Periodic 100 Hz tick**, not tickless: idle costs ~1.3% host CPU in QEMU
  (0.3% when halted with interrupts off in M1).
* **Processes are single-threaded**, can only be started by the kernel (no
  spawn/exec syscalls), and only the one `init.elf` image exists (no
  filesystem). No device interrupts: the I/O APIC is not programmed yet.
* **IPC is channels only**: no shared-memory objects, no service registry
  (services get their endpoints from the kernel at spawn), no per-process
  quotas on channels (one global limit of 4,096). Round trips are slow
  under TCG (≈87–104 k cycles) with no fast path.
* **Sends that would create a reference cycle are refused**, which some
  designs may find restrictive (e.g. two services exchanging each other's
  endpoints through their own channels).
* **The heap keeps pages it grows into**; lifecycle tests therefore treat
  `free + heap-held` frames as the leak invariant.
* **Scheduling is single-queue round-robin** with no priorities, budgets or
  CPU accounting; a CPU-bound process gets its fair share but is never
  limited.
* **~1 MiB of RAM** is spent pre-populating all 256 kernel PML4 slots so
  process address spaces share kernel mappings.
* **No KPTI / speculative-execution mitigations.** The kernel half is
  mapped (supervisor-only) in every process. On CPUs affected by Meltdown
  a user program could read kernel memory speculatively; Spectre-class
  mitigations are not implemented either.
* **AVX and FSGSBASE are disabled** for user programs (only x87/SSE state
  is saved per thread).
* **Heap memory is never returned** to the frame allocator, and small-class
  pages are never reused for other classes (bounded by peak use).
* **x2APIC and HPET-fallback code paths are unverified** (QEMU 8.2 TCG lacks
  x2APIC; q35 always offers the PM timer).
* **`_S5` is found by pattern match**, not AML interpretation; firmware that
  computes `_S5` in a method cannot power off.
* **ACPI tables with bad checksums are rejected**, which may disable
  features on firmware with broken tables.
* **Calibration busy-waits 50 ms** at boot.
* **Intermediate page tables are never freed** after unmapping; a 2 MiB
  mapping cannot replace a now-empty 4 KiB table (`SizeConflict`).
* **Direct map caching:** all direct-mapped memory uses the default PAT
  entry (WB); the framebuffer relies on firmware MTRRs. No write-combining.
* **Frames below 1 MiB are never allocated** (deliberate).
* **Memory maps with overlapping descriptors are rejected** (boot halts with
  a message on COM1) rather than repaired.
* **Framebuffer:** 32-bit RGB/BGR GOP modes only; resolution is whatever the
  firmware selected; text console is ASCII with an original 5×7 font, no
  Unicode.
* **No Secure Boot, no image signatures**, unsigned loader.
* **Timing figures are from TCG emulation** and are not hardware boot times.
* **Fuzzing and Miri run on nightly** (`rustup run nightly`), separate from the
  pinned stable toolchain used for shipped artifacts.
* **No dependency-advisory scan tool** is wired in; the shipped binaries have
  no third-party dependencies, which is the current mitigation.

## Other architectures (ADR 0015)
* **Bring-up only.** RISC-V 64/32, AArch64 and 32-bit x86 run the portable
  kernel with the **MMU off** (physical = virtual, no W^X enforcement, no
  isolation): no paging, user mode, processes, scheduler or syscalls yet.
* **RISC-V 32 runs in M-mode** without firmware; no RV32 SBI firmware is
  available in this build environment.
* **Single core** on every port: secondary harts/cores are parked (AArch64)
  or never started.
* **QEMU `virt`/`pc` only**: device addresses fall back to QEMU's when the
  device tree lacks the UART; the GIC and CLINT addresses are fixed to
  QEMU's layout (not read from the device tree yet).
* **AArch64 failures exit QEMU with status 0** (PSCI reset under
  `-no-reboot`); the harness detects them from the serial log.
* **32-bit x86 uses the hosted `i686-unknown-linux-gnu` target**
  freestanding, with its own `mem*` routines; physical memory above 4 GiB
  is ignored (no PAE yet).
* **At most 16 RAM and 16 reserved ranges** are taken from a device tree
  (extra ones are counted and dropped).
