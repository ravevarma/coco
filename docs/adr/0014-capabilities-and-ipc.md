# ADR 0014 — Capabilities and channel IPC

**Status:** accepted (M4)

## Capabilities
* Every process owns a handle table (`coco-capability`, ≤ 256 handles). A
  handle value encodes `generation << 32 | slot + 1`; the generation
  changes when a slot is freed, so closed or forged values are rejected
  even after the slot is reused. Zero is never valid.
* Each handle carries rights: READ, WRITE, TRANSFER, DUPLICATE. Every
  syscall states the rights it needs. `HANDLE_DUPLICATE` requires
  DUPLICATE and can only **remove** rights (attenuation); undefined bits
  are rejected.
* A process has no ambient authority over channels: it can only use
  handles it was given at spawn, created itself, or received.

## Channels
* Bidirectional, two endpoints, bounded: 256-byte messages, 4 handles per
  message, 64 queued messages per direction, 4 waiters per side, 4,096
  channels system-wide. Limits return errors (`TooLarge`, `QueueFull`,
  `NoResources`), never block or allocate without bound.
* `CHANNEL_SEND` moves capabilities: every transferred handle needs
  TRANSFER, duplicates in one message are rejected, and handles leave the
  sender's table **only if the send succeeds**.
* `CHANNEL_RECV` is non-blocking and checks the destination buffers before
  taking a message, so delivery cannot fail half-way; a message that does
  not fit (or a full handle table) stays queued.
* `WAIT_READABLE(handle, timeout)` blocks until a message arrives or the
  peer closes (timeout 0 = forever). Single-CPU lost-wake-up freedom comes
  from checking readiness and blocking with interrupts disabled.
* Closing the last reference to a side (including process exit or kill)
  wakes the peer, which then sees `PeerClosed` after draining.
* **No reference cycles:** a send that would make a channel reachable from
  itself through in-flight endpoints is refused (`InvalidArgument`). The
  in-flight graph therefore stays acyclic, and every channel is released
  when its endpoints close. Releasing long chains is iterative (a
  worklist), never recursive.
* State machine in `coco-ipc` (host-tested, fuzzed, Miri-checked); the
  kernel adds locking, reference counting and waking.

## Consequences
+ Least privilege is expressible (read-only or non-transferable handles)
  and enforced on every call; the first user-space service (echo) runs
  with exactly one handle.
− Round trips cost two blocking waits and two address-space switches
  (≈87–104 k cycles under TCG); a direct hand-off fast path and PCIDs are
  future work.
− The cycle check walks reachable in-flight endpoints on sends that carry
  handles (bounded by the channel count).
