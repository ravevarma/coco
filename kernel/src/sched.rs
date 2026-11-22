//! Preemptive round-robin scheduler for kernel and user threads.
//!
//! * Every thread has its own kernel stack (32 KiB, guard page below) in
//!   the kernel-stack region; the boot thread keeps the boot stack.
//! * Context switches save the callee-saved registers and `rsp` (assembly
//!   below), the x87/SSE state (FXSAVE), and switch CR3 when the address
//!   space differs. Kernel threads run on the kernel's own page tables.
//! * The APIC timer preempts the running thread on every tick
//!   (`cpu::trap_dispatch` → [`preempt`]).
//! * Nothing on the tick/preemption path allocates: queues are reserved to
//!   [`MAX_THREADS`] when the scheduler starts.
//! * Dead threads are reaped by another thread (never on their own stack).
//!
//! Lock order (rule L3): `SCHED` → `HEAP` → `MEMORY` → `CONSOLE`.
//! `SCHED` is an `IrqSpinLock` and is never held across a context switch.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use coco_abi::layout::{BOOT_STACK_TOP, KERNEL_STACK_REGION, KERNEL_THREAD_STACK_SIZE, PAGE_SIZE};
use coco_arch::cpu::{self as arch, FxArea};

use crate::process::Pid;
use crate::sync::IrqSpinLock;
use crate::{cpu, mm};

/// Thread identifier.
pub type Tid = u64;
/// Most threads that can exist at once.
pub const MAX_THREADS: usize = 1024;
/// First thread-stack slot (the boot stack lives at the region base).
const THREAD_STACKS_BASE: u64 = KERNEL_STACK_REGION + (1 << 30);
const SLOT_STRIDE: u64 = KERNEL_THREAD_STACK_SIZE + PAGE_SIZE;

/// Scheduling state of a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Waiting in the run queue.
    Ready,
    /// On the CPU.
    Running,
    /// Blocked until the given tick.
    Sleeping(u64),
    /// Finished; awaiting reaping.
    Dead,
    /// A reaper is freeing the thread's resources; the record disappears
    /// only once everything is released, so `join` cannot return early.
    Reaping,
}

/// Scheduler errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedError {
    /// [`MAX_THREADS`] reached.
    TooManyThreads,
    /// No memory for the stack or bookkeeping.
    OutOfMemory,
    /// The scheduler has not been started.
    NotRunning,
    /// `init` was called twice.
    AlreadyRunning,
}

struct Thread {
    name: &'static str,
    state: State,
    /// Saved stack pointer while not running.
    rsp: u64,
    stack_top: u64,
    /// Stack slot (None for the boot thread).
    slot: Option<u32>,
    fx: Box<FxArea>,
    process: Option<Pid>,
    cr3: u64,
}

struct Scheduler {
    threads: BTreeMap<Tid, Box<Thread>>,
    ready: VecDeque<Tid>,
    dead: Vec<Tid>,
    free_slots: Vec<u32>,
    next_slot: u32,
    current: Tid,
    idle: Tid,
    next_tid: Tid,
    kernel_cr3: u64,
    switches: u64,
    /// Clean x87/SSE image (after `fninit`) given to new threads.
    default_fx: Box<FxArea>,
}

static SCHED: IrqSpinLock<Option<Scheduler>> = IrqSpinLock::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static SWITCHES: AtomicU64 = AtomicU64::new(0);

// switch_context(old_rsp: *mut u64 [rdi], new_rsp: u64 [rsi])
// thread_start: entered via `ret` on a fresh stack; r12 = entry, r13 = arg.
core::arch::global_asm!(
    r#"
.section .text
.global coco_switch_context
coco_switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.global coco_thread_start
coco_thread_start:
    mov rdi, r13
    call r12
    ud2
"#
);

unsafe extern "C" {
    fn coco_switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn coco_thread_start();
}

/// Starts the scheduler: the caller becomes thread 0 ("kernel-main") and
/// an idle thread is created.
///
/// # Errors
/// [`SchedError::OutOfMemory`] if bookkeeping cannot be reserved.
pub fn init() -> Result<(), SchedError> {
    if RUNNING.load(Ordering::Acquire) || SCHED.lock().is_some() {
        return Err(SchedError::AlreadyRunning);
    }
    let mut threads = BTreeMap::new();
    let mut ready = VecDeque::new();
    let mut dead = Vec::new();
    let mut free_slots = Vec::new();
    ready.try_reserve_exact(MAX_THREADS).map_err(|_| SchedError::OutOfMemory)?;
    dead.try_reserve_exact(MAX_THREADS).map_err(|_| SchedError::OutOfMemory)?;
    free_slots.try_reserve_exact(MAX_THREADS).map_err(|_| SchedError::OutOfMemory)?;
    arch::fninit();
    let mut default_fx = Box::new(FxArea::ZERO);
    // SAFETY: CR4.OSFXSR is set and CR0.EM clear (cpu::init).
    unsafe { arch::fxsave(&mut default_fx) };
    let fx = default_fx.clone();
    let kernel_cr3 = arch::read_cr3();
    threads.insert(
        0,
        Box::new(Thread {
            name: "kernel-main",
            state: State::Running,
            rsp: 0,
            stack_top: BOOT_STACK_TOP,
            slot: None,
            fx,
            process: None,
            cr3: kernel_cr3,
        }),
    );
    *SCHED.lock() = Some(Scheduler {
        threads,
        ready,
        dead,
        free_slots,
        next_slot: 0,
        current: 0,
        idle: 0,
        next_tid: 1,
        kernel_cr3,
        switches: 0,
        default_fx,
    });
    cpu::set_kernel_stack(BOOT_STACK_TOP);
    // The idle thread is never queued: it runs only when nothing is ready.
    let idle = spawn_raw("idle", idle_main, 0, None, false)?;
    if let Some(s) = SCHED.lock().as_mut() {
        s.idle = idle;
    }
    RUNNING.store(true, Ordering::Release);
    Ok(())
}

extern "C" fn idle_main(_: u64) -> ! {
    loop {
        reap();
        // SAFETY: interrupts are fully configured; no lock is held.
        unsafe { arch::enable_interrupts_and_halt() };
    }
}

fn slot_base(slot: u32) -> u64 {
    THREAD_STACKS_BASE + u64::from(slot) * SLOT_STRIDE
}

/// Creates a thread that starts at `entry(arg)` with interrupts disabled.
fn spawn_raw(
    name: &'static str,
    entry: extern "C" fn(u64) -> !,
    arg: u64,
    process: Option<(Pid, u64)>,
    runnable: bool,
) -> Result<Tid, SchedError> {
    let mut fx = Box::new(FxArea::ZERO);
    let slot = {
        let mut g = SCHED.lock();
        let s = g.as_mut().ok_or(SchedError::NotRunning)?;
        if s.threads.len() >= MAX_THREADS {
            return Err(SchedError::TooManyThreads);
        }
        *fx = *s.default_fx;
        match s.free_slots.pop() {
            Some(slot) => slot,
            None => {
                let slot = s.next_slot;
                s.next_slot += 1;
                slot
            }
        }
    };
    let stack_bottom = slot_base(slot) + PAGE_SIZE; // guard page at slot_base
    let stack_top = stack_bottom + KERNEL_THREAD_STACK_SIZE;
    if !mm::with(|m| m.map_fresh(stack_bottom, KERNEL_THREAD_STACK_SIZE)).unwrap_or(false) {
        if let Some(s) = SCHED.lock().as_mut() {
            s.free_slots.push(slot);
        }
        return Err(SchedError::OutOfMemory);
    }
    // Initial frame consumed by coco_switch_context's pops + ret:
    // [r15, r14, r13=arg, r12=entry, rbx, rbp, ret=thread_start, pad, pad]
    let rsp = stack_top - 72;
    let words = [0, 0, arg, entry as *const () as u64, 0, 0, coco_thread_start as *const () as u64, 0, 0];
    for (i, w) in words.iter().enumerate() {
        // SAFETY: `[stack_top - 72, stack_top)` lies in the stack just mapped
        // (fresh, zeroed, owned by this new thread).
        unsafe { ((rsp + 8 * i as u64) as *mut u64).write(*w) };
    }
    let mut g = SCHED.lock();
    let s = g.as_mut().ok_or(SchedError::NotRunning)?;
    let tid = s.next_tid;
    s.next_tid += 1;
    let (pid, cr3) = match process {
        Some((pid, cr3)) => (Some(pid), cr3),
        None => (None, s.kernel_cr3),
    };
    s.threads.insert(
        tid,
        Box::new(Thread { name, state: State::Ready, rsp, stack_top, slot: Some(slot), fx, process: pid, cr3 }),
    );
    if runnable {
        s.ready.push_back(tid); // capacity reserved: no allocation
    }
    Ok(tid)
}

/// Boxed kernel-thread body.
type Body = Box<dyn FnOnce() + Send>;

extern "C" fn kernel_thread_main(arg: u64) -> ! {
    // SAFETY: `arg` is the pointer produced by `Box::into_raw` in
    // `spawn_kernel`, consumed exactly once here.
    let body = unsafe { Box::from_raw(arg as *mut Body) };
    // SAFETY: the scheduler is running; this thread holds no locks.
    unsafe { arch::enable_interrupts() };
    body();
    exit_current()
}

/// Spawns a kernel thread running `f`.
///
/// # Errors
/// See [`SchedError`].
pub fn spawn_kernel(name: &'static str, f: impl FnOnce() + Send + 'static) -> Result<Tid, SchedError> {
    let body: Box<Body> = Box::new(Box::new(f));
    let raw = Box::into_raw(body);
    spawn_raw(name, kernel_thread_main, raw as u64, None, true).inspect_err(|_| {
        // SAFETY: the thread was not created, so ownership is still ours.
        drop(unsafe { Box::from_raw(raw) });
    })
}

/// Creates (but does not start) a thread belonging to process `pid`
/// (address space `cr3`). The caller records the thread id and then calls
/// [`start`], so no other code can observe the process before its thread is
/// known.
///
/// # Errors
/// See [`SchedError`].
pub fn spawn_user(name: &'static str, entry: extern "C" fn(u64) -> !, pid: Pid, cr3: u64) -> Result<Tid, SchedError> {
    spawn_raw(name, entry, pid, Some((pid, cr3)), false)
}

/// Makes a thread created by [`spawn_user`] runnable. Idempotent.
pub fn start(tid: Tid) {
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return };
    if s.threads.get(&tid).is_some_and(|t| t.state == State::Ready) && !s.ready.contains(&tid) {
        s.ready.push_back(tid); // capacity reserved: no allocation
    }
}

/// Destroys a thread created by [`spawn_user`] that was never started.
pub fn discard_unstarted(tid: Tid) {
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return };
    if s.threads.get(&tid).is_some_and(|t| t.state == State::Ready) && !s.ready.contains(&tid) {
        if let Some(t) = s.threads.get_mut(&tid) {
            t.state = State::Dead;
        }
        s.dead.push(tid);
    }
}

/// Switches away from the current thread, leaving it in `mark`.
/// Must be called with interrupts disabled and no locks held.
fn switch_away(mark: State) {
    let plan = {
        let mut g = SCHED.lock();
        let Some(s) = g.as_mut() else { return };
        let cur_id = s.current;
        let idle = s.idle;
        if let Some(cur) = s.threads.get_mut(&cur_id) {
            cur.state = mark;
        }
        if mark == State::Ready && cur_id != idle {
            s.ready.push_back(cur_id);
        }
        if mark == State::Dead {
            s.dead.push(cur_id); // capacity reserved: no allocation
        }
        let mut next_id = idle;
        while let Some(t) = s.ready.pop_front() {
            if s.threads.get(&t).is_some_and(|t| t.state == State::Ready) {
                next_id = t;
                break;
            }
        }
        if next_id == cur_id {
            if let Some(cur) = s.threads.get_mut(&cur_id) {
                cur.state = State::Running;
            }
            None
        } else {
            s.current = next_id;
            s.switches += 1;
            let (new_rsp, new_top, new_cr3, new_fx) = match s.threads.get_mut(&next_id) {
                Some(n) => {
                    n.state = State::Running;
                    (n.rsp, n.stack_top, n.cr3, core::ptr::addr_of!(*n.fx))
                }
                None => return,
            };
            match s.threads.get_mut(&cur_id) {
                Some(c) => Some((
                    core::ptr::addr_of_mut!(c.rsp),
                    core::ptr::addr_of_mut!(*c.fx),
                    new_rsp,
                    new_top,
                    new_cr3,
                    new_fx,
                )),
                None => None,
            }
        }
    };
    let Some((old_rsp, old_fx, new_rsp, new_top, new_cr3, new_fx)) = plan else { return };
    SWITCHES.fetch_add(1, Ordering::Relaxed);
    cpu::set_kernel_stack(new_top);
    if arch::read_cr3() != new_cr3 {
        // SAFETY: `new_cr3` is either the kernel's PML4 or a process PML4
        // whose kernel half is a copy of the kernel's (shared tables), so
        // this code, the current stack and all kernel data stay mapped.
        unsafe { arch::write_cr3(new_cr3) };
    }
    // SAFETY: both FX areas are boxed in live Thread records. Interrupts
    // are disabled on the only CPU, so no other code can reap or touch the
    // threads (or the pointers derived above) until the switch completes.
    let old_fx = unsafe { &mut *old_fx };
    // SAFETY: as above.
    let new_fx = unsafe { &*new_fx };
    // SAFETY: CR4.OSFXSR set, CR0.EM clear (cpu::init).
    unsafe { arch::fxsave(old_fx) };
    // SAFETY: as above; `new_fx` holds an image produced by fxsave.
    unsafe { arch::fxrstor(new_fx) };
    // SAFETY: `old_rsp` points into the current thread's live record;
    // `new_rsp` is the saved stack pointer of a thread that is not running
    // (or a fresh stack prepared by `spawn_raw`).
    unsafe { coco_switch_context(old_rsp, new_rsp) };
}

/// Timer-tick preemption point (interrupts disabled, no locks held).
pub fn preempt() {
    if RUNNING.load(Ordering::Acquire) {
        switch_away(State::Ready);
    }
}

/// Called from the timer interrupt: wakes sleeping threads whose deadline
/// passed. Allocation-free.
pub fn tick(now: u64) {
    if !RUNNING.load(Ordering::Acquire) {
        return;
    }
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return };
    let Scheduler { threads, ready, .. } = s;
    for (&tid, t) in threads.iter_mut() {
        if let State::Sleeping(until) = t.state
            && until <= now
        {
            t.state = State::Ready;
            ready.push_back(tid);
        }
    }
}

/// Gives up the CPU to the next ready thread.
pub fn yield_now() {
    let enabled = arch::interrupts_enabled();
    arch::disable_interrupts();
    switch_away(State::Ready);
    if enabled {
        // SAFETY: restores the caller's interrupt state; no lock is held.
        unsafe { arch::enable_interrupts() };
    }
}

/// Blocks the current thread for at least `ticks` timer ticks.
pub fn sleep(ticks: u64) {
    let enabled = arch::interrupts_enabled();
    arch::disable_interrupts();
    let until = crate::interrupts::ticks() + ticks.max(1);
    switch_away(State::Sleeping(until));
    if enabled {
        // SAFETY: as in `yield_now`.
        unsafe { arch::enable_interrupts() };
    }
}

/// Terminates the current thread.
pub fn exit_current() -> ! {
    arch::disable_interrupts();
    switch_away(State::Dead);
    // A dead thread is never scheduled again.
    crate::fatal_stop()
}

/// Frees the stacks and records of dead threads (never the current one).
/// Must not be called from interrupt context.
///
/// Several threads may reap concurrently (the idle thread, `join` callers).
/// Each dead thread is claimed by exactly one reaper (removed from the dead
/// list and marked [`State::Reaping`] under `SCHED`); its record is removed
/// and dropped inside one `SCHED` critical section after the stack is
/// freed, so [`state`] reports the thread until every resource is released.
pub fn reap() {
    loop {
        let (tid, slot) = {
            let mut g = SCHED.lock();
            let Some(s) = g.as_mut() else { return };
            let current = s.current;
            let Some(pos) = s.dead.iter().position(|&t| t != current) else { return };
            let tid = s.dead.swap_remove(pos);
            let Some(t) = s.threads.get_mut(&tid) else { continue };
            t.state = State::Reaping;
            (tid, t.slot.take())
        };
        if let Some(slot) = slot {
            let bottom = slot_base(slot) + PAGE_SIZE;
            mm::with(|m| m.unmap_and_free(bottom, KERNEL_THREAD_STACK_SIZE));
        }
        let mut g = SCHED.lock();
        if let Some(s) = g.as_mut() {
            if let Some(slot) = slot {
                s.free_slots.push(slot); // capacity reserved: no allocation
            }
            // Dropped under SCHED (lock order SCHED -> HEAP allows it) so
            // the record vanishes atomically with its memory.
            drop(s.threads.remove(&tid));
        }
    }
}

/// The current thread's id.
pub fn current() -> Tid {
    SCHED.lock().as_ref().map_or(0, |s| s.current)
}

/// The process of the current thread, if any.
pub fn current_process() -> Option<Pid> {
    SCHED.lock().as_ref().and_then(|s| s.threads.get(&s.current).and_then(|t| t.process))
}

/// State of thread `tid` (`None` once reaped).
pub fn state(tid: Tid) -> Option<State> {
    SCHED.lock().as_ref().and_then(|s| s.threads.get(&tid).map(|t| t.state))
}

/// `(live threads, context switches so far)`.
pub fn stats() -> (usize, u64) {
    let threads = SCHED.lock().as_ref().map_or(0, |s| s.threads.len());
    (threads, SWITCHES.load(Ordering::Relaxed))
}

/// Name of thread `tid`.
pub fn name(tid: Tid) -> Option<&'static str> {
    SCHED.lock().as_ref().and_then(|s| s.threads.get(&tid).map(|t| t.name))
}

/// Waits (sleeping) until `tid` has finished and been reaped.
pub fn join(tid: Tid) {
    while state(tid).is_some() {
        reap();
        if state(tid).is_some() {
            sleep(1);
        }
    }
}

/// Diagnostic view of the scheduler.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    /// Live thread records.
    pub threads: usize,
    /// Ready-queue length.
    pub ready: usize,
    /// Dead threads awaiting reaping.
    pub dead: usize,
    /// Running thread.
    pub current: Tid,
    /// Up to 8 live `(tid, name, state)` entries.
    pub live: [(Tid, &'static str, State); 8],
}

/// Takes a diagnostic snapshot of the scheduler.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot { threads: 0, ready: 0, dead: 0, current: 0, live: [(0, "", State::Dead); 8] };
    let g = SCHED.lock();
    let Some(s) = g.as_ref() else { return snap };
    for (slot, (&tid, t)) in snap.live.iter_mut().zip(s.threads.iter()) {
        *slot = (tid, t.name, t.state);
    }
    snap.threads = s.threads.len();
    snap.ready = s.ready.len();
    snap.dead = s.dead.len();
    snap.current = s.current;
    snap
}
