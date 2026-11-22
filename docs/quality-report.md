# coco quality report — M0 + M1

**Release decision: EXPERIMENTAL — NOT RELEASE READY.**
All gates that apply to the M0/M1 scope pass. The build is not a release
because several mandatory release gates cannot be evaluated yet (no
processes, applications, drivers, shutdown path or hardware testing exist)
and core security features (user/kernel isolation, signed boot) are not
implemented. Nothing in this report claims the system is free of defects.

## Environment

| Item | Version |
|---|---|
| Source revision tested | `0c9c2633f4cb617be91a0c6df037f30eb070eaf7` |
| Rust (shipped artifacts) | `rustc 1.97.0 (2d8144b78 2026-07-07)`, pinned in `rust-toolchain.toml` |
| Rust (Miri, fuzzing only) | `rustc 1.102.0-nightly (bdf712ee8 2026-11-21)`, cargo-fuzz (libFuzzer) |
| QEMU | 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18), TCG — no KVM in this environment |
| Firmware | OVMF 2024.02-2ubuntu0.10 (`OVMF_CODE_4M.fd`, non-Secure-Boot) |
| Image tools | mtools 4.0.43, coreutils sha256sum |
| Host | Ubuntu 24.04.5, x86_64, 4 vCPUs, 15 GiB RAM |

## Results summary

| Gate | Command | Result |
|---|---|---|
| Clean build (both targets) | `cargo clean; cargo xtask check; cargo xtask qemu-test` | ✅ PASS |
| Formatting | `cargo fmt --all --check` | ✅ PASS |
| Clippy, host crates, all targets, `-D warnings` | `cargo clippy --all-targets` | ✅ PASS |
| Clippy, loader (`x86_64-unknown-uefi`) | `cargo clippy -p coco-boot --target …` | ✅ PASS |
| Clippy, kernel (`x86_64-unknown-none`) | `cargo clippy -p coco-kernel --target …` | ✅ PASS |
| Unit + property tests (debug and release) | `cargo test`, `cargo test --release` | ✅ 43/43 each |
| Rustdoc `-D warnings` | `cargo doc --no-deps` | ✅ PASS |
| Miri (UB detection) | `cargo +nightly miri test` on 5 host crates | ✅ 40/40, no UB reported |
| Loom (model checking) | `RUSTFLAGS="--cfg loom" cargo test -p coco-sync` | ✅ 2/2; mutation test detected |
| Fuzzing, 300 s per target | `cargo +nightly fuzz run …` | ✅ 0 crashes in 104.1 M executions |
| QEMU/OVMF boot matrix | `cargo xtask qemu-test` | ✅ 22/22 |
| Reproducible build | `cargo xtask repro` | ✅ 4/4 artifacts bit-identical |
| Base size ≤ 108,000,000 bytes | `cargo xtask build` | ✅ 75,264 bytes |
| Dependency licences | `cargo metadata` review | ✅ all permissive; none in shipped binaries |
| Dependency advisories | — | ⚠️ not run (no audit tool installed); shipped binaries have **zero** third-party crates |

### Host tests (43)
`coco-abi` 15 · `coco-arch-x86_64` 3 · `coco-elf` 8 · `coco-memory` 12 ·
`coco-sync` 2 · `xtask` 3. Property tests use fixed-seed generators:
2,000 random region sets + 5,000 garbage sets (memory-map normalisation),
200,000 mutated ELF images, truncation at every length, 100,000 random
allocator operations against a model, 20,000 random map/unmap operations
against a model, 8 threads × 20,000 lock acquisitions.

### Fuzzing (libFuzzer, 300 s each, run concurrently)
| Target | Executions | Coverage (edges) | Corpus | Crashes |
|---|---:|---:|---:|---:|
| `elf_parse` (seeded with the real kernel) | 26,267,898 | 139 | 301 | 0 |
| `memory_map` | 47,496,108 | 97 | 70 | 0 |
| `boot_info` (header + cmdline) | 30,370,345 | 152 | 703 | 0 |

Each target also asserts invariants on accepted input (W^X, bounds,
window, no shared pages; page conservation; pointers inside the direct map).

### QEMU boot matrix (22 runs)
| Group | Cases | What is verified |
|---|---|---|
| Normal boot | `boot-default` | production config (no cmdline); splash verified by pixel colours via QMP screendump |
| In-kernel memory suite | `selftest-256M` ×3 cold boots, `selftest-4G`, `selftest-smp4-1G`, `selftest-ahci` | 5/5 self-tests each: pattern test, full exhaustion + recovery, 2,000 map/unmap cycles leak check, page-table policy check, #BP resume |
| Fault handling | breakpoint, pagefault, null, guard-page, stack-overflow, nx, write-protect, invalid-opcode, divide, panic, fault-with-console-locked | exact diagnostic text + exit status 35 (controlled stop) |
| Input validation | unknown-test, reject-bad-cmdline, reject-truncated-kernel, reject-missing-kernel | loader/kernel refuse malformed input before kernel entry |

Per-case logs: `target/coco/qemu/<case>/serial.clean.log`;
table: `target/coco/qemu-results.md`.

## Memory findings
* **Leaks:** none detected. In-kernel: allocator free count returns exactly
  to its initial value after full exhaustion (63,717 frames at 256 MiB,
  1,046,661 at 4 GiB) and after 2,000 map/unmap cycles (allocated-frame
  count constant from iteration 1 on; 3 intermediate table frames retained
  by design). Host: model-based tests check every allocation is unique and
  accounted.
* **OOM:** exhaustion returns `FrameError::OutOfMemory` (never panics) and
  the allocator recovers fully (host and kernel).
* **Double free / forged frames:** detected and reported as errors (host).
* **UB:** Miri reports none for all host crates, including the page-table
  mapper's raw-pointer paths over simulated physical memory and the lock
  under real threads.
* **Overflow:** checked arithmetic is enforced by lint in `coco-abi`,
  `coco-elf` and `coco-memory`; release builds keep `overflow-checks`.
* **Stack overflow:** guard page + IST double-fault stack verified.

## Unsafe-code audit
109 `unsafe` blocks total (arch 21, loader 45, kernel 37, memory 2,
sync 4); the parsers of untrusted input (`coco-abi`, `coco-elf`) contain
**none**. Every block has a `SAFETY:` justification and a single unsafe
operation (lint-enforced). Each justification was reviewed against the
invariants in `docs/kernel-safety-invariants.md`. Notable outcomes:
* Loader handoff asm: input operands could have been allocated to `rax`,
  which the sequence clobbers (`mov rax, cr0`). Fixed by pinning inputs to
  `r8`–`r10`.
* FFI calls of the form `((*p).f)(p, …)` were split into a dereference and a
  call with separate justifications; a `File` type now encodes "open until
  closed" and consumes itself on close.

## Concurrency audit
* M1 runs on one CPU with interrupts disabled; NMI and #MC can still
  arrive (IST stacks, non-blocking handlers).
* One lock (`CONSOLE`). Trap/panic paths never block on it (rule L2),
  verified by `fault-with-console-locked`.
* `SpinLock` ordering verified with Loom; a deliberately weakened
  (`Relaxed`) variant fails with "Causality violation: Concurrent write
  accesses to `UnsafeCell`", showing the model detects ordering bugs.
* Not yet applicable: deadlock/priority inversion/starvation/lost wakeups
  (no scheduler or wait queues exist).

## Security findings and mitigations
| Finding | Severity | Status |
|---|---|---|
| Error path used the firmware console after a failed `ExitBootServices` (UEFI spec violation) | low | fixed: reports on COM1 and halts |
| No kernel image signature verification (ESP write access ⇒ arbitrary kernel) | high for a release, inherent to M1 | open, documented; Secure Boot / signed images planned |
| No user/kernel separation, SMEP/SMAP, KASLR | n/a (no user mode yet) | planned M3 |
| Direct map uses WB for all RAM; MMIO not mapped | info | by design |
Verified protections: NX, CR0.WP read-only text/rodata, W^X image layout,
unmapped lower half, stack guard page, strict ELF and boot-info validation.

## Defects found and fixed during this cycle
1. Two host tests never ran because the test files did not compile; once
   fixed, both failed due to **wrong test expectations** (the parser and
   allocator were correct): corrected.
2. IST stacks placed the whole CPU-table struct in `.data`: kernel file
   90,816 → 41,664 bytes.
3. Loader not reproducible (COFF timestamp, PDB path): `/Brepro`,
   `/DEBUG:NONE`.
4. Unchecked arithmetic and multi-op unsafe blocks flagged by lints in
   memory-map normalisation, ELF alignment check, page-table indexing,
   loader FFI: rewritten with checked operations / split blocks.
5. Memory report rounded loader-reclaimed memory to 0 MiB: now KiB.
6. First fuzz run was terminated early by process management (no crashes):
   re-run to completion; the reported numbers are from the complete run.

## Tests not run / not applicable (reported as unverified, not passed)
| Directive item | Why |
|---|---|
| IPC allocation/release, task create/destroy, app open/close | no IPC, tasks or applications exist |
| Long-duration memory growth | kernel halts after boot; no running workload yet |
| Shutdown / reboot lifecycle | no ACPI power management yet; only repeated cold boots (3×) were tested |
| Sanitizers for freestanding kernel code | not available; substituted by in-kernel accounting, guard pages, QEMU fault tests |
| Scheduler/interrupt latency, context switch, UI frame time, storage throughput | components do not exist |
| Physical hardware, Secure Boot firmware | not available in this environment |
| Dependency advisory database scan | no audit tool installed; zero third-party crates ship |

## Performance and size
See `docs/benchmarks.md` (TCG figures only) and `docs/size-report.md`.
Kernel steady-state RAM 268 KiB at 256 MiB guest; idle host CPU 0.3%;
installed base 75,264 bytes (0.07% of the 108,000,000-byte ceiling).

## Remaining risks
Real-hardware behaviour (firmware quirks, missing COM1, GOP formats,
memory-map quirks) is untested; IST stacks lack guard pages; intermediate
page tables are never freed; timings are emulated only.
