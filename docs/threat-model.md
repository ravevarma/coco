# Threat model (M1 scope)

## Assets
Kernel integrity (code, page tables, descriptor tables), confidentiality of
future user data, and availability of the boot path.

## Trust boundaries in M1
1. **Firmware → loader.** UEFI firmware and its memory map are trusted for
   correctness of the platform but **not** for well-formedness: memory
   descriptors are bounds- and overlap-checked (`normalize` rejects
   overlapping maps rather than guessing).
2. **Boot volume → loader.** `kernel.elf` and `cmdline.txt` are treated as
   untrusted input: size caps (32 MiB kernel file, 1 KiB cmdline), strict
   ELF validation (no dynamic linking/TLS/interp, window-bounded,
   W^X, no page sharing, entry in an executable segment), printable-ASCII
   cmdline. Tested by fuzzing and by QEMU rejection tests.
   **Limitation:** there is no signature verification yet, so anyone able to
   write the ESP can replace the kernel. Secure Boot / measured boot are
   future work and are not claimed.
3. **Loader → kernel.** The kernel copies and validates `BootInfo` before
   dereferencing any pointer in it.

## Mitigations in place (verified)
| Threat | Mitigation | Test |
|---|---|---|
| Code injection into data/stack | NX on data, rodata, stack and direct map | `fault-nx`, `protections` self-test |
| Kernel code/rodata modification | read-only text/rodata + CR0.WP | `fault-write-protect` |
| Null/low pointer dereference | lower half unmapped after boot | `fault-null` |
| Stack overflow corrupting memory | unmapped guard page + IST #DF stack | `fault-guard-page`, `fault-stack-overflow` |
| Malformed kernel image | `coco-elf` validation | fuzzing, `reject-truncated-kernel`, host tests |
| Malformed command line | ASCII validation + length cap | `reject-bad-cmdline`, fuzzing |
| Integer overflow in parsers/allocators | checked arithmetic enforced by `clippy::arithmetic_side_effects`; release builds keep `overflow-checks = true` | lint gate |
| Supply-chain compromise | zero third-party crates in shipped binaries; reproducible builds | `cargo tree`, `cargo xtask repro` |
| Kernel heap misuse (double free, use-after-free write, invalid free) | exact double-free detection, poisoning, pointer validation; fatal stop | host tests, Miri |
| Allocation from interrupt context (deadlock/corruption risk) | refused and reported | `irq-alloc-refused` |
| Malformed ACPI tables | checksums enforced, bounded lengths/lists, checked arithmetic, no `unsafe` | host tests, 31 M fuzz runs |
| Future user-space access/exec from kernel | SMEP, SMAP, UMIP enabled when supported | `protections` self-test |
| IST stack overflow corrupting neighbours | guard pages | `protections` self-test |
| User process reading/writing kernel memory | separate page tables, no USER bit on kernel pages, SMEP/SMAP | `user-isolation` (kernel read killed) |
| User process crashing the kernel | CPL-3 exceptions kill only the process | 6 fault classes in `user-isolation` |
| Malicious syscall arguments | range + mapping checks, copies inside STAC/CLAC, bounded lengths, unknown numbers rejected | 2 fuzz processes, ≈18.4 k random hostile syscalls each |
| Kernel data leaking to user space | registers zeroed on entry, restored on syscall return, x87/SSE state saved per thread, AVX disabled, fresh frames zeroed | code review (`docs/adr/0013`) |
| SYSRET non-canonical RCX attack | RCX validated before `sysret` | code review |
| User code disabling SMAP for the kernel (RFLAGS.AC) | FMASK clears AC on syscall entry | code review |
| Using a capability without the right | rights checked on every call; duplicate only attenuates | 21 user-mode checks |
| Forged or stale handle values | generation-checked values; zero invalid | host model test, fuzzing, user checks |
| Kernel-memory exhaustion through IPC | bounded messages/queues/handles, 4,096-channel limit, cycle refusal (no leaks) | `ipc-capabilities`, fuzzing |
| Peer hangs after a crash | handles closed on exit/kill → `PeerClosed` | kill-cleanup test |

## Not yet addressed (by design of the milestone order)
Speculative-execution attacks (Meltdown: no KPTI; Spectre: no
mitigations), per-process resource quotas, IOMMU/DMA isolation,
KASLR, stack canaries, secure randomness, resource quotas per process,
signed updates and rollback protection. None of these are claimed.
