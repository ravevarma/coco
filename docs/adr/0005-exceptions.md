# ADR 0005 — CPU exception handling and fatal-error policy

**Status:** accepted (M1)

* Entry stubs are assembly (`global_asm!`, stable Rust; no unstable
  `x86-interrupt` ABI). Every vector produces one `TrapFrame` layout and
  calls `trap_dispatch` with a 16-byte aligned stack.
* #DF, NMI and #MC use IST stacks (16 KiB each) so a kernel stack overflow
  is reported instead of triple-faulting. The handler recognises a fault
  address in the boot-stack guard page and names the cause.
* #BP and NMI are recoverable (logged, resumed). Every other exception is a
  kernel invariant violation: full register dump, CR2/CR3, decoded #PF
  error code, red banner on the framebuffer, then a controlled stop
  (QEMU: `isa-debug-exit` status 35; hardware: `cli; hlt` loop).
* Trap and panic paths never block on locks (rule L2): they use
  `try_lock` and fall back to the lock-free UART path.
