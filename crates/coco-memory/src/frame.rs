//! Bitmap physical frame allocator.
//!
//! # Invariants
//! * `managed` has a bit set for exactly the frames that were handed to the
//!   allocator through [`FrameAllocator::add_free_range`].
//! * `free ⊆ managed` at all times.
//! * `free_count == popcount(free)` and `managed_count == popcount(managed)`.
//! * A [`PhysFrame`] token exists for a managed frame iff its free bit is
//!   clear (the token is the ownership proof; it is neither `Copy` nor `Clone`).
//! * `hint` is a word index such that no word below it contains a free bit
//!   that was freed before the last full scan (it is only a search start).

use crate::PAGE_SIZE;

/// Ownership token for one 4 KiB physical frame.
///
/// Dropping a token without returning it to the allocator leaks the frame;
/// the type is `#[must_use]` and leak tests compare allocator accounting.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "dropping a PhysFrame leaks physical memory"]
pub struct PhysFrame(u64);

impl PhysFrame {
    /// Physical address of the frame.
    #[must_use]
    pub const fn addr(&self) -> u64 {
        self.0
    }

    /// Converts the token into a raw address, transferring ownership to the
    /// caller (for example into a page-table entry).
    #[must_use]
    pub const fn into_raw(self) -> u64 {
        self.0
    }

    /// Recreates a token from a raw address.
    ///
    /// # Safety
    /// The caller must own the frame at `addr` exclusively (it was obtained
    /// via [`PhysFrame::into_raw`] and has not been re-wrapped since), and
    /// `addr` must be page aligned.
    pub const unsafe fn from_raw(addr: u64) -> Self {
        Self(addr)
    }
}

/// Allocator errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// No free frame is available.
    OutOfMemory,
    /// Address not page aligned.
    Misaligned,
    /// Address beyond the range covered by the bitmap.
    OutOfRange,
    /// Range already (partially) managed by the allocator.
    AlreadyManaged,
    /// Frame is not managed by this allocator.
    NotManaged,
    /// Frame is already free (double free).
    DoubleFree,
    /// Bitmap storage too small for the requested frame count.
    StorageTooSmall,
}

/// Bitmap frame allocator over caller-provided storage.
#[derive(Debug)]
pub struct FrameAllocator<'a> {
    managed: &'a mut [u64],
    free: &'a mut [u64],
    frames: u64,
    free_count: u64,
    managed_count: u64,
    hint: usize,
}

/// Number of `u64` words needed per bitmap to track `frames` frames, or
/// `None` if that does not fit in `usize` (possible on 32-bit targets:
/// never truncate a size).
#[must_use]
pub fn words_for(frames: u64) -> Option<usize> {
    usize::try_from(frames.div_ceil(64)).ok()
}

impl<'a> FrameAllocator<'a> {
    /// Creates an allocator covering frames `0..frames`. All frames start
    /// unmanaged. Both bitmaps are zeroed.
    ///
    /// # Errors
    /// [`FrameError::StorageTooSmall`] if either bitmap is shorter than
    /// [`words_for`]`(frames)`; [`FrameError::OutOfRange`] if `frames`
    /// cannot be indexed on this target.
    pub fn new(managed: &'a mut [u64], free: &'a mut [u64], frames: u64) -> Result<Self, FrameError> {
        let words = words_for(frames).ok_or(FrameError::OutOfRange)?;
        if managed.len() < words || free.len() < words {
            return Err(FrameError::StorageTooSmall);
        }
        managed.fill(0);
        free.fill(0);
        Ok(Self { managed, free, frames, free_count: 0, managed_count: 0, hint: 0 })
    }

    fn index(&self, addr: u64) -> Result<(usize, u64), FrameError> {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Misaligned);
        }
        let frame = addr / PAGE_SIZE;
        if frame >= self.frames {
            return Err(FrameError::OutOfRange);
        }
        // `frame < frames` and `words_for(frames)` fitted in usize at
        // construction, so this conversion cannot fail; checked anyway.
        let word = usize::try_from(frame / 64).map_err(|_| FrameError::OutOfRange)?;
        Ok((word, 1u64 << (frame % 64)))
    }

    /// Hands `[base, base + pages * 4096)` to the allocator as free memory.
    ///
    /// The operation is atomic: on error nothing is changed.
    ///
    /// # Errors
    /// Misaligned, out-of-range or already-managed input.
    pub fn add_free_range(&mut self, base: u64, pages: u64) -> Result<(), FrameError> {
        let end = pages.checked_mul(PAGE_SIZE).and_then(|l| base.checked_add(l)).ok_or(FrameError::OutOfRange)?;
        if pages == 0 {
            return Ok(());
        }
        let last = end.checked_sub(PAGE_SIZE).ok_or(FrameError::OutOfRange)?;
        self.index(base)?;
        self.index(last)?;
        let mut addr = base;
        while addr < end {
            let (w, bit) = self.index(addr)?;
            if self.managed.get(w).is_some_and(|m| m & bit != 0) {
                return Err(FrameError::AlreadyManaged);
            }
            addr = addr.saturating_add(PAGE_SIZE);
        }
        let mut addr = base;
        while addr < end {
            let (w, bit) = self.index(addr)?;
            if let (Some(m), Some(f)) = (self.managed.get_mut(w), self.free.get_mut(w)) {
                *m |= bit;
                *f |= bit;
            }
            addr = addr.saturating_add(PAGE_SIZE);
        }
        self.managed_count = self.managed_count.saturating_add(pages);
        self.free_count = self.free_count.saturating_add(pages);
        self.hint = self.hint.min(usize::try_from(base / PAGE_SIZE / 64).unwrap_or(usize::MAX));
        Ok(())
    }

    /// Allocates one frame (lowest free address at or after the search hint).
    ///
    /// # Errors
    /// [`FrameError::OutOfMemory`] when no frame is free.
    pub fn allocate(&mut self) -> Result<PhysFrame, FrameError> {
        if self.free_count == 0 {
            return Err(FrameError::OutOfMemory);
        }
        let words = self.free.len();
        for step in 0..words {
            let w = self.hint.saturating_add(step).checked_rem(words).unwrap_or(0);
            let Some(word) = self.free.get_mut(w) else {
                continue;
            };
            if *word != 0 {
                let bit = word.trailing_zeros();
                *word &= !(1u64 << bit);
                self.free_count = self.free_count.saturating_sub(1);
                self.hint = w;
                let frame = (w as u64).saturating_mul(64).saturating_add(u64::from(bit));
                return Ok(PhysFrame(frame.saturating_mul(PAGE_SIZE)));
            }
        }
        // Unreachable while invariants hold: free_count > 0 implies a set bit.
        Err(FrameError::OutOfMemory)
    }

    /// Returns a frame to the allocator.
    ///
    /// # Errors
    /// [`FrameError::NotManaged`] / [`FrameError::DoubleFree`] if the token
    /// was forged; the token is consumed either way.
    pub fn free(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        let (w, bit) = self.index(frame.0)?;
        let managed = self.managed.get(w).copied().unwrap_or(0);
        if managed & bit == 0 {
            return Err(FrameError::NotManaged);
        }
        let word = self.free.get_mut(w).ok_or(FrameError::OutOfRange)?;
        if *word & bit != 0 {
            return Err(FrameError::DoubleFree);
        }
        *word |= bit;
        self.free_count = self.free_count.saturating_add(1);
        self.hint = self.hint.min(w);
        Ok(())
    }

    /// Is the frame at `addr` currently free?
    #[must_use]
    pub fn is_free(&self, addr: u64) -> bool {
        self.index(addr).is_ok_and(|(w, bit)| self.free.get(w).is_some_and(|f| f & bit != 0))
    }

    /// Number of free frames.
    #[must_use]
    pub const fn free_frames(&self) -> u64 {
        self.free_count
    }

    /// Number of frames under management (free + allocated).
    #[must_use]
    pub const fn managed_frames(&self) -> u64 {
        self.managed_count
    }

    /// Number of frames currently allocated.
    #[must_use]
    pub const fn allocated_frames(&self) -> u64 {
        self.managed_count.saturating_sub(self.free_count)
    }

    /// Recomputes counters from the bitmaps and checks every invariant.
    /// Intended for self-tests; O(n).
    #[must_use]
    pub fn check_invariants(&self) -> bool {
        let mut free = 0u64;
        let mut managed = 0u64;
        for (m, f) in self.managed.iter().zip(self.free.iter()) {
            if f & !m != 0 {
                return false;
            }
            free = free.saturating_add(u64::from(f.count_ones()));
            managed = managed.saturating_add(u64::from(m.count_ones()));
        }
        free == self.free_count && managed == self.managed_count
    }
}
