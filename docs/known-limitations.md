# Known limitations (M0/M1)

* **QEMU only.** Nothing has run on physical hardware. Laptops commonly lack
  COM1; coco then runs framebuffer-only (handled, but untested on hardware).
* **Single CPU.** Application processors stay parked by the firmware.
* **Periodic 100 Hz tick**, not tickless: idle costs ~1.3% host CPU in QEMU
  (0.3% when halted with interrupts off in M1).
* **Processes are single-threaded**, can only be started by the kernel (no
  spawn/exec syscalls), and only the one `init.elf` image exists (no
  filesystem). No IPC or capabilities yet (M4). No device interrupts: the
  I/O APIC is not programmed yet.
* **Scheduling is single-queue round-robin** with no priorities, budgets or
  CPU accounting; a CPU-bound process gets its fair share but is never
  limited.
* **~1 MiB of RAM** is spent pre-populating all 256 kernel PML4 slots so
  process address spaces share kernel mappings.
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
