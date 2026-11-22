# Memory and performance measurements

Measured in this environment: QEMU 8.2.2 with **TCG software emulation
(no KVM)**, OVMF 2024.02, `-machine q35 -cpu max`, 4 host vCPUs. TCG
timings are not hardware timings; they are a regression baseline only.
Since M2 the TSC is calibrated against the ACPI PM timer (measured
≈2.1 GHz under TCG), so cycle counts can be converted.

## Binary and image size

| Artifact | M1 (`0c9c263`) | M2 (`b60418a`) | M3 (`c89325a`) | M4 (`99e8620`) | post-M4 audit + ports |
|---|---:|---:|---:|---:|---:|
| `BOOTX64.EFI` (loader) | 31,232 | 31,232 | 32,256 | 32,256 | 32,256 |
| `kernel.elf` | 41,664 | 90,992 | 132,160 | 160,872 | 160,872 (audit fixes and `test=bench` +4,096; fallible cycle check without `BTreeSet` −4,096) |
| `init.elf` (first user program) | — | — | 12,648 | 16,760 | 16,760 |
| Installed base (cluster-rounded) | 75,264 | 124,416 | 179,712 | 212,480 | **212,480** of 108,000,000 |
| Kernel image in memory (text + rodata + data/bss) | 88 KiB | 156 KiB | 196 KiB | — | — |

The M2 growth comes from the heap and `alloc` collections, ACPI parsing,
APIC/timer code and the in-kernel self-tests (which ship in the same
binary so that the tested artifact is the released one).

### Optimization experiments (M1 source, measured)

| Change | Kernel file | Loader | Decision |
|---|---:|---:|---|
| IST stacks moved from `.data` to `.bss` | 90,816 → 41,664 (−54%) | — | kept |
| `/Brepro` + `/DEBUG:NONE` (determinism) | — | 31,232 → 31,232 (no size change) | kept |
| `opt-level = "s"` | 41,664 (text 0x5a14) | 31,232 | **selected** |
| `opt-level = "z"` | 41,664 (text 0x57af) | 32,256 | loader larger |
| `opt-level = 2` | 49,856 | 33,792 | larger, no measured hot path yet |
| `opt-level = 3` | 53,952 | 36,352 | larger, no measured hot path yet |

## Boot (host wall-clock, `cargo xtask qemu-test`, revision `b60418a`)

| Case | Firmware start → kernel entry (s) | → completion (s) |
|---|---:|---:|
| boot-default (256 MiB) | 2.09 | 2.68 (incl. screenshot) |
| selftest-256M, 3 cold boots | 2.24 / 2.09 / 2.12 | 3.66 / 3.58 / 3.55 |
| selftest-4G | 2.27 | 5.22 |
| lifecycle: 3 boots + 2 ACPI resets + S5 power-off | 2.04 (first entry) | 6.46 |

Kernel stage breakdown (boot-default, TSC cycles at ≈2.1 GHz):
CPU tables 5.4 M · framebuffer + splash 24.2 M · memory init 6.4 M ·
heap + ACPI + APIC/timer 127.1 M (≈60 ms; dominated by the 50 ms
calibration busy-wait) · total kernel 166.5 M (≈79 ms).

## Kernel memory footprint (steady state, 256 MiB guest, no tests)

| Item | KiB |
|---|---:|
| Kernel image | 156 |
| Boot stack | 64 |
| Loader page tables | 92 |
| Boot info | 8 |
| Frame bitmaps | 16 (384 at 4 GiB) |
| Heap mapped (1 allocation: ACPI info) | 64 |
| **Total** | **400** |

## Micro-benchmarks (in-kernel, TCG; `selftest-256M` last of 3 runs / `selftest-4G`, revision `b60418a`)

| Operation | 256 MiB | 4 GiB |
|---|---:|---:|
| Frame alloc incl. 2 direct-map writes (cycles/frame, full exhaustion) | 2,775 | 2,814 |
| Frame free incl. 2 direct-map reads | 376 | 388 |
| 4 KiB map + write + read + unmap + invlpg + alloc/free (cycles/page) | 2,234 | 2,158 |
| Heap churn (Vec growth, 2,250 boxed slices, 20 k BTreeMap ops ×3; cycles/op, poisoning on) | 1,986 | 2,223 |
| Heap peak / mapped after churn | 8,078 / 8,924 KiB | same |
| Heap exhaustion (512 MiB guest): 1 MiB blocks before clean OOM | 496 | — |

## Timer accuracy

100 ticks at 100 Hz measured **1,001 ms** (256 MiB) and **1,000 ms**
(4 GiB) by the calibrated TSC, with 0 spurious and 0 unexpected
interrupts. Calibrated rates: TSC 2,102–2,108 MHz, APIC timer
62.6–62.8 MHz after the ÷16 divider.

## Scheduler and processes (M3, revision `c89325a`, TCG)

| Measurement | Value |
|---|---:|
| Context switch (yield ping-pong between two kernel threads, incl. FXSAVE/FXRSTOR) | 4,160 cycles (≈1,979 ns at 2.1 GHz TSC) |
| Fairness: two never-yielding spinners over 30 ticks | 837,405 : 886,065 iterations (1.06×) |
| Context switches during that phase (4 threads + main) | 70 |
| Cross-process preemption | `init` hello finished at tick 2 while a spinning process ran until tick 32 |
| Thread churn | 300 spawn/join: frames and heap exactly back to baseline |
| Process churn | 120 processes (100 sequential, 20 concurrent): frames 63,403 → 63,403, heap 27,648 → 27,648 B |
| Hostile syscalls | 2 fuzz processes, 20,000 draws each (draws of `exit` skipped): 6,312 + 12,123 and 6,150 + 12,342 (succeeded + rejected with a valid error code); both processes exited 0 |
| Kernel-half pre-population | 252 level-3 tables (≈1 MiB) |

## IPC (M4, revision `99e8620`, TCG)

| Measurement | Value |
|---|---:|
| Echo round trip (client send → service recv/upper-case/send → client recv, both blocking) | 86,741 and 104,281 cycles (two rounds; ≈41–50 µs) |
| Context switch, re-measured (3 runs, quiet host) | 4,172–4,422 cycles (6,278 observed once inside the busy full-matrix run) |

## Idle CPU (host CPU used by QEMU over 10 s after boot)

| Kernel state | Host CPU |
|---|---:|
| M1: `cli; hlt` (no interrupts) | 0.3% |
| M2: `sti; hlt` with 100 Hz APIC tick | 1.3% |
| M3: scheduler + idle thread, 100 Hz tick | 1.2% |

## Syscall, yield and IPC cost (post-M4 audit, `test=bench`, TCG)

One quiet-host run of the `bench-ipc` case (other runs during the audit
varied by up to ±40%: the host is shared and TCG is sensitive to it).

| Measurement | Value |
|---|---:|
| Null syscall (`GET_PID` round trip, SYSCALL/SYSRET + swapgs) | 592 cycles (336 in another run) |
| Process yield (two processes yielding to each other; one figure covers both turns and two CR3 switches) | 36,501 / 36,678 cycles |
| Echo IPC round trip | 104,058 / 93,527 cycles (two rounds) |

Finding: the kernel's own paths are small (syscall ≈0.6 k cycles, kernel
thread switch ≈4.2 k); the process-to-process cost is dominated by
address-space switches, which flush the whole software TLB under TCG.
PCID and global kernel pages would remove most of that on hardware, but
TCG cannot measure the benefit, so they are recorded as a hardware
measurement item rather than claimed. A combined send+wait syscall
(`CHANNEL_CALL`) was implemented and measured: no reproducible gain
(round trips 59–75 k cycles with it vs 72–76 k without, overlapping),
so it was **removed** to keep the ABI small.

## Portable bring-up kernel (QEMU TCG, `cargo xtask port-test`)

| Port | ELF file (B) | text + data + bss (B) | 20 ticks at 100 Hz, by independent counter | Frames exhausted and recovered |
|---|---:|---:|---:|---:|
| RISC-V 64 | 33,376 | 26,900 | 203–205 ms (`rdtime`) | 32,356 (126 MiB) |
| RISC-V 32 | 37,256 | 28,482 | 204–205 ms (CLINT `mtime`) | 32,451 (126 MiB) |
| AArch64 | 41,568 | 32,128 | 204–207 ms (`cntvct`) | 32,212 (125 MiB) |
| x86 32-bit | 33,176 | 26,760 | 199–200 ms (TSC, PIT-calibrated) | 32,198 (125 MiB) |

Each image contains the full self-test suite, the frame allocator, heap,
capability and IPC code, and a formatting console.

Size experiment: lld aligns AArch64 segments to 64 KiB by default; linking
with `-z max-page-size=4096` (all ports use 4 KiB pages) shrank the AArch64
file from **103,008 to 41,568 bytes (−60%)**. The other targets already
used 4 KiB and are unchanged.

## Not yet measurable

Scheduler latency, context switches, application launch, redraw latency,
frame time and storage throughput need components that do not exist yet.
Interrupt latency will be measured once a second interrupt source exists.
