# Feature completeness matrix

Legend: ✅ implemented and tested · 🟡 partial · ⬜ not started.
"Tested" means an automated test exercises it (host, Miri, Loom, fuzz or QEMU).

| Milestone | Feature | Status | Notes / evidence |
|---|---|---|---|
| M0 | Pinned toolchain, Cargo workspace, lints | ✅ | `rust-toolchain.toml`, `cargo xtask check` |
| M0 | Reproducible build | ✅ | `cargo xtask repro`: loader, kernel, ESP, disk image bit-identical |
| M0 | Size report + 108 MB gate | ✅ | `cargo xtask build` fails above the ceiling |
| M1 | Native UEFI loader | ✅ | hand-written UEFI bindings, no third-party code |
| M1 | ELF loading with W^X policy | ✅ | `coco-elf` (fuzzed), loader maps per-segment permissions |
| M1 | Kernel entry, higher-half, guard-paged stack | ✅ | QEMU `fault-guard-page`, `fault-stack-overflow` |
| M1 | Serial diagnostics | ✅ | COM1 probe + loopback test |
| M1 | Framebuffer + boot splash + text console | ✅ | pixel-verified screendump |
| M1 | Boot-info validation | ✅ | host tests + fuzzing + loader rejection tests |
| M2 | GDT/TSS/IDT, exception handling with IST | ✅ | 10 fault-class QEMU tests |
| M2 | Physical frame allocator | ✅ | host property tests, Miri, in-kernel exhaustion test to 4 GiB |
| M2 | Page-table mapper (map/unmap/translate, 4K/2M) | ✅ | model-based tests, Miri, in-kernel leak test |
| M2 | Kernel heap allocator (hardened) | ✅ | host model tests + Miri; in-kernel churn, exhaustion/recovery, IRQ refusal |
| M2 | ACPI tables (MADT, FADT, HPET, `_S5`) | ✅ | host tests, Miri, 31 M fuzz runs, QEMU |
| M2 | Interrupts: IDT 256 vectors, PIC masked, local APIC (xAPIC) | ✅ | QEMU; x2APIC path 🟡 unverified |
| M2 | TSC + APIC timer calibration, 100 Hz tick, `sti;hlt` idle | ✅ | `timer` case: 1,001 ms per 100 ticks |
| M2 | IRQ-safe locks, lock order | ✅ | `fault-with-console-locked`, Loom (inner lock) |
| M2 | IST guard pages; SMEP/SMAP/UMIP | ✅ | `protections` self-test |
| M2 | Reboot (ACPI reset) and power-off (S5); lifecycle | ✅ | `lifecycle-3-boots`, `shutdown` |
| M2 | Tickless idle, HPET as clock source | ⬜ | |
| M2 | Demand paging, CoW, shared memory, quotas, reclamation, DMA buffers | ⬜ | with M3/M5 (need processes/drivers) |
| M3 | Kernel threads, context switch (FXSAVE), guard-paged stacks | ✅ | `threads`: 300 spawn/join, exact frame+heap baseline |
| M3 | Preemptive round-robin, sleep/yield, deferred reaping | ✅ | spinner shares 837k:886k; switch ≈4.2 k cycles |
| M3 | Processes: private address spaces, ELF user loading, ring 3 | ✅ | `user-isolation`, `boot-default` (init pid 1) |
| M3 | SYSCALL/SYSRET, validated user pointers (SMAP-aware) | ✅ | 2 fuzz processes, ≈18.4 k hostile syscalls each, survived |
| M3 | Fault isolation (process killed, kernel continues) | ✅ | 6 fault classes |
| M3 | Process churn without leaks | ✅ | 120 processes, frames/heap exact |
| M3 | Multi-threaded processes, spawn/exec from user space, signals | ⬜ | |
| M3 | Per-CPU queues, affinity, priorities, real-time budgets | ⬜ | needs SMP |
| M4 | Capability handle tables (rights, generations, attenuation, limits) | ✅ | host model test, Miri, 11.8 M fuzz runs, 21 user-mode checks |
| M4 | Channels: bounded messages, handle transfer, peer-closed, blocking wait | ✅ | echo service/client, kill cleanup |
| M4 | Reference-cycle refusal; iterative release of chains | ✅ | `ipc-capabilities` (0 live channels after 2 rounds) |
| M4 | First user-space service (echo) | ✅ | 1,000 verified round trips |
| M4 | Name/service registry, shared memory objects, process spawning from user space | ⬜ | next |
| M5 | Storage, input, networking drivers | ⬜ | |
| M6 | Compositor, desktop, CocoKit | ⬜ | only a boot splash exists |
| M7 | Files, Settings, Search, app runtime | ⬜ | |
| M8 | CocoDoc engine; Write, Sheets, Slides | ⬜ | |
| M9 | Audio/video framework, player | ⬜ | |
| M10 | Physical laptop drivers, hardening, performance | ⬜ | |
| M11 | Optional browser / Tor Browser packages | ⬜ | none exist; not counted in base size |
| — | Secure Boot / signed updates / image verification | ⬜ | no claims made |
