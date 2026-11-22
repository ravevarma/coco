# Memory and performance measurements

All numbers below were **measured** on revision `0c9c263` in this
environment: QEMU 8.2.2 with **TCG software emulation (no KVM)**, OVMF
2024.02, `-machine q35 -cpu max`, host with 4 vCPUs. TCG timings are not
hardware timings; they are recorded as a regression baseline only. TSC
values are cycles of QEMU's emulated TSC; coco does not yet calibrate the
TSC, so no conversion to time is claimed.

## Binary and image size

| Artifact | Bytes |
|---|---:|
| `BOOTX64.EFI` (loader) | 31,232 |
| `kernel.elf` | 41,664 |
| Installed base (cluster-rounded, incl. directories) | 75,264 of 108,000,000 |

### Optimization experiments (measured, same source)

| Change | Kernel file | Loader | Decision |
|---|---:|---:|---|
| IST stacks moved from `.data` to `.bss` | 90,816 → 41,664 (−54%) | — | kept |
| `/Brepro` + `/DEBUG:NONE` (determinism) | — | 31,232 → 31,232 (no size change) | kept |
| `opt-level = "s"` | 41,664 (text 0x5a14) | 31,232 | **selected** |
| `opt-level = "z"` | 41,664 (text 0x57af) | 32,256 | loader larger |
| `opt-level = 2` | 49,856 | 33,792 | larger, no measured hot path yet |
| `opt-level = 3` | 53,952 | 36,352 | larger, no measured hot path yet |

Performance-oriented levels will be applied selectively once a hot path
(scheduler, IPC, compositor) exists and shows a measured benefit.

## Boot (QEMU TCG, host wall-clock, `cargo xtask qemu-test`)

| Case | Firmware start → kernel entry (s) | → test completion (s) |
|---|---:|---:|
| boot-default (256 MiB) | 1.94 | 2.52 (incl. screenshot) |
| selftest-256M, 3 cold boots | 1.88 / 1.89 / 1.93 | 2.01 / 1.99 / 2.11 |
| selftest-4G | 1.91 | 3.50 |
| selftest-smp4-1G | 2.17 | 2.52 |

Almost all of the time to kernel entry is OVMF itself. Stage breakdown from
the kernel's TSC log (boot-default, cycles): loader 30.7 M · handoff→entry
0.13 M · CPU tables 2.9 M · framebuffer + splash 14.2 M · memory init 3.7 M.
The splash is the most expensive kernel stage (volatile per-pixel writes to
an uncached framebuffer under TCG); a back buffer is planned with the
compositor.

## Kernel memory footprint (steady state after boot, 256 MiB guest)

| Item | KiB |
|---|---:|
| Kernel image (text 24 + rodata 8 + data/bss 56) | 88 |
| Boot stack | 64 |
| Page tables built by the loader | 92 |
| Boot info (header, memory map, cmdline) | 8 |
| Frame-allocator bitmaps | 16 (384 at 4 GiB) |
| **Total** | **268** |

No heap exists yet. Free memory at 256 MiB: 63,717 frames (248 MiB);
at 4 GiB: 1,046,661 frames (4,088 MiB).

## Allocator and paging micro-benchmarks (in-kernel, TCG)

| Operation | 256 MiB guest | 4 GiB guest |
|---|---:|---:|
| Frame alloc incl. 2 direct-map writes (cycles/frame, full exhaustion) | 2,575 | 2,691 |
| Frame free incl. 2 direct-map reads (cycles/frame) | 968 | 374 |
| 4 KiB map + write + read + unmap + invlpg + alloc/free (cycles/page) | 4,565 | 2,163 |
| Page-table frames retained after 2,000 map/unmap cycles | 3 | 3 |

## Idle CPU

After boot the kernel executes `cli; hlt`. Host CPU used by the QEMU
process over a 10 s window after `coco: boot complete`: **0.3%** (3 ticks
at 100 Hz) — the vCPU is halted, not spinning.

## Not yet measurable

Scheduler latency, interrupt latency, context switch cost, application
launch time, window redraw latency, frame time and storage throughput need
components that do not exist yet (timers, scheduler, compositor, drivers).
