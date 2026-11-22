//! Synchronization primitives for coco.
//!
//! [`SpinLock`] is a test-and-test-and-set lock. It does **not** manage
//! interrupts; the kernel wraps it so that a lock that may be taken from an
//! interrupt handler is only ever acquired with interrupts disabled (see
//! `docs/kernel-safety-invariants.md`, rule L1).
//!
//! Memory ordering: acquisition uses `Acquire` (on the successful CAS) and
//! release uses `Release`, so every write made while holding the lock
//! happens-before the next holder's reads. This protocol is model-checked
//! with loom (`RUSTFLAGS="--cfg loom" cargo test -p coco-sync --release`).
#![no_std]

#[cfg(loom)]
extern crate std;

use core::ops::{Deref, DerefMut};

#[cfg(not(loom))]
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(loom)]
use loom::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(loom))]
use core::cell::UnsafeCell;
#[cfg(loom)]
use loom::cell::UnsafeCell;

/// A mutual-exclusion spin lock.
#[derive(Debug)]
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: the lock serializes all access to `data`, so sharing the lock
// between threads only ever gives one thread access at a time; `T: Send` is
// required because the value is effectively moved between threads.
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: moving the lock moves the owned `T`, which is `Send`.
unsafe impl<T: Send> Send for SpinLock<T> {}

/// RAII guard; the lock is released when it is dropped.
#[derive(Debug)]
#[must_use = "the lock is released immediately if the guard is dropped"]
pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    #[cfg(loom)]
    ptr: loom::cell::MutPtr<T>,
}

impl<T> SpinLock<T> {
    /// Creates an unlocked lock.
    #[cfg(not(loom))]
    pub const fn new(value: T) -> Self {
        Self { locked: AtomicBool::new(false), data: UnsafeCell::new(value) }
    }

    /// Creates an unlocked lock.
    #[cfg(loom)]
    pub fn new(value: T) -> Self {
        Self { locked: AtomicBool::new(false), data: UnsafeCell::new(value) }
    }

    /// Spins until the lock is acquired.
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            // Test-and-test-and-set: wait with plain loads so the cache line
            // is not bounced between cores by failed CAS attempts.
            while self.locked.load(Ordering::Relaxed) {
                #[cfg(loom)]
                loom::thread::yield_now();
                #[cfg(not(loom))]
                core::hint::spin_loop();
            }
        }
    }

    /// Acquires the lock if it is free, without spinning.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok().map(|_| SpinLockGuard {
            lock: self,
            #[cfg(loom)]
            ptr: self.data.get_mut(),
        })
    }

    /// Returns true if the lock is currently held (racy; diagnostics only).
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    /// Mutable access without locking; statically exclusive.
    #[cfg(not(loom))]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Consumes the lock, returning the value.
    #[cfg(not(loom))]
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        #[cfg(not(loom))]
        // SAFETY: the guard exists only while `locked` is true and was set by
        // this guard's successful CAS, so no other guard can access `data`.
        unsafe {
            &*self.lock.data.get()
        }
        #[cfg(loom)]
        // SAFETY: as above; loom additionally tracks the access.
        unsafe {
            self.ptr.deref()
        }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        #[cfg(not(loom))]
        // SAFETY: exclusive by the lock invariant, and `&mut self` prevents
        // a second reference through this same guard.
        unsafe {
            &mut *self.lock.data.get()
        }
        #[cfg(loom)]
        // SAFETY: as above.
        unsafe {
            self.ptr.deref()
        }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    extern crate std;
    use super::SpinLock;
    use std::sync::Arc;
    use std::vec::Vec;

    #[test]
    fn exclusive_and_reentrant_try_lock_fails() {
        let l = SpinLock::new(5);
        let g = l.lock();
        assert!(l.is_locked());
        assert!(l.try_lock().is_none());
        drop(g);
        assert!(!l.is_locked());
        *l.try_lock().unwrap() += 1;
        assert_eq!(l.into_inner(), 6);
    }

    #[test]
    fn threads_do_not_lose_updates() {
        let lock = Arc::new(SpinLock::new((0u64, 0u64)));
        let (n_threads, iters) = if cfg!(miri) { (3, 200) } else { (8, 20_000) };
        let threads: Vec<_> = (0..n_threads)
            .map(|_| {
                let l = Arc::clone(&lock);
                std::thread::spawn(move || {
                    for _ in 0..iters {
                        let mut g = l.lock();
                        // Two fields updated non-atomically: a broken lock
                        // would let them diverge.
                        g.0 += 1;
                        g.1 = g.0;
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let g = lock.lock();
        let total = n_threads * iters;
        assert_eq!(*g, (total, total));
    }
}

#[cfg(all(test, loom))]
mod loom_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    extern crate std;
    use super::SpinLock;
    use loom::sync::Arc;

    #[test]
    fn loom_mutual_exclusion() {
        loom::model(|| {
            let lock = Arc::new(SpinLock::new(0usize));
            let handles: std::vec::Vec<_> = (0..2)
                .map(|_| {
                    let l = Arc::clone(&lock);
                    loom::thread::spawn(move || {
                        let mut g = l.lock();
                        let v = *g;
                        *g = v + 1;
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            assert_eq!(*lock.lock(), 2);
        });
    }

    #[test]
    fn loom_try_lock_never_overlaps() {
        loom::model(|| {
            let lock = Arc::new(SpinLock::new(0usize));
            let l2 = Arc::clone(&lock);
            let t = loom::thread::spawn(move || {
                if let Some(mut g) = l2.try_lock() {
                    *g += 1;
                }
            });
            if let Some(mut g) = lock.try_lock() {
                *g += 10;
            }
            t.join().unwrap();
            let v = *lock.lock();
            assert!(v == 1 || v == 10 || v == 11);
        });
    }
}
