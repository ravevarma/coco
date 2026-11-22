# Memory and performance measurements

Measured in this environment: QEMU 8.2.2 with **TCG software emulation
(no KVM)**, OVMF 2024.02, `-machine q35 -cpu max`, 4 host vCPUs. TCG
timings are not hardware timings; they are a regression baseline only.
Since M2 the TSC is calibrated against the ACPI PM timer (measured
≈2.1 GHz under TCG), so cycle counts can be converted.

## Binary and image size

| Artifact | M1 (`0c9c263`) | M2 (`b60418a`) |
|---|---:|---:|
| `BOOTX64.EFI` (loader) | 31,232 | 31,232 |
| `kernel.elf` | 41,664 | 90,992 |
| Installed base (cluster-rounded) | 75,264 | **124,416** of 108,000,000 |
| Kernel image in memory (text + rodata + data/bss) | 88 KiB | 156 KiB (62 + 12 + 72, incl. 3 IST guard pages) |

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

## Idle CPU (host CPU used by QEMU over 10 s after boot)

| Kernel state | Host CPU |
|---|---:|
| M1: `cli; hlt` (no interrupts) | 0.3% |
| M2: `sti; hlt` with 100 Hz APIC tick | 1.3% |

## Not yet measurable

Scheduler latency, context switches, application launch, redraw latency,
frame time and storage throughput need components that do not exist yet.
Interrupt latency will be measured once a second interrupt source exists.
