//! Kernel objects reachable through capabilities, and channel system calls.
//!
//! * [`KObject`] is what a handle names (currently only channel endpoints).
//! * A channel's state machine is `coco_ipc::Channel` (host-tested) behind
//!   an `IrqSpinLock`; this module adds reference counting, waking of
//!   blocked threads and the syscall surface.
//! * Closing the last reference to an endpoint drains its queue. Drained
//!   messages may hold endpoints of other channels; they are released
//!   iteratively through a worklist ("graveyard") rather than recursively,
//!   so arbitrarily long chains cannot overflow the kernel stack.
//!
//! Lock order (L3): `PROCESSES` → `CHANNEL` → `GRAVEYARD` → `SCHED` →
//! `HEAP` → `MEMORY` → `CONSOLE`.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use coco_abi::syscall::{Error, MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES, rights};
use coco_ipc::{Channel, Message, Wake};

use crate::sync::IrqSpinLock;
use crate::{process, sched, syscall};

/// Most channels alive at once (bounds kernel memory used by IPC).
pub const MAX_CHANNELS: usize = 4096;
static LIVE_CHANNELS: AtomicUsize = AtomicUsize::new(0);

/// A capability in flight inside a message: the object and its rights.
pub struct Transferred {
    object: KObject,
    rights: u64,
}

struct Shared {
    state: IrqSpinLock<Channel<Transferred>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        LIVE_CHANNELS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A reference to one side of a channel. Cloning opens another reference
/// to that side; dropping the last one closes it.
pub struct ChannelRef {
    shared: Arc<Shared>,
    side: usize,
}

impl Clone for ChannelRef {
    fn clone(&self) -> Self {
        self.shared.state.lock().opened(self.side);
        Self { shared: Arc::clone(&self.shared), side: self.side }
    }
}

impl Drop for ChannelRef {
    fn drop(&mut self) {
        let closed = self.shared.state.lock().closed(self.side);
        if let Some((drained, wake)) = closed {
            wake_all(&wake);
            bury(drained);
        }
    }
}

/// A kernel object named by a handle.
#[derive(Clone)]
pub enum KObject {
    /// One endpoint of a channel.
    Channel(ChannelRef),
}

impl KObject {
    fn channel(&self) -> &ChannelRef {
        match self {
            Self::Channel(c) => c,
        }
    }
}

static GRAVEYARD: IrqSpinLock<VecDeque<Message<Transferred>>> = IrqSpinLock::new(VecDeque::new());
static BURYING: AtomicBool = AtomicBool::new(false);

/// Releases drained messages without recursion: the outermost caller loops
/// over a worklist; nested closes (triggered while dropping a message) only
/// append to it. A drainer preempted mid-loop finishes messages appended by
/// other threads when it resumes.
fn bury(messages: VecDeque<Message<Transferred>>) {
    if !messages.is_empty() {
        let mut g = GRAVEYARD.lock();
        if g.try_reserve(messages.len()).is_err() {
            // No memory for the worklist: drop here instead (recursion
            // depth then bounded by the stack; reported).
            drop(g);
            crate::kprintln_emergency!("ipc: graveyard allocation failed; dropping inline");
            drop(messages);
            return;
        }
        g.extend(messages);
    }
    loop {
        if BURYING.swap(true, Ordering::AcqRel) {
            return; // another frame or thread is draining
        }
        loop {
            // The guard is released before the message is dropped.
            let next = GRAVEYARD.lock().pop_front();
            match next {
                Some(m) => drop(m),
                None => break,
            }
        }
        BURYING.store(false, Ordering::Release);
        // Something appended between the last pop and clearing the flag?
        if GRAVEYARD.lock().is_empty() {
            return;
        }
    }
}

fn wake_all(wake: &Wake) {
    for tid in wake.iter().flatten() {
        sched::wake(*tid);
    }
}

/// Number of live channels.
pub fn live_channels() -> usize {
    LIVE_CHANNELS.load(Ordering::Relaxed)
}

/// Creates a channel and returns its two endpoints.
///
/// # Errors
/// [`Error::NoResources`] when [`MAX_CHANNELS`] exist.
pub fn create_pair() -> Result<(KObject, KObject), Error> {
    if LIVE_CHANNELS.fetch_add(1, Ordering::AcqRel) >= MAX_CHANNELS {
        LIVE_CHANNELS.fetch_sub(1, Ordering::AcqRel);
        return Err(Error::NoResources);
    }
    // Channel::new() starts each side with one open reference, owned by
    // the two ChannelRefs created here.
    let shared = Arc::new(Shared { state: IrqSpinLock::new(Channel::new()) });
    let a = ChannelRef { shared: Arc::clone(&shared), side: 0 };
    let b = ChannelRef { shared, side: 1 };
    Ok((KObject::Channel(a), KObject::Channel(b)))
}

/// Serialises the acyclicity check with the enqueue for every send that
/// carries capabilities (see [`enqueue`]).
static GRAPH: IrqSpinLock<()> = IrqSpinLock::new(());

/// Most in-flight endpoints the cycle check may examine per send. The check
/// runs with interrupts disabled, so its worst case must be small and fixed;
/// a send whose reachability cannot be decided within the budget is refused
/// with `NoResources` (conservative: it never admits a cycle).
const REACH_BUDGET: usize = 4096;

/// True if channel `from` can reach channel `target` through endpoints in
/// flight: depth-first search with a fixed work budget.
///
/// No visited set is kept: the in-flight graph is acyclic (invariant I3),
/// so the search terminates, and the budget bounds it even if shared
/// sub-graphs are walked more than once. Every allocation is fallible, so
/// heap exhaustion refuses the send (`NoResources`) instead of panicking
/// with interrupts disabled. Arcs on the stack are dropped after the
/// channel lock guard (reverse declaration order).
fn reaches(from: &Arc<Shared>, target: &Arc<Shared>) -> Result<bool, Error> {
    let mut budget = REACH_BUDGET;
    let mut stack: Vec<Arc<Shared>> = Vec::new();
    stack.try_reserve(8).map_err(|_| Error::NoResources)?;
    stack.push(Arc::clone(from));
    while let Some(c) = stack.pop() {
        if Arc::ptr_eq(&c, target) {
            return Ok(true);
        }
        budget = budget.checked_sub(1).ok_or(Error::NoResources)?;
        let state = c.state.lock();
        for t in state.in_flight() {
            budget = budget.checked_sub(1).ok_or(Error::NoResources)?;
            stack.try_reserve(1).map_err(|_| Error::NoResources)?;
            stack.push(Arc::clone(&t.object.channel().shared));
        }
    }
    Ok(false)
}

/// Enqueues a message at `target`'s peer. A capability graph edge
/// "`target`'s channel holds an endpoint of C" is only added if C cannot
/// already reach `target`'s channel, so the in-flight graph stays acyclic
/// and every channel is eventually released when its endpoints close.
fn enqueue(target: &ChannelRef, data: &[u8], moving: Vec<Transferred>) -> Result<(), Error> {
    let _graph = (!moving.is_empty()).then(|| GRAPH.lock());
    for t in &moving {
        if reaches(&t.object.channel().shared, &target.shared)? {
            drop(_graph);
            drop(moving);
            return Err(Error::InvalidArgument); // would create a reference cycle
        }
    }
    let result = target.shared.state.lock().send(target.side, data, moving);
    drop(_graph);
    match result {
        Ok(wake) => {
            wake_all(&wake);
            Ok(())
        }
        Err((e, back)) => {
            drop(back);
            Err(e)
        }
    }
}

/// Sends a message from kernel code through endpoint `from` (tests and
/// bootstrap). The transferred objects are moved into the message.
///
/// # Errors
/// As for `CHANNEL_SEND`.
pub fn kernel_send(from: &KObject, data: &[u8], handles: Vec<(KObject, u64)>) -> Result<(), Error> {
    let moving = handles.into_iter().map(|(object, rights)| Transferred { object, rights }).collect();
    enqueue(from.channel(), data, moving)
}

fn same_channel(a: &KObject, b: &KObject) -> bool {
    Arc::ptr_eq(&a.channel().shared, &b.channel().shared)
}

// ---------------------------------------------------------------- syscalls

/// Runs `f` on the caller's handle table; `BadHandle` if the caller is not
/// a process.
fn with_table<R>(f: impl FnOnce(&mut coco_capability::HandleTable<KObject>) -> R) -> Result<R, Error> {
    process::with_handles(f).ok_or(Error::BadHandle)
}

pub fn sys_channel_create(out: u64) -> Result<u64, Error> {
    syscall::check_user_writable(out, 16)?;
    let (a, b) = create_pair()?;
    let values = with_table(|t| {
        let ha = t.insert(a, rights::ALL).map_err(|(e, _)| e)?;
        match t.insert(b, rights::ALL) {
            Ok(hb) => Ok([ha, hb]),
            Err((e, _)) => {
                let _ = t.remove(ha);
                Err(e)
            }
        }
    })??;
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&values[0].to_le_bytes());
    bytes[8..].copy_from_slice(&values[1].to_le_bytes());
    syscall::copy_to_user(out, &bytes)?;
    Ok(0)
}

pub fn sys_channel_send(h: u64, data_ptr: u64, len: u64, handles_ptr: u64, count: u64) -> Result<u64, Error> {
    if len > MAX_MESSAGE_BYTES || count > MAX_MESSAGE_HANDLES {
        return Err(Error::TooLarge);
    }
    let mut data = [0u8; MAX_MESSAGE_BYTES as usize];
    let data = data.get_mut(..len as usize).ok_or(Error::TooLarge)?;
    syscall::copy_from_user(data, data_ptr)?;
    let mut raw = [0u8; 8 * MAX_MESSAGE_HANDLES as usize];
    let raw = raw.get_mut(..8 * count as usize).ok_or(Error::TooLarge)?;
    syscall::copy_from_user(raw, handles_ptr)?;
    let values: Vec<u64> = raw.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect();
    for (i, v) in values.iter().enumerate() {
        if values.get(..i).is_some_and(|prev| prev.contains(v)) {
            return Err(Error::InvalidArgument);
        }
    }
    // Validate and clone everything first; nothing is moved unless the send
    // succeeds.
    let (target, transferred) = with_table(|t| -> Result<_, Error> {
        let (target, _) = t.get(h, rights::WRITE)?;
        let target = target.clone();
        let mut moving = Vec::new();
        for &v in &values {
            let (obj, r) = t.get(v, rights::TRANSFER)?;
            if same_channel(obj, &target) {
                return Err(Error::InvalidArgument); // no endpoint through its own channel
            }
            moving.push(Transferred { object: obj.clone(), rights: r });
        }
        Ok((target, moving))
    })??;
    enqueue(target.channel(), data, transferred)?;
    // Complete the move: the sender loses the transferred handles.
    let removed = with_table(|t| values.iter().filter_map(|&v| t.remove(v).ok()).collect::<Vec<_>>());
    drop(removed);
    Ok(0)
}

pub fn sys_channel_recv(h: u64, buf: u64, cap: u64, handles_ptr: u64, handles_cap: u64) -> Result<u64, Error> {
    let cap = cap.min(MAX_MESSAGE_BYTES);
    let handles_cap = handles_cap.min(MAX_MESSAGE_HANDLES);
    // Check destinations before taking a message, so delivery cannot fail
    // half-way (processes are single-threaded: the mappings cannot change).
    syscall::check_user_writable(buf, cap)?;
    syscall::check_user_writable(handles_ptr, handles_cap * 8)?;
    let endpoint = with_table(|t| t.get(h, rights::READ).map(|(o, _)| o.clone()))??;
    let ch = endpoint.channel();
    let message = ch.shared.state.lock().recv(ch.side)?;
    if message.data.len() as u64 > cap || message.handles.len() as u64 > handles_cap {
        ch.shared.state.lock().unrecv(ch.side, message);
        return Err(Error::TooLarge);
    }
    let Message { data, handles } = message;
    let count = handles.len();
    let installed = with_table(|t| {
        if t.available() < count {
            return Err(handles);
        }
        let mut values = [0u64; MAX_MESSAGE_HANDLES as usize];
        for (slot, tr) in values.iter_mut().zip(handles) {
            // Capacity was checked above, so insertion cannot fail for
            // lack of slots; on allocation failure the object is dropped.
            *slot = t.insert(tr.object, tr.rights).unwrap_or(0);
        }
        Ok(values)
    });
    let values = match installed {
        Ok(Ok(v)) => v,
        Ok(Err(handles)) => {
            ch.shared.state.lock().unrecv(ch.side, Message { data, handles });
            return Err(Error::NoResources);
        }
        Err(e) => return Err(e),
    };
    syscall::copy_to_user(buf, &data)?;
    let mut raw = [0u8; 8 * MAX_MESSAGE_HANDLES as usize];
    for (chunk, v) in raw.chunks_exact_mut(8).zip(values.iter().take(count)) {
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    syscall::copy_to_user(handles_ptr, raw.get(..8 * count).unwrap_or(&[]))?;
    Ok(data.len() as u64 | (count as u64) << 32)
}

pub fn sys_handle_close(h: u64) -> Result<u64, Error> {
    let removed = with_table(|t| t.remove(h))??;
    drop(removed);
    Ok(0)
}

pub fn sys_handle_duplicate(h: u64, new_rights: u64) -> Result<u64, Error> {
    with_table(|t| t.duplicate(h, new_rights))?
}

pub fn sys_handle_rights(h: u64) -> Result<u64, Error> {
    with_table(|t| t.get(h, 0).map(|(_, r)| r))?
}

pub fn sys_wait_readable(h: u64, timeout: u64) -> Result<u64, Error> {
    if timeout > coco_abi::syscall::MAX_SLEEP_TICKS {
        return Err(Error::InvalidArgument);
    }
    let endpoint = with_table(|t| t.get(h, rights::READ).map(|(o, _)| o.clone()))??;
    let ch = endpoint.channel();
    let deadline = if timeout == 0 { u64::MAX } else { crate::interrupts::ticks() + timeout };
    let me = sched::current();
    loop {
        let enabled = coco_arch::cpu::interrupts_enabled();
        coco_arch::cpu::disable_interrupts();
        let outcome = {
            let mut st = ch.shared.state.lock();
            if st.readable(ch.side) {
                st.remove_waiter(ch.side, me);
                Some(Ok(0))
            } else if crate::interrupts::ticks() >= deadline {
                st.remove_waiter(ch.side, me);
                Some(Err(Error::TimedOut))
            } else {
                st.add_waiter(ch.side, me).err().map(Err)
            }
        };
        match outcome {
            Some(r) => {
                if enabled {
                    // SAFETY: restores the caller's interrupt state; no lock held.
                    unsafe { coco_arch::cpu::enable_interrupts() };
                }
                return r;
            }
            // Interrupts stay disabled from the readiness check until the
            // thread is asleep, so a sender cannot run in between (single
            // CPU): no lost wake-up.
            None => sched::block_until(deadline),
        }
        if enabled {
            // SAFETY: as above.
            unsafe { coco_arch::cpu::enable_interrupts() };
        }
    }
}
