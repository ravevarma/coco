# coco OS

**Compact · Composable · Secure · Fast · Intuitive**

coco is an independent operating system for x86_64 UEFI machines, written
from scratch in Rust: its own UEFI loader, its own kernel, its own boot
protocol — no Linux, BSD, XNU, Windows or Redox underneath.

> **Status: M0 + M1 complete — EXPERIMENTAL, NOT RELEASE READY.**
> coco boots in QEMU/OVMF to a framebuffer splash and serial console, with
> exception handling, a physical frame allocator and a page-table mapper.
> It has no scheduler, processes, drivers or applications yet and has never
> run on physical hardware. See [docs/quality-report.md](docs/quality-report.md).

![coco boot screen in QEMU](docs/images/boot-splash.png)

## What exists

| Component | Path | Notes |
|---|---|---|
| UEFI loader | `boot/` | reads and validates the kernel, builds W^X page tables, exits boot services, hands off |
| Kernel | `kernel/` | GDT/TSS/IDT with IST, fatal-fault diagnostics, framebuffer console, frame allocator, self-tests |
| Boot protocol | `crates/coco-abi/` | versioned `BootInfo`, memory map normalisation, cmdline parsing |
| ELF parser | `crates/coco-elf/` | strict, allocation-free, fuzzed |
| Memory | `crates/coco-memory/` | bitmap frame allocator, 4-level page-table mapper |
| Sync | `crates/coco-sync/` | spin lock, Loom-checked |
| Arch | `arch/x86_64/` | privileged instructions, descriptors, 16550 UART |
| Tooling | `tools/xtask/` | build, GPT/FAT image, size gate, QEMU test matrix, reproducibility check |

Base image today: **75,264 bytes** of the 108,000,000-byte budget
(see [docs/size-report.md](docs/size-report.md)).

## Quick start

```bash
cargo xtask build        # loader + kernel + disk image + size report
cargo xtask qemu-test    # boot test matrix under QEMU/OVMF
cargo xtask run          # interactive boot, serial on stdio
```

Requirements and every other command: [docs/building.md](docs/building.md).

## Documentation

* [Architecture decisions](docs/adr/)
* [Kernel safety invariants](docs/kernel-safety-invariants.md)
* [Threat model](docs/threat-model.md)
* [Hardware compatibility](docs/hardware-compatibility.md)
* [Feature matrix](docs/feature-matrix.md)
* [Benchmarks](docs/benchmarks.md)
* [Known limitations](docs/known-limitations.md)
* [Quality report](docs/quality-report.md)
