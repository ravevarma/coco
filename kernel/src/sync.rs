//! Interrupt-safe locking (rule L1).
//!
//! [`IrqSpinLock`] disables maskable interrupts before acquiring its inner
//! [`SpinLock`] and restores the previous interrupt state after releasing
//! it, so an interrupt handler can never spin on a lock held by the code it
//! interrupted on the same CPU.

use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};

use coco_arch::cpu;
use coco_sync::{SpinLock, SpinLockGuard};

/// A spin lock that is held with interrupts disabled.
#[derive(Debug)]
pub struct IrqSpinLock<T> {
    inner: SpinLock<T>,
}

/// Guard for [`IrqSpinLock`]; releases the lock, then restores interrupts.
#[derive(Debug)]
#[must_use = "the lock is released immediately if the guard is dropped"]
pub struct IrqGuard<'a, T> {
    guard: ManuallyDrop<SpinLockGuard<'a, T>>,
    reenable: bool,
}

impl<T> IrqSpinLock<T> {
    /// Creates an unlocked lock.
    pub const fn new(value: T) -> Self {
        Self { inner: SpinLock::new(value) }
    }

    /// Disables interrupts and acquires the lock.
    pub fn lock(&self) -> IrqGuard<'_, T> {
        let reenable = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        IrqGuard { guard: ManuallyDrop::new(self.inner.lock()), reenable }
    }

    /// Acquires the lock only if it is free (never spins).
    pub fn try_lock(&self) -> Option<IrqGuard<'_, T>> {
        let reenable = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        match self.inner.try_lock() {
            Some(g) => Some(IrqGuard { guard: ManuallyDrop::new(g), reenable }),
            None => {
                if reenable {
                    // SAFETY: restores the interrupt state observed on entry;
                    // no lock is held by this call.
                    unsafe { cpu::enable_interrupts() };
                }
                None
            }
        }
    }
}

impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: the inner guard is dropped exactly once, here, and never
        // used afterwards.
        unsafe { ManuallyDrop::drop(&mut self.guard) };
        if self.reenable {
            // SAFETY: interrupts were enabled when the lock was taken and
            // the lock is now released, so restoring them is safe.
            unsafe { cpu::enable_interrupts() };
        }
    }
}
