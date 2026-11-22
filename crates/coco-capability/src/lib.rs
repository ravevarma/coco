//! Per-process capability tables.
//!
//! A handle names a slot plus the slot's generation
//! (`coco_abi::syscall::handle`). Every operation re-validates both, so a
//! closed handle — or a forged one — is rejected even after its slot has
//! been reused. Rights can only be reduced: `duplicate` requires
//! `DUPLICATE` and refuses any right the source lacks.
//!
//! # Invariants
//! * `live == number of occupied slots <= capacity`.
//! * Every free slot index appears exactly once in `free`.
//! * A slot's generation changes whenever its occupant is removed.
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

extern crate alloc;

use alloc::vec::Vec;

use coco_abi::syscall::{Error, handle, rights};

struct Slot<T> {
    generation: u32,
    entry: Option<(T, u64)>,
}

/// A process's table of capabilities to kernel objects of type `T`.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    live: usize,
    capacity: usize,
}

impl<T> core::fmt::Debug for HandleTable<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HandleTable").field("live", &self.live).field("capacity", &self.capacity).finish()
    }
}

impl<T> HandleTable<T> {
    /// An empty table holding at most `capacity` handles.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self { slots: Vec::new(), free: Vec::new(), live: 0, capacity }
    }

    /// Number of live handles.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.live
    }

    /// True if no handle is live.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Free capacity.
    #[must_use]
    pub const fn available(&self) -> usize {
        self.capacity.saturating_sub(self.live)
    }

    /// Installs `object` with `rights`, returning the new handle value.
    ///
    /// # Errors
    /// `(Error::NoResources, object)` when full (or memory is exhausted),
    /// `(Error::InvalidArgument, object)` for undefined rights bits. The
    /// object is handed back so the caller decides how to release it.
    pub fn insert(&mut self, object: T, rights_bits: u64) -> Result<u64, (Error, T)> {
        if rights_bits & !rights::ALL != 0 {
            return Err((Error::InvalidArgument, object));
        }
        if self.live >= self.capacity {
            return Err((Error::NoResources, object));
        }
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                let Ok(i) = u32::try_from(self.slots.len()) else { return Err((Error::NoResources, object)) };
                if self.slots.try_reserve(1).is_err() || self.free.try_reserve(1).is_err() {
                    return Err((Error::NoResources, object));
                }
                self.slots.push(Slot { generation: 0, entry: None });
                i
            }
        };
        let Some(slot) = self.slots.get_mut(index as usize) else { return Err((Error::NoResources, object)) };
        slot.entry = Some((object, rights_bits));
        self.live = self.live.saturating_add(1);
        Ok(handle::encode(index, slot.generation))
    }

    fn slot(&self, value: u64) -> Result<&Slot<T>, Error> {
        let (index, generation) = handle::decode(value).ok_or(Error::BadHandle)?;
        let slot = self.slots.get(index as usize).ok_or(Error::BadHandle)?;
        if slot.generation != generation || slot.entry.is_none() {
            return Err(Error::BadHandle);
        }
        Ok(slot)
    }

    /// Looks up `value`, requiring every right in `required`.
    ///
    /// # Errors
    /// [`Error::BadHandle`] or [`Error::AccessDenied`].
    pub fn get(&self, value: u64, required: u64) -> Result<(&T, u64), Error> {
        let (object, held) = self.slot(value)?.entry.as_ref().ok_or(Error::BadHandle)?;
        if held & required != required {
            return Err(Error::AccessDenied);
        }
        Ok((object, *held))
    }

    /// Removes `value` (closing it), returning the object and its rights.
    ///
    /// # Errors
    /// [`Error::BadHandle`].
    pub fn remove(&mut self, value: u64) -> Result<(T, u64), Error> {
        self.slot(value)?;
        let (index, _) = handle::decode(value).ok_or(Error::BadHandle)?;
        let slot = self.slots.get_mut(index as usize).ok_or(Error::BadHandle)?;
        let entry = slot.entry.take().ok_or(Error::BadHandle)?;
        slot.generation = slot.generation.wrapping_add(1);
        // Capacity for this push was reserved when the slot was created.
        self.free.push(index);
        self.live = self.live.saturating_sub(1);
        Ok(entry)
    }

    /// Removes every handle (process exit), returning the objects.
    pub fn drain(&mut self) -> Vec<T> {
        let mut out = Vec::with_capacity(self.live);
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Some((object, _)) = slot.entry.take() {
                slot.generation = slot.generation.wrapping_add(1);
                out.push(object);
                if let Ok(i) = u32::try_from(i) {
                    self.free.push(i);
                }
            }
        }
        self.live = 0;
        out
    }
}

impl<T: Clone> HandleTable<T> {
    /// Creates a second handle to the same object with `new_rights`
    /// (which must be a subset of the source's rights).
    ///
    /// # Errors
    /// [`Error::BadHandle`], [`Error::AccessDenied`] (source lacks
    /// `DUPLICATE`, or `new_rights` would add a right),
    /// [`Error::InvalidArgument`] (undefined bits), [`Error::NoResources`].
    pub fn duplicate(&mut self, value: u64, new_rights: u64) -> Result<u64, Error> {
        if new_rights & !rights::ALL != 0 {
            return Err(Error::InvalidArgument);
        }
        let (object, held) = self.get(value, rights::DUPLICATE)?;
        if new_rights & !held != 0 {
            return Err(Error::AccessDenied);
        }
        let copy = object.clone();
        self.insert(copy, new_rights).map_err(|(e, _dropped)| e)
    }
}

#[cfg(test)]
mod tests;
