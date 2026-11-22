# ADR 0012 — Threads and preemptive scheduling

**Status:** accepted (M3)

* **Threads** have a 32 KiB kernel stack with an unmapped guard page, in
  the kernel-stack region (PML4 508) at fixed slots; the boot thread keeps
  the boot stack. Thread records are boxed (stable addresses).
* **Context switch** (`coco_switch_context`, assembly) saves callee-saved
  registers and `rsp`; the scheduler additionally swaps x87/SSE state with
  FXSAVE/FXRSTOR, reloads CR3 when the address space differs, and points
  TSS.RSP0 and the per-CPU syscall stack at the incoming thread.
* **Policy:** single run queue, round-robin, preemption on every 100 Hz
  timer tick from the interrupt path; `yield`, `sleep(ticks)` (woken by the
  tick) and `exit`. Kernel threads run on the kernel's own page tables, so
  a process's tables can be freed while kernel threads run.
* **No allocation on the tick path:** run queue, dead list and slot list
  are reserved to 1,024 entries at start; spawning beyond fails cleanly.
* **Reaping** is deferred to another thread (idle loop or `join`), never on
  the dying thread's own stack.
* **Kernel preemptibility:** code is preemptible except while holding an
  `IrqSpinLock` (interrupts are off while held). Lock order:
  `SCHED → HEAP → MEMORY → CONSOLE`; `PROCESSES` is only ever taken alone.

Future (later M3 increments / M10): per-CPU run queues, work stealing, CPU
affinity, latency classes and energy-aware policy, moved into a separate
resource-management service as the directive requires.
