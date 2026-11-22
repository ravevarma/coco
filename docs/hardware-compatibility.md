# Hardware compatibility matrix

coco makes **no** claim of compatibility with any physical laptop yet. Every
"verified" entry below was exercised by an automated test in QEMU; nothing
has been run on real hardware.

## Verified (QEMU 8.2.2, TCG, OVMF edk2-stable 2024.02 non-Secure-Boot)

| Component | Configuration | Driver | Status | Evidence |
|---|---|---|---|---|
| Firmware | UEFI 2.x (OVMF `OVMF_CODE_4M.fd`) | coco-boot | ✅ boots | `cargo xtask qemu-test` (all cases) |
| CPU | `-cpu max`, x86_64, 4-level paging, NX | arch | ✅ | all cases |
| SMP | `-smp 4` (APs left parked by firmware) | — | ✅ boots, BSP only | `selftest-smp4-1G` |
| RAM | 256 MiB, 1 GiB, 4 GiB (memory above 4 GiB) | frame allocator | ✅ | `selftest-*` |
| Boot disk | NVMe (`-device nvme`) | firmware driver (pre-ExitBootServices) | ✅ | default for all cases |
| Boot disk | SATA AHCI (`ide-hd` on ICH9 AHCI) | firmware driver | ✅ | `selftest-ahci` |
| Display | GOP linear framebuffer 1280×800, BGRX | kernel `fb` | ✅ pixel-verified | `boot-default` screendump check |
| Serial | 16550 UART at COM1 (0x3F8), 115200 8N1 | `coco_arch::serial` | ✅ | all cases |
| Debug exit | `isa-debug-exit` at 0xF4 (QEMU only, opt-in `qemu-exit`) | kernel | ✅ | all exit-code cases |

## Not supported yet (planned milestone)

| Area | Milestone |
|---|---|
| ACPI table parsing (RSDP address is already passed to the kernel), APIC, HPET/TSC calibration, timer interrupts | M2 |
| SMP bring-up (APs), per-CPU data | M3 |
| PCIe enumeration, NVMe/AHCI/xHCI/USB HID/VirtIO drivers (coco's own) | M5 |
| Keyboard, touchpad, Wi-Fi, Ethernet, GPU acceleration, audio | M5–M10 |
| Battery/thermal, suspend/resume | M10 |
| 5-level paging (LA57) | rejected by the loader with a clear error |
| GOP pixel-bitmask / BLT-only modes | boots serial-only |
| Secure Boot | loader is unsigned; OVMF Secure Boot variant not tested |

## Device IDs

No PCI/USB device is driven by coco itself yet, so there are no device IDs
to list. This table will be filled as drivers land, together with the
physical laptop selected for M10.
