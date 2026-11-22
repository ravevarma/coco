# Known limitations (M0/M1)

* **QEMU only.** Nothing has run on physical hardware. Laptops commonly lack
  COM1; coco then runs framebuffer-only (handled, but untested on hardware).
* **Single CPU, interrupts off.** No timer, no IRQs, no APs. After boot the
  kernel halts (`cli; hlt`): ~0.3% host CPU measured in QEMU.
* **No heap, no processes, no user mode, no syscalls, no IPC** (M2–M4).
* **IST stacks have no guard pages** (they are `.bss` statics).
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
