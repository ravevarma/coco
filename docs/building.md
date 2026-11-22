# Building and testing coco

## Toolchain (pinned)
* Rust **1.97.0** stable with targets `x86_64-unknown-uefi` and
  `x86_64-unknown-none` and components rustfmt, clippy, rust-src,
  llvm-tools — installed automatically by rustup from `rust-toolchain.toml`.
* Host tools: `mtools` (FAT image), `qemu-system-x86_64` (tested 8.2.2) and
  OVMF (`/usr/share/OVMF/OVMF_CODE_4M.fd`, `OVMF_VARS_4M.fd`; tested edk2
  2024.02), coreutils `sha256sum`.
  On Ubuntu 24.04: `apt-get install qemu-system-x86 ovmf mtools`.
* Optional: nightly Rust with Miri (`rustup toolchain install nightly
  --component miri`) and `cargo install cargo-fuzz`.

## Commands
| Command | What it does |
|---|---|
| `cargo xtask build` | release loader + kernel, `target/coco/esp/`, `esp.img`, GPT `coco.img`, size report (fails above 108,000,000 bytes) |
| `cargo xtask check` | rustfmt, clippy `-D warnings` (host + both bare-metal targets), tests (debug + release), rustdoc |
| `cargo xtask qemu-test [filter]` | QEMU/OVMF boot matrix; logs in `target/coco/qemu/<case>/` |
| `cargo xtask run [--cmdline "test=memory"] [--memory 512M] [--smp 2]` | interactive boot, serial on stdio |
| `cargo xtask repro` | builds from two copies of the tree at different paths and compares SHA-256 of every artifact |
| `rustup run nightly cargo miri test -p coco-abi -p coco-elf -p coco-memory -p coco-sync -p coco-arch-x86_64` | Miri |
| `RUSTFLAGS="--cfg loom" cargo test -p coco-sync --release --target-dir target/loom` | Loom |
| `cd fuzz && cargo +nightly fuzz run elf_parse -- -max_total_time=300` | fuzzing (`elf_parse`, `memory_map`, `boot_info`) |

## Kernel command line (`\coco\cmdline.txt` on the ESP)
* `test=memory` — in-kernel memory/paging/protection self-tests.
* `test=breakpoint|pagefault|null|guard|stack-overflow|nx|wp|ud|divide|panic|locked-fault`
  — deliberate fault tests.
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
