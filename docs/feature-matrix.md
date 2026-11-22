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
| M2 | Kernel heap allocator | ⬜ | next |
| M2 | Timers (APIC/HPET/TSC), interrupts | ⬜ | next |
| M2 | Demand paging, CoW, shared memory, quotas, reclamation, DMA buffers | ⬜ | later in M2/M5 |
| M3 | Threads, scheduler, processes, syscalls, user mode | ⬜ | |
| M4 | IPC, capabilities, userspace services | ⬜ | |
| M5 | Storage, input, networking drivers | ⬜ | |
| M6 | Compositor, desktop, CocoKit | ⬜ | only a boot splash exists |
| M7 | Files, Settings, Search, app runtime | ⬜ | |
| M8 | CocoDoc engine; Write, Sheets, Slides | ⬜ | |
| M9 | Audio/video framework, player | ⬜ | |
| M10 | Physical laptop drivers, hardening, performance | ⬜ | |
| M11 | Optional browser / Tor Browser packages | ⬜ | none exist; not counted in base size |
| — | Secure Boot / signed updates / image verification | ⬜ | no claims made |
