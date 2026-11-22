//! Channel state for coco IPC.
//!
//! A channel has two sides (0 and 1). A message sent from side `s` is
//! queued at side `1 - s`. This crate holds only the state machine; the
//! kernel provides locking and blocking. All methods are O(1) or O(queue)
//! and never block.
//!
//! Semantics:
//! * Each side counts its open references (`opened` / `closed`); a side is
//!   closed when the count reaches zero.
//! * Sending to a closed side fails with `PeerClosed`; receiving from an
//!   empty queue fails with `WouldBlock` while the peer is open and
//!   `PeerClosed` once it is closed.
//! * When a side closes, its inbound queue is drained and returned to the
//!   caller, who drops the messages (and any handles inside them) *outside*
//!   its lock, together with the peer's waiters to wake.
//! * Limits: [`MAX_MESSAGE_BYTES`] per message, [`MAX_MESSAGE_HANDLES`]
//!   handles per message, [`MAX_QUEUED_MESSAGES`] per queue,
//!   [`MAX_WAITERS`] registered waiters per side.
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub use coco_abi::syscall::{Error, MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES, MAX_QUEUED_MESSAGES};

/// Most threads that can wait on one side at once.
pub const MAX_WAITERS: usize = 4;

/// A queued message carrying bytes and transferred handles of type `H`.
#[derive(Debug, PartialEq, Eq)]
pub struct Message<H> {
    /// Payload.
    pub data: Vec<u8>,
    /// Transferred capabilities.
    pub handles: Vec<H>,
}

#[derive(Debug)]
struct Side<H> {
    inbound: VecDeque<Message<H>>,
    open: usize,
    waiters: [Option<u64>; MAX_WAITERS],
}

impl<H> Side<H> {
    const fn new() -> Self {
        Self { inbound: VecDeque::new(), open: 1, waiters: [None; MAX_WAITERS] }
    }
}

/// Waiters to wake after an operation (thread ids).
pub type Wake = [Option<u64>; MAX_WAITERS];

/// The state of one channel.
#[derive(Debug)]
pub struct Channel<H> {
    sides: [Side<H>; 2],
}

impl<H> Default for Channel<H> {
    fn default() -> Self {
        Self::new()
    }
}

/// The peer of side `side` (sides are always taken modulo 2).
fn other(side: usize) -> usize {
    (side & 1) ^ 1
}

impl<H> Channel<H> {
    /// A new channel with one open reference per side.
    #[must_use]
    pub const fn new() -> Self {
        Self { sides: [Side::new(), Side::new()] }
    }

    fn side(&self, s: usize) -> &Side<H> {
        let [a, b] = &self.sides;
        if s & 1 == 0 { a } else { b }
    }

    fn side_mut(&mut self, s: usize) -> &mut Side<H> {
        let [a, b] = &mut self.sides;
        if s & 1 == 0 { a } else { b }
    }

    /// True if side `s` still has open references.
    #[must_use]
    pub fn is_open(&self, s: usize) -> bool {
        self.side(s).open > 0
    }

    /// Messages waiting at side `s`.
    #[must_use]
    pub fn queued(&self, s: usize) -> usize {
        self.side(s).inbound.len()
    }

    /// True if a receive on side `s` would not return `WouldBlock`.
    #[must_use]
    pub fn readable(&self, s: usize) -> bool {
        !self.side(s).inbound.is_empty() || !self.is_open(other(s))
    }

    /// Sends from side `from`. On failure the handles are returned.
    ///
    /// # Errors
    /// `PeerClosed`, `TooLarge`, `QueueFull`, `NoResources` (allocation).
    pub fn send(&mut self, from: usize, data: &[u8], handles: Vec<H>) -> Result<Wake, (Error, Vec<H>)> {
        let to = other(from);
        if data.len() as u64 > MAX_MESSAGE_BYTES || handles.len() as u64 > MAX_MESSAGE_HANDLES {
            return Err((Error::TooLarge, handles));
        }
        if !self.is_open(to) {
            return Err((Error::PeerClosed, handles));
        }
        let side = self.side_mut(to);
        if side.inbound.len() >= MAX_QUEUED_MESSAGES {
            return Err((Error::QueueFull, handles));
        }
        let mut payload = Vec::new();
        if payload.try_reserve_exact(data.len()).is_err() || side.inbound.try_reserve(1).is_err() {
            return Err((Error::NoResources, handles));
        }
        payload.extend_from_slice(data);
        side.inbound.push_back(Message { data: payload, handles });
        Ok(core::mem::replace(&mut side.waiters, [None; MAX_WAITERS]))
    }

    /// Receives at side `at`.
    ///
    /// # Errors
    /// `WouldBlock` (empty, peer open) or `PeerClosed` (empty, peer closed).
    pub fn recv(&mut self, at: usize) -> Result<Message<H>, Error> {
        let peer_open = self.is_open(other(at));
        match self.side_mut(at).inbound.pop_front() {
            Some(m) => Ok(m),
            None if peer_open => Err(Error::WouldBlock),
            None => Err(Error::PeerClosed),
        }
    }

    /// Puts a received message back at the head of side `at` (used when the
    /// receiver cannot accept it, e.g. its handle table is full).
    pub fn unrecv(&mut self, at: usize, message: Message<H>) {
        self.side_mut(at).inbound.push_front(message);
    }

    /// Every handle currently in flight in either queue of this channel.
    pub fn in_flight(&self) -> impl Iterator<Item = &H> {
        self.sides.iter().flat_map(|s| s.inbound.iter()).flat_map(|m| m.handles.iter())
    }

    /// Registers thread `tid` to be woken when side `at` becomes readable.
    ///
    /// # Errors
    /// `NoResources` when [`MAX_WAITERS`] threads already wait.
    pub fn add_waiter(&mut self, at: usize, tid: u64) -> Result<(), Error> {
        let waiters = &mut self.side_mut(at).waiters;
        if waiters.contains(&Some(tid)) {
            return Ok(());
        }
        let slot = waiters.iter_mut().find(|w| w.is_none()).ok_or(Error::NoResources)?;
        *slot = Some(tid);
        Ok(())
    }

    /// Unregisters `tid` from side `at`.
    pub fn remove_waiter(&mut self, at: usize, tid: u64) {
        for w in &mut self.side_mut(at).waiters {
            if *w == Some(tid) {
                *w = None;
            }
        }
    }

    /// Records a new reference to side `s`.
    pub fn opened(&mut self, s: usize) {
        let side = self.side_mut(s);
        side.open = side.open.saturating_add(1);
    }

    /// Drops a reference to side `s`. When it was the last one, returns the
    /// drained inbound queue (to be dropped by the caller outside its lock)
    /// and the peer's waiters to wake.
    pub fn closed(&mut self, s: usize) -> Option<(VecDeque<Message<H>>, Wake)> {
        let side = self.side_mut(s);
        side.open = side.open.saturating_sub(1);
        if side.open > 0 {
            return None;
        }
        let drained = core::mem::take(&mut side.inbound);
        side.waiters = [None; MAX_WAITERS];
        let peer = self.side_mut(other(s));
        Some((drained, core::mem::replace(&mut peer.waiters, [None; MAX_WAITERS])))
    }
}

#[cfg(test)]
mod tests;
