# ADR 0009 — Kernel heap

**Status:** accepted (M2)

## Decision
* `coco_memory::heap` (host-tested, Miri-checked) is the allocator; the
  kernel wraps it in `#[global_allocator]` behind an IRQ-safe lock.
* Virtual window `0xFFFF_FD80_0000_0000` (PML4 507), 1 GiB, backed lazily
  in 64 KiB steps with **zeroed** frames mapped RW+NX. Growth is
  all-or-nothing: a partially mapped step is rolled back.
* Small requests (≤ 2048 B): 8 power-of-two classes with intrusive free
  lists. Large requests: first-fit page runs, address-ordered and always
  coalesced. Alignment up to 4096 B; larger alignments return null.
* Hardening, always on: exact double-free detection (magic + list check for
  small blocks, overlap check for runs), pointer validation, and poisoning
  of freed small blocks verified on reuse (detects writes after free).
  Heap corruption or an invalid free is a fatal invariant violation.
* Allocation in interrupt context is refused (null → `handle_alloc_error`)
  and reported.

## Consequences
+ Memory-safety defects in kernel code surface as precise, early failures.
− Heap memory is never returned to the frame allocator, and small-class
  pages are never returned to the run list: usage is bounded by the peak.
  A reclaiming design (per-CPU magazines, page release) is planned with M3.
− Poisoning costs a memset per small free and a scan per small allocation.
  The whole churn test runs at ~2.0–2.2 k cycles/op under TCG with
  poisoning on (revision `b60418a`); the share caused by poisoning alone
  has not been isolated yet.
