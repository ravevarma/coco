# coco quality report — M2 (kernel memory, interrupts, timer, ACPI, power)

**Release decision: EXPERIMENTAL — NOT RELEASE READY.**
Every gate that applies to the implemented scope passes on revision
`b60418a`. The build is not a release: processes, user mode, drivers,
signed boot and hardware testing do not exist yet, and two implemented
code paths (x2APIC, HPET fallback) could not be exercised in this
environment. Nothing here claims the system is free of defects.

The M0/M1 report for `0c9c263` is preserved in git history
(`git show 2f2a915:docs/quality-report.md`).

## Environment

| Item | Version |
|---|---|
| Source revision tested | `b60418a` (all gates re-run from `cargo clean`) |
| Rust (shipped artifacts) | `rustc 1.97.0 (2d8144b78 2026-07-07)`, pinned |
| Rust (Miri, fuzzing) | `rustc 1.102.0-nightly (bdf712ee8 2026-11-21)`, cargo-fuzz |
| QEMU / firmware | QEMU 8.2.2 TCG (no KVM); OVMF 2024.02-2ubuntu0.10 |
| Host | Ubuntu 24.04.5, x86_64, 4 vCPUs |

## Results summary

| Gate | Result |
|---|---|
| rustfmt; clippy `-D warnings` (host crates, loader target, kernel target); rustdoc `-D warnings` | ✅ PASS |
| Host unit + property tests (debug and release) | ✅ 60/60 each (`coco-abi` 15, `coco-acpi` 11, `coco-arch` 3, `coco-elf` 8, `coco-memory` 18, `coco-sync` 2, `xtask` 3) |
| Miri, all 6 host library crates | ✅ 57/57, no UB reported (re-run at M2: `coco-acpi` 11 + `coco-memory` 18; `coco-abi` 15, `coco-arch` 3, `coco-elf` 8, `coco-sync` 2 unchanged since their `0c9c263` run) |
| Loom (`SpinLock`) | ✅ 2/2; mutation (Relaxed) detected |
| Fuzzing | ✅ 0 crashes: `acpi_tables` 31,452,968 runs (new); `elf_parse`, `memory_map`, `boot_info` 104.1 M runs at `0c9c263` (code unchanged) |
| QEMU/OVMF matrix | ✅ **30/30** cases in 32 runs (`selftest-256M` ×3); the lifecycle case alone boots the kernel 3 times |
| Reproducible build | ✅ loader, kernel, ESP and disk image bit-identical across two checkouts |
| Base size ≤ 108,000,000 B | ✅ 124,416 B |
| Third-party code in shipped binaries | ✅ none |

## New in M2 and how it was verified

| Feature | Verification |
|---|---|
| Kernel heap (size classes + coalescing runs, double-free detection, poisoning) | 7 host tests incl. a 40,000-step model test checking overlap, alignment, contents and accounting; OOM + recovery; use-after-free write detection; Miri. In-kernel: 3 churn rounds (~217 k ops) returning exactly to baseline; exhaustion to clean OOM (496 MiB) and full recovery |
| Allocation in interrupt context refused | `irq-alloc-refused`: refusal message + controlled abort |
| ACPI (RSDP/XSDT/RSDT, MADT, FADT, HPET, `_S5`) | 11 host tests incl. 20,000 random corruptions; Miri; 31.5 M fuzz runs from a valid seed; QEMU: 1 and 4 CPUs detected correctly |
| IDT 256 vectors, PIC masked, local APIC, IRQ-safe locks | all M2 QEMU cases; 0 unexpected interrupts |
| TSC/APIC-timer calibration and 100 Hz tick | `timer`: 100 ticks = 1,000–1,001 ms by TSC |
| IST guard pages; SMEP/SMAP/UMIP | `protections` self-test; `-cpu qemu64` correctly reports them absent |
| ACPI reset, S5 power-off, boot/reboot/power-off lifecycle | `lifecycle-3-boots` (memory suite passes on every boot; CMOS counter survives resets; QEMU exits 0 on S5); `shutdown` |

## Memory findings
* No leaks: heap in-use returns to the exact baseline after every churn
  round and after exhaustion; frame-allocator counts are restored after
  full exhaustion (63,679 frames at 256 MiB, 1,046,623 at 4 GiB); the
  allocated-frame count is constant across 2,000 map/unmap cycles.
* OOM is a clean error at both layers (frames and heap) and both recover.
* Known, documented retention: heap memory is not returned to the frame
  allocator (8.9 MiB stays mapped after the churn test).

## Unsafe-code audit
175 `unsafe` blocks (arch 28, loader 45, kernel 77, memory 21, sync 4);
`coco-abi`, `coco-acpi` and `coco-elf` contain none. All carry `SAFETY:`
justifications with one unsafe operation each (lint-enforced) and were
reviewed against `docs/kernel-safety-invariants.md` (new: M9 MMIO, M10
heap, C3 interrupt rules, L1/L3 locking). Heap header casts go through two
helpers that state and debug-assert the alignment invariant.

## Concurrency audit
* Interrupts are now live (APIC timer). All kernel-global locks are
  `IrqSpinLock`s; handlers take no locks and do not allocate.
* Lock order `HEAP → MEMORY → CONSOLE`; the only nested acquisition is heap
  growth taking `MEMORY`. Trap/panic paths remain non-blocking (L2,
  `fault-with-console-locked`).
* Still single-CPU: data races across CPUs, priority inversion and
  starvation become testable with SMP and the scheduler (M3).

## Defects found and fixed during M2
1. Heap model test hit OOM: diagnosed as genuine exhaustion (8,347,488 of
   8,388,608 bytes in use) caused by an alloc-biased test; live set bounded.
2. QEMU expectations out of date after unified test reporting (2 cases).
3. x2APIC "force" case could not pass: QEMU 8.2 TCG does not implement
   x2APIC. The kernel correctly refused (CPUID check); the case now tests
   that fallback and the x2APIC path is reported **unverified**.
4. Self-test label claimed "tables retained" for a total allocated-frame
   count; relabelled (the leak check itself was correct).
5. Lints: unchecked arithmetic (ACPI root count), alignment casts (heap),
   double unsafe ops (HPET config), collapsible conditions.
6. A benchmark table was briefly edited with values that did not match the
   logs; caught during review and replaced with the gate-run values.

## Not run / unverified (reported as such, not as passed)
| Item | Reason |
|---|---|
| x2APIC register path | QEMU 8.2 TCG lacks x2APIC |
| HPET calibration fallback | q35 always provides the PM timer |
| Process/IPC/application lifecycle tests | not implemented yet (M3+) |
| Long-duration (hours) memory-growth soak | not run; idle heap is static (1 allocation) |
| Physical hardware, Secure Boot | unavailable |
| Dependency advisory scan | no tool installed; zero third-party crates ship |

## Performance and size
See `docs/benchmarks.md`. Highlights: base 124,416 B; kernel steady-state
RAM ≈400 KiB at 256 MiB; idle 1.3% host CPU with a 100 Hz tick; boot to
kernel entry ≈2.1 s and kernel init ≈79 ms under TCG.
