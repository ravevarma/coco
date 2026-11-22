# ADR 0004 — Physical frame allocator

**Status:** accepted (M1, first M2 component)

## Decision
* Bitmap allocator (`coco_memory::frame`) with two bitmaps: `managed`
  (frames ever handed to it) and `free`. Invariant `free ⊆ managed`;
  counters always equal bitmap popcounts (`check_invariants`).
* Ownership is a move-only `PhysFrame` token (not `Copy`/`Clone`,
  `#[must_use]`). Freeing consumes the token; forging one requires
  `unsafe PhysFrame::from_raw`. Double frees and frees of unmanaged frames
  are still detected at runtime and reported as errors.
* Bitmap storage is carved from the first suitable Usable region and
  excluded from the free set. Frames below 1 MiB are never allocated
  (legacy structures, future SMP trampoline).
* Loader memory (`EfiLoaderCode/Data`) and boot-services memory are
  reclaimed; kernel image, boot stack, page tables and boot info are not.

## Why a bitmap
2 bits per 4 KiB frame (0.006% overhead, 384 KiB for 4 GiB), O(1)
amortised allocation via a word hint, trivially verifiable invariants.
Per-CPU caches and contiguous/DMA allocation are planned (M2/M5) on top.
