# ADR 0001 — Independent kernel written in Rust

**Status:** accepted (M0)

## Context
coco must be a standalone OS: its own loader, kernel, ABI and userspace. No
existing kernel (Linux, BSD, XNU, Windows, Redox, …) may serve as the
foundation. Memory safety is a primary goal.

## Decision
* Every component that runs on the target is written from scratch in Rust
  (edition 2024, toolchain pinned in `rust-toolchain.toml`).
* The loader and kernel have **no third-party runtime dependencies**; UEFI
  bindings are hand-written (`boot/src/uefi.rs`) for the handful of
  services coco uses. Third-party crates are allowed only in test/fuzz
  builds (`loom`, `libfuzzer-sys`) where they never reach an artifact.
* Public design literature (Intel SDM, UEFI 2.10, System V ABI, ELF spec)
  is the reference; no code is taken from other kernels.

## Consequences
+ The trusted computing base is entirely in this repository and reviewable.
+ No supply-chain exposure for shipped artifacts.
− More code to own (e.g. UEFI bindings, ELF parsing); mitigated by keeping
  each piece small and host-testable.
