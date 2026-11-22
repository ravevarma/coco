# coco quality report — M3 (threads, scheduler, processes, syscalls, isolation)

**Release decision: EXPERIMENTAL — NOT RELEASE READY.**
Every gate that applies to the implemented scope passes on revision
`c89325a`. coco is not a release: there is no IPC, capability model,
filesystem, driver set, signed boot or hardware validation, and some code
paths (x2APIC, HPET fallback) cannot be exercised in this environment.
Nothing here claims the system is free of defects.

Earlier reports: M0/M1 `git show 2f2a915:docs/quality-report.md`,
M2 `git show 778ece7:docs/quality-report.md`.

## Hardening pass after M3 (revision `c527b49`)

A review of the scheduler, process lifecycle, interrupt and syscall paths,
plus two new tests, found and fixed:

| Defect | Severity | Evidence / fix |
|---|---|---|
| **Reaper/join race**: two concurrent reapers (idle thread, `join` caller); a reaper preempted after removing a dead thread's record but before dropping it let `join` return while 640 B (thread record + FPU area) were still held | medium (breaks `join`'s resource guarantee; transient, not a permanent leak) | Found by the new 60 s soak test (failed 2 of 3 runs at rounds 468 and 3,293; diagnostics showed a clean scheduler with +640 B heap). Fixed with a `Reaping` state and an atomic remove+drop under `SCHED`; soak then passed 4 of 4 runs (≈24,000 rounds, ≈216,000 processes, ≈96,000 threads, frames and heap exact every round) |
| **Process-start race**: user thread queued before its id was recorded in the process; a third party waiting on the process could have joined thread 0 | medium (latent: only the spawner waits today) | Threads are now created stopped, registered, then started |
| **Vector overlap**: masked PIC remapped onto 0x20–0x2F, aliasing the APIC timer vector 0x20 | low | PIC remapped to 0x30–0x3F |
| `sched::init` not idempotent | low | second call refused |

New coverage: user mode on a CPU without SMEP/SMAP (`-cpu qemu64`, plain
copy path) and the 60 s soak. After the fixes: 8/8 static gates and
**32 cases / 34 runs** pass in QEMU.

## Environment

| Item | Version |
|---|---|
| Source revision tested | `c89325a` (all gates re-run from `cargo clean`) |
| Rust (shipped artifacts) | `rustc 1.97.0 (2d8144b78 2026-07-07)`, pinned |
| Rust (Miri, fuzzing) | `rustc 1.102.0-nightly (bdf712ee8 2026-11-21)`, cargo-fuzz |
| QEMU / firmware | QEMU 8.2.2 TCG (no KVM); OVMF 2024.02-2ubuntu0.10 |
| Host | Ubuntu 24.04.5, x86_64, 4 vCPUs |

## Results summary

| Gate | Result |
|---|---|
| rustfmt; clippy `-D warnings` on host crates, loader, kernel and the user program; rustdoc `-D warnings` | ✅ 8/8 |
| Host unit + property tests (debug and release) | ✅ 64/64 each (`coco-abi` 18, `coco-acpi` 11, `coco-arch` 3, `coco-elf` 8, `coco-memory` 19, `coco-sync` 2, `xtask` 3) |
| Miri | ✅ no UB: re-run at M3 for the changed crates `coco-abi` 18 + `coco-memory` 19; `coco-acpi` 11 at `b60418a`; `coco-arch` 3, `coco-elf` 8, `coco-sync` 2 at `0c9c263` (unchanged since) |
| Loom (`SpinLock`) | ✅ 2/2 (crate unchanged since M2) |
| Fuzzing | ✅ 0 crashes: `boot_info` 21,736,346 runs at M3 (BootInfo v2); `acpi_tables` 31.5 M (M2); `elf_parse`, `memory_map` 26.3 M / 47.5 M (M1; unchanged code) |
| QEMU/OVMF matrix | ✅ **30 cases, 32/32 runs** (`selftest-256M` runs 3×; the lifecycle case boots the kernel 3 times) |
| Reproducible build | ✅ loader, kernel, ESP (incl. `init.elf`) and disk image bit-identical across two checkouts |
| Base size ≤ 108,000,000 B | ✅ 179,712 B |
| Third-party code in shipped binaries | ✅ none |

## New in M3 and how it was verified

| Feature | Evidence |
|---|---|
| Kernel threads, context switch with FXSAVE, guard-paged stacks | `threads`: all 4 threads progressed; 300 spawn/join cycles; frames and heap exactly at baseline |
| Timer preemption, fairness | Two never-yielding spinners: 837,405 vs 886,065 iterations |
| Context-switch cost | 4,160 cycles per switch (TCG) |
| User processes, ring-3 entry, `init.elf` from the loader | `boot-default`: `init: hello from user space, pid 1`, exit 0 |
| Cross-process preemption | hello finished at tick 2 while a spinner process ran to tick 32 |
| Fault isolation | null deref (#PF 0x4), kernel read (#PF 0x5), `cli` (#GP), execute stack (#PF 0x15), write to text (#PF 0x7), user stack overflow (#PF 0x6): each process killed, kernel continued |
| Syscall validation | 2 fuzz processes (≈18.4 k hostile calls each) exited 0; every result was a success value or a defined error code |
| Process lifecycle without leaks | 120 processes (100 sequential, 20 concurrent) with correct exit codes; frames 63,403 → 63,403, heap 27,648 → 27,648 B |
| Address-space teardown | host test (every leaf and table frame returned exactly once, kernel half intact) + Miri + in-kernel churn |

## Memory findings
* No leaks in thread or process lifecycles (exact frame and heap
  accounting above), in addition to the M1/M2 allocator and heap checks.
* Address-space teardown frees frames without heap allocation while the
  memory lock is held. This was a lock-order defect in the first draft,
  caught in review before it ran (see below).

## Unsafe-code audit
223 `unsafe` blocks (arch 31, loader 46, kernel 116, memory 21, sync 4,
user program 5); `coco-abi`, `coco-acpi` and `coco-elf` contain none.
All have a `SAFETY:` justification and a single unsafe operation each
(lint-enforced) and were reviewed against
`docs/kernel-safety-invariants.md`, which gains C5/C6 (per-CPU block, GS),
L5 (no lock across a switch), T1–T2 (threads), P1 (address spaces) and
U1–U3 (user memory, return to user, fault containment).

## Concurrency audit
* Preemption happens only outside `IrqSpinLock` critical sections.
  `SCHED` is never held across a switch: raw pointers to the two thread
  records are taken under the lock and used with interrupts disabled.
* Lock order `SCHED → HEAP → MEMORY → CONSOLE`; `PROCESSES` is never
  combined with another lock. The tick path takes only `SCHED` and never
  allocates (queue capacity is reserved).
* Single CPU: cross-CPU races, TLB shootdown and per-CPU scheduling become
  relevant with SMP bring-up.

## Defects found and fixed during M3
1. **Lock-order defect (caught in review, never ran):** the first draft of
   address-space teardown collected frames into `Vec`s while holding
   `MEMORY`, which could deadlock if the heap needed to grow. It now frees
   frames from the teardown callbacks.
2. **Unsound draft (caught in review, never ran):** `set_kernel_stack`
   briefly formed a `&mut` to the CPU tables (violating C1); replaced with
   raw-pointer, unaligned writes.
3. The thread test counted its own `Vec`s as leaks (false positive); the
   test now uses fixed-size bookkeeping.
4. A self-test label reported all context switches as "in 30 ticks";
   corrected to the measured phase (70 switches).
5. The user-isolation test took 35 s because fuzzed `sleep` calls slept
   about 1,500 ticks; `sleep(0)` now yields (documented ABI), and the case
   takes 9 s.
6. Lints: missing/invalid SAFETY comments, double unsafe operations, a
   rustdoc link (`STAR[63:48]`).
7. Documentation: two wrong numbers I typed were caught against the logs
   before commit (an `unsafe fn` count of 18 instead of 17; the syscall
   fuzz success counts).
8. **Correction to the M2 report (`778ece7`):** it stated "30/30 cases in
   32 runs". The QEMU harness counts runs: M2 had **28 cases in 30 runs**.
   All of them passed; only the count was wrong.

## Not run / unverified
| Item | Reason |
|---|---|
| x2APIC register path; HPET fallback | not available under QEMU 8.2 TCG / q35 |
| Multi-threaded processes, IPC, capabilities | not implemented (M3 follow-ups / M4) |
| Soak longer than 60 s | not run (the test takes `secs=N`) |
| Physical hardware, Secure Boot | unavailable |
| Dependency advisory scan | no tool installed; zero third-party crates ship |

## Performance and size
See `docs/benchmarks.md`: base 179,712 B; context switch ≈4.2 k cycles;
idle 1.2% host CPU with scheduler and 100 Hz tick; boot to kernel entry
≈2.0–2.2 s under TCG.
