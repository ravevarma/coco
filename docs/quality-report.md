# coco quality report — M4 (capabilities, channel IPC, first user-space service)

**Release decision: EXPERIMENTAL — NOT RELEASE READY.**
Every gate that applies to the implemented scope passes on revision
`99e8620`. coco is not a release: there is no filesystem, driver set,
service registry, signed boot or hardware validation, and some code paths
(x2APIC, HPET fallback) cannot be exercised here. Nothing in this report
claims the system is free of defects.

Earlier reports: M0/M1 `git show 2f2a915:docs/quality-report.md`,
M2 `git show 778ece7:docs/quality-report.md`,
M3 and the post-M3 hardening pass `git show c2cbb15:docs/quality-report.md`.

## Environment

| Item | Version |
|---|---|
| Source revision tested | `99e8620` (static gates and QEMU matrix from `cargo clean` at `16de847`; matrix re-run after the test fix in `99e8620`) |
| Rust (shipped artifacts) | `rustc 1.97.0 (2d8144b78 2026-07-07)`, pinned |
| Rust (Miri, fuzzing) | `rustc 1.102.0-nightly (bdf712ee8 2026-11-21)`, cargo-fuzz |
| QEMU / firmware | QEMU 8.2.2 TCG (no KVM); OVMF 2024.02-2ubuntu0.10 |

## Results summary

| Gate | Result |
|---|---|
| rustfmt; clippy `-D warnings` (host crates, loader, kernel, user program); rustdoc | ✅ 8/8 |
| Host unit + property tests (debug and release) | ✅ 81/81 each (`coco-abi` 19, `coco-acpi` 11, `coco-arch` 3, `coco-capability` 7, `coco-elf` 8, `coco-ipc` 9, `coco-memory` 19, `coco-sync` 2, `xtask` 3) |
| Miri | ✅ no UB: `coco-capability` 7, `coco-ipc` 9, `coco-abi` 19 at M4; `coco-memory` 19 at M3; `coco-acpi` 11 at M2; `coco-arch` 3, `coco-elf` 8, `coco-sync` 2 at M1 (unchanged since) |
| Loom (`SpinLock`) | ✅ (crate unchanged since M2) |
| Fuzzing (0 crashes) | `capabilities` 11,765,139 runs and `ipc_channel` 1,779,087 runs (new); `boot_info` 21.7 M (M3); `acpi_tables` 31.5 M (M2); `elf_parse` 26.3 M, `memory_map` 47.5 M (M1) |
| QEMU/OVMF matrix | ✅ **33 cases, 35/35 runs** |
| Reproducible build | ✅ loader, kernel, ESP (incl. `init.elf`) and disk image bit-identical |
| Base size ≤ 108,000,000 B | ✅ 212,480 B |
| Third-party code in shipped binaries | ✅ none |

## New in M4 and how it was verified

| Feature | Evidence |
|---|---|
| Capability tables: rights, generation checks, attenuation, limits | host model test (100 k steps, reference counts checked), fuzzing, Miri; user-mode checks 2, 5–9, 13, 19 |
| Channels: bounded queues, too-large/full/peer-closed errors | host model test, fuzzing; user checks 3, 4, 10, 11, 17, 20 |
| Capability transfer (move on success only, TRANSFER required, no self/duplicate transfer) | user checks 12–15 |
| Blocking wait with timeout | user check 18 (TimedOut); the echo service blocks between messages |
| Hostile pointers in IPC calls; message survives a failed receive | user check 16 |
| Reference cycles refused | user check 21; kernel 3-channel chain test |
| First user-space service | echo service + client, 1,000 verified round trips per round |
| Cleanup on crash | client killed while holding an endpoint → service sees `PeerClosed`, exits 0 |
| No leaks | two full rounds: 0 live channels; `free + heap` frames and heap bytes identical |

## Defects found and fixed during M4

1. **Channel side masking (caught by a host test):** `other(side)`
   disagreed with the masked side index for out-of-range input; fixed
   before any kernel use.
2. **Zero-length user ranges rejected (caught by the first IPC boot):**
   empty Rust slices carry dangling pointers (e.g. `0x8`);
   `validate_user_range` and the page walk in `user_range_mapped` both
   rejected them, so every "no handles" send/receive failed. Empty ranges
   are now valid at any address (they touch no memory).
3. **Reference cycles leaked channels (caught by the leak check):** two
   channels holding each other's endpoints kept each other alive forever,
   a kernel-memory denial of service. Sends that would close a cycle are
   now refused, keeping the in-flight graph acyclic.
4. **Broken graveyard re-check (caught in review before running):** the
   first draft's post-drain re-check called `bury` with an empty queue,
   which returned immediately.
5. **Fuzzer could block forever:** a fuzzed `WAIT_READABLE` with timeout 0
   on a valid handle; fuzzed waits are bounded to one tick.
6. **Leak-test invariant (regression caught by the full matrix):** the M3
   user test compared raw free frames; once M4's fuzzer raised peak heap
   use, retained heap pages looked like a leak. All lifecycle tests now
   use `free + heap-held frames` together with exact heap bytes.
7. **Process slip:** commit `0786dfc` says it contains this report, but the
   file write had failed; the report was added in the next commit.

## Unsafe-code audit
227 `unsafe` blocks (arch 31, loader 46, kernel 119, memory 21, sync 4,
user program 6). The two new crates (`coco-capability`, `coco-ipc`) and
the parsers contain none. New invariants I1–I4 (capabilities, channel
references, acyclic in-flight graph, exit cleanup) and the extended lock
order are in `docs/kernel-safety-invariants.md`.

## Concurrency audit
* Lock order `PROCESSES → CHANNEL`, `GRAPH → CHANNEL`,
  `CHANNEL → GRAVEYARD → SCHED → HEAP → MEMORY → CONSOLE`; `PROCESSES` and
  `GRAPH` are never nested. Objects are dropped outside `PROCESSES` and
  outside channel locks.
* Wait/wake: readiness is checked and the thread blocked with interrupts
  disabled (single CPU), so a sender cannot slip in between. This argument
  does not hold on SMP and must be revisited with AP bring-up.

## Not run / unverified
| Item | Reason |
|---|---|
| x2APIC register path; HPET fallback | not available under QEMU 8.2 TCG / q35 |
| SMP correctness of wait/wake and locking | single CPU only |
| IPC throughput optimisation | not attempted (baseline recorded) |
| Physical hardware, Secure Boot | unavailable |
| Dependency advisory scan | no tool installed; zero third-party crates ship |

## Performance and size
See `docs/benchmarks.md`: base 212,480 B; IPC round trip ≈87–104 k cycles
under TCG; context switch 4.2–4.4 k cycles.
