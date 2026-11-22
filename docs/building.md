# Building and testing coco

## Toolchain (pinned)
* Rust **1.97.0** stable with targets `x86_64-unknown-uefi` and
  `x86_64-unknown-none` and components rustfmt, clippy, rust-src,
  llvm-tools — installed automatically by rustup from `rust-toolchain.toml`.
* Host tools: `mtools` (FAT image), `qemu-system-x86_64` (tested 8.2.2) and
  OVMF (`/usr/share/OVMF/OVMF_CODE_4M.fd`, `OVMF_VARS_4M.fd`; tested edk2
  2024.02), coreutils `sha256sum`.
  On Ubuntu 24.04: `apt-get install qemu-system-x86 ovmf mtools`.
* Ports and cross-architecture tests: `qemu-system-misc` (RISC-V),
  `qemu-system-arm` (AArch64), `qemu-system-x86` (i386), `qemu-user`
  (`qemu-aarch64`, `qemu-riscv64`); Rust targets `aarch64-unknown-linux-musl`
  and `riscv64gc-unknown-linux-musl` for `cross-test`
  (`rustup target add …`); the other port targets come from
  `rust-toolchain.toml`. The 32-bit x86 port links with the host `cc` and
  `rust-lld`.
* Optional: nightly Rust with Miri (`rustup toolchain install nightly
  --component miri`) and `cargo install cargo-fuzz`.

## Commands
| Command | What it does |
|---|---|
| `cargo xtask build` | release loader + kernel, `target/coco/esp/`, `esp.img`, GPT `coco.img`, size report (fails above 108,000,000 bytes) |
| `cargo xtask check` | rustfmt, clippy `-D warnings` (host, x86_64 bare-metal targets, and `coco-port` for all four port targets), tests (debug + release), rustdoc |
| `cargo xtask qemu-test [filter]` | QEMU/OVMF boot matrix; logs in `target/coco/qemu/<case>/` |
| `cargo xtask run [--cmdline "test=memory"] [--memory 512M] [--smp 2]` | interactive boot, serial on stdio |
| `cargo xtask port-test [filter]` | builds `ports/coco-port` for RISC-V 64/32, AArch64 and x86 (32-bit), boots each in QEMU, plus two negative cases per port; results in `target/coco/port-results.md`, logs in `target/coco/ports/` |
| `cargo xtask cross-test` | portable crates' unit tests on i686 (native), AArch64 and RISC-V 64 (QEMU user mode); release builds for six bare-metal targets |
| `cargo xtask repro` | builds from two copies of the tree at different paths and compares SHA-256 of every artifact |
| `rustup run nightly cargo miri test -p coco-abi -p coco-acpi -p coco-capability -p coco-elf -p coco-fdt -p coco-ipc -p coco-memory -p coco-sync -p coco-arch-x86_64` | Miri |
| `RUSTFLAGS="--cfg loom" cargo test -p coco-sync --release --target-dir target/loom` | Loom |
| `cd fuzz && cargo +nightly fuzz run elf_parse -- -max_total_time=300` | fuzzing (`elf_parse`, `memory_map`, `boot_info`, `acpi_tables`, `capabilities`, `ipc_channel`, `fdt_parse` — seed the last with `crates/coco-fdt/testdata`) |

## Kernel command line (`\coco\cmdline.txt` on the ESP)
* `test=memory` — frame allocator, paging, protection policy, heap churn,
  timer accuracy and #BP self-tests.
* `test=heap` — heap churn plus exhaustion/recovery (consumes free frames).
* `test=threads` — kernel-thread fairness, context-switch cost, 300-thread churn.
* `test=ipc` — echo service/client, 21 user-mode capability checks, kill
  cleanup, cycle refusal, IPC fuzzing; two rounds with exact accounting.
* `test=soak secs=N` — N seconds of process/thread/heap churn; frames and
  heap must be exactly at baseline after every round.
* `test=user` — user processes: preemption across processes, 6 fault
  classes killed, 2 syscall fuzzers, 120-process churn, leak checks.
* (no `test=`) — boots and runs `\coco\init.elf` as pid 1.
* `test=timer` — 100 ticks measured against the TSC.
* `test=lifecycle cycles=N` — run the memory suite, reboot via ACPI until N
  boots are counted in CMOS, then power off via ACPI S5.
* `test=shutdown` — ACPI S5 power-off.
* `test=irq-alloc` — make the timer handler allocate (must be refused; fatal).
* `test=breakpoint|pagefault|null|guard|stack-overflow|nx|wp|ud|divide|panic|locked-fault`
  — deliberate fault tests.
* `x2apic` — switch the local APIC to x2APIC mode if the CPU supports it.
* `qemu-exit` — report results through QEMU's `isa-debug-exit` device
  (only with `-device isa-debug-exit,iobase=0xf4,iosize=0x04`).

## Writing to a USB stick (untested on hardware)
`target/coco/coco.img` is a GPT disk with one FAT32 ESP and can be written
raw to a disk. This has **not** been tried on a physical machine.

## Reproducibility
Artifacts depend only on the source tree and toolchain: paths are remapped
(`--remap-path-prefix`), the PE uses `/Brepro` and no PDB reference, FAT
images use a fixed serial and `SOURCE_DATE_EPOCH=1767225600`, and the GPT
uses fixed GUIDs.
