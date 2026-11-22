# ADR 0008 — Repository layout refinements

**Status:** accepted (M0)

Deviations from the suggested tree, each with a reason:

* `crates/coco-elf` (added): the ELF parser consumes untrusted input and
  must be fuzzable and Miri-checkable on the host, separately from the
  loader binary.
* `crates/coco-sync` (added): synchronization primitives are shared by the
  kernel and must be model-checked with Loom on the host.
* `arch/x86_64` is the package `coco-arch-x86_64` (lib name `coco_arch`).
* `crates/coco-scheduler`, `coco-ipc`, `coco-capability`, `coco-hal`,
  `coco-graphics`, `coco-ui`, `coco-document`, `coco-media`, `services/`,
  `drivers/` and `applications/` are **not created yet**: they would be
  empty. Each appears with the milestone that gives it code and tests.
  The early framebuffer and UART drivers live in the kernel/arch crate
  until the driver framework (M4/M5) exists.
* `fuzz/` is a separate, nightly-only workspace so the main workspace stays
  on the pinned stable toolchain.
