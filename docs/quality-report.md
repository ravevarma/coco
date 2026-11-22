# coco quality report — post-M4 audit and multi-architecture ports

**Release decision: EXPERIMENTAL — NOT RELEASE READY.**
Every gate that applies to the implemented scope passes on the revision
this report was committed with. coco is not a release. It has no
filesystem, driver set, service registry, signed boot or hardware
validation. The new RISC-V, AArch64 and 32-bit x86 ports are bring-up
kernels running with the MMU off. Nothing in this report claims the system
is free of defects.

Earlier reports: M0/M1 `git show 2f2a915:docs/quality-report.md`,
M2 `git show 778ece7:docs/quality-report.md`,
M3 and the post-M3 hardening pass `git show c2cbb15:docs/quality-report.md`,
M4 `git show 8f6c0cd:docs/quality-report.md`.

## Scope of this round
1. Audit of all existing code for correctness on 32-bit and non-x86
   architectures, memory leaks, unbounded work, and performance (measured
   before any optimisation).
2. Portable bring-up kernel for **RISC-V 64, RISC-V 32, AArch64 and
   32-bit x86** (ADR 0015), plus the `coco-fdt` device-tree parser.

## Environment

| Item | Version |
|---|---|
| Rust (shipped artifacts) | `rustc 1.97.0 (2d8144b78 2026-07-07)`, pinned |
| Rust (Miri, fuzzing, sanitizers) | `rustc 1.102.0-nightly (bdf712ee8 2026-11-21)`, cargo-fuzz |
| QEMU / firmware | QEMU 8.2.2 TCG (no KVM): `qemu-system-{x86_64,i386,riscv64,riscv32,aarch64}`, `qemu-{aarch64,riscv64}` user mode; OVMF 2024.02; OpenSBI bundled with QEMU (RISC-V 64 only) |

## Results summary

| Gate | Result |
|---|---|
| rustfmt; clippy `-D warnings` (host crates, loader, kernel, user program, **`coco-port` on 4 targets**); rustdoc | ✅ 12/12 steps (`cargo xtask check`) |
| Host unit + property tests (debug and release) | ✅ 89/89 each |
| **Cross-architecture tests** (`cargo xtask cross-test`) | ✅ 83 portable tests each on i686 (32-bit), AArch64 and RISC-V 64 = 249; portable crates build for 6 bare-metal targets |
| **Port boots** (`cargo xtask port-test`) | ✅ 12/12: 4 ports × (self-tests + 2 negative cases that must fail) |
| x86_64 QEMU/OVMF matrix | ✅ **36/36 runs** (incl. `bench-ipc`), re-run after the final kernel change: IPC rounds end with 0 live channels and exact frame/heap baselines |
| Miri | ✅ `coco-fdt` 6 tests (new, strict provenance); other crates unchanged since their last Miri run (M4 report) except `coco-memory` (`words_for`, re-run in the audit) |
| AddressSanitizer / ThreadSanitizer (nightly, host) | ✅ ASan: 76 tests, no reports; TSan: 18 tests, no races (audit run) |
| Fuzzing (0 crashes) | `fdt_parse` (new): 44.7 M runs unseeded + 6.2 M runs seeded with real QEMU device trees (coverage 166 → 331 edges); earlier targets unchanged |
| Reproducible build | ✅ loader, kernel, ESP, disk image **and all four port images** bit-identical from two different paths |
| Base size ≤ 108,000,000 B | ✅ 212,480 B (kernel 160,872 B: the audit's additions are offset by removing `BTreeSet` from the IPC cycle check) |
| Third-party code in shipped binaries | ✅ none |

## Defects found and fixed in this round

1. **Frame-bitmap size truncation on 32-bit targets (real bug, audit):**
   `words_for(frames) as usize` truncated on 32-bit CPUs for very large
   frame counts, so undersized bitmap storage could be accepted and
   out-of-range frames silently skipped.
   `words_for` now returns `Option<usize>`, and every index conversion is
   checked. A regression test asserts the exact result on 64-bit and a
   refusal on 32-bit.
2. **`sleep` could return early (audit):** wake-ups are shared with channel
   waits, so a sleeping thread woken by IPC returned before its deadline.
   It now re-blocks until the deadline.
3. **Unbounded work with interrupts disabled (audit):** the IPC cycle check
   walked the whole in-flight graph. It now has a fixed budget, and sends
   it cannot decide are refused (`NoResources`) — never admitted.
4. **Infallible allocation in that check (found while writing this
   report):** the search still used `collect`, `extend` and
   `BTreeSet::insert`. Heap exhaustion during a send would therefore have
   panicked the kernel with interrupts off. It now allocates only with
   `try_reserve` and needs no visited set (the graph is acyclic and the
   budget bounds revisits).
5. **x86_64-only code in a portable crate:** `coco-arch-x86_64`'s
   instruction modules broke 32-bit builds; they are now gated on
   `target_arch`.
6. **Port bring-up issues (caught before commit):** the i686 target's
   `compiler_builtins` leaves `memcpy`/`memset` to libc (we now provide
   them with string instructions); LLVM reserves `esi` (it is swapped
   around `rep movsb`); x86 trap stubs now FXSAVE SSE state; the RISC-V 64
   FPU is switched off explicitly so FP use traps; clippy findings fixed on
   all four targets.
7. **Wasted size:** lld's default 64 KiB AArch64 segment alignment padded
   the AArch64 image by about 61 KB. Linking with `max-page-size=4096`
   brought it from 103,008 to 41,568 B.

## Performance work (measured, not assumed)
* Null syscall ≈ 0.3–0.6 k cycles; kernel-thread switch ≈ 4.2 k; process
  yield ≈ 36.5–39.9 k; IPC round trip ≈ 94–104 k cycles (TCG; the final
  matrix run measured 526 / 39.7 k / 100–104 k, i.e. no regression from
  fix 4). Process-switch
  cost is dominated by CR3 reloads that flush TCG's software TLB.
* A combined send+receive syscall (`CHANNEL_CALL`) showed no reproducible
  gain and was **removed** rather than kept as unmeasured complexity.
* PCID and global kernel pages cannot be measured under TCG; they are
  deferred until hardware or KVM measurements are possible.

## Unsafe-code audit
The ports add 68 `unsafe` blocks (portable core 9, RISC-V 25, AArch64 16,
x86 18), each with a `SAFETY` comment (enforced by
`clippy::undocumented_unsafe_blocks`). They cover device registers, trap
and entry assembly, and memory reported by firmware. Invariants R1–R5 are
in `docs/kernel-safety-invariants.md`. `coco-fdt` contains no `unsafe`.

## Not run / unverified
| Item | Reason |
|---|---|
| Ports with MMU/paging, user mode, scheduler, SMP | not implemented yet (next port milestones) |
| RISC-V 32 in S-mode; GICv3; real boards (SiFive, Raspberry Pi, any PC) | no firmware / hardware available |
| AArch64 failure exit code | PSCI reset exits QEMU with 0; failure is detected from the serial log only |
| x2APIC register path; HPET fallback (x86_64) | not available under QEMU 8.2 TCG / q35 |
| SMP correctness of wait/wake and locking | single CPU only |
| PCID / global-page benefit | TCG cannot measure it |
| Physical hardware, Secure Boot | unavailable |
| Dependency advisory scan | no tool installed; zero third-party crates ship |

## Performance and size
See `docs/benchmarks.md`. Base image 212,480 B; port images 33–42 KB each.
