# ADR 0013 — Processes, system calls and user isolation

**Status:** accepted (M3)

* **Address spaces:** each process has its own PML4. The user half
  (slots 0–255) is private; the kernel half is a copy of the kernel's
  PML4 entries. All 256 kernel slots are pre-populated at scheduler start
  (≈1 MiB of level-3 tables) so every later kernel mapping is visible in
  every process without synchronisation.
* **Loading:** `coco-boot` passes `\coco\init.elf` to the kernel in
  BOOT_INFO memory (`BootInfo` v2). The kernel validates it with
  `coco-elf` under a user policy (window `0x40_0000..0x7FFF_FFFF_F000`,
  W^X, ≤ 64 MiB), maps segments page by page with `USER` and exactly their
  permissions, and maps a 64 KiB stack with an unmapped guard page below.
* **Entry:** `iretq` to ring 3 with every general-purpose register zeroed
  except the two entry arguments.
* **System calls:** `syscall`/`sysret` (ABI in `coco_abi::syscall`). The
  stub brackets its two per-CPU accesses with `swapgs` so the kernel never
  runs with a kernel GS base; FMASK clears IF, TF, DF and AC; all
  caller-saved registers are restored from the saved frame; RCX/RSP are
  validated before `sysret`. User pointers are checked for range, then for
  present+USER mappings in the caller's page tables, and copied inside a
  `stac`/`clac` window (SMAP stays on otherwise). Output is sanitised
  against terminal-escape injection.
* **Isolation:** any CPU exception raised at CPL 3 terminates only that
  process (reason, RIP, fault address and error code are logged).
* **Teardown:** `wait` joins the dead thread, then frees every user page,
  every user page-table frame and the PML4 (`teardown_slots`), without
  heap allocation under the memory lock.
* **SIMD state:** user x87/SSE state is saved per thread; XSAVE-managed
  state (AVX…) and FSGSBASE stay disabled until the save area covers them.
