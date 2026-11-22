# ADR 0010 — Interrupt controllers, timer and IRQ-safe locking

**Status:** accepted (M2)

* IDT covers all 256 vectors: 32 exception stubs plus 224 IRQ stubs generated
  with an assembler `.rept`, laid out 16 bytes apart; `cpu::init` verifies
  the layout byte-by-byte before loading the IDT.
* Legacy 8259 PICs are remapped to 0x20–0x2F and fully masked; their
  spurious vectors are counted and ignored.
* Local APIC: used in the mode the firmware left (xAPIC via an uncached
  MMIO mapping, or x2APIC via MSRs). `x2apic` on the command line requests
  the switch when CPUID reports support.
* Calibration: 50 ms busy-wait against the ACPI PM timer (HPET fallback),
  measuring TSC and APIC-timer rates, with a timeout so a stuck reference
  cannot hang boot. The APIC timer then runs periodic at 100 Hz on vector
  0x20. Idle is `sti; hlt` (no lost wake-ups).
* Interrupt handlers are allocation-free and lock-free (atomics + APIC
  registers only). Kernel locks that interrupt paths could touch are
  `IrqSpinLock`s (disable interrupts while held, restore afterwards).
* Lock order (rule L3): `HEAP` → `MEMORY` → `CONSOLE`.
* I/O APIC routing (device interrupts) is deferred to M5 with the drivers.
