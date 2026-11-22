# ADR 0006 — Verification strategy

**Status:** accepted (M0)

| Layer | Tool | Scope |
|---|---|---|
| Lints | rustfmt, clippy `-D warnings` with `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`, `unwrap_used`, `expect_used`, `panic`; `arithmetic_side_effects` + `indexing_slicing` in parser/allocator crates | host crates **and** both bare-metal targets |
| Unit + property tests | `cargo test` (deterministic xorshift generators, model-based checks) | `coco-abi`, `coco-elf`, `coco-memory`, `coco-sync`, `coco-arch` encodings, xtask GPT/CRC |
| UB detection | Miri (nightly) | all host-testable crates incl. the page-table mapper over simulated memory |
| Concurrency | Loom (`--cfg loom`) | `SpinLock` acquire/release protocol; mutation-tested |
| Fuzzing | cargo-fuzz/libFuzzer | ELF parser, memory-map normalisation, boot-info + cmdline validation |
| System | QEMU/OVMF matrix via `cargo xtask qemu-test` | boot, in-kernel memory tests, 10 fault classes, loader rejection of malformed input, framebuffer pixels |
| Supply chain / reproducibility | `cargo xtask repro` | bit-identical loader, kernel, ESP and disk image from two checkouts |

Host results do not prove the freestanding kernel correct; the in-kernel
self-tests and QEMU fault tests cover what the host cannot.
