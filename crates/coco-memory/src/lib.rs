//! Physical memory management primitives for coco.
//!
//! * [`frame`] — bitmap physical frame allocator with move-only frame
//!   ownership tokens, runtime double-free detection and exact accounting.
//! * [`paging`] — x86_64 4-level page-table mapper, generic over how page
//!   tables are reached (identity map in the loader, direct map in the
//!   kernel, a simulated arena in host tests).
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

pub mod frame;
pub mod paging;

/// Size of a base page / physical frame.
pub const PAGE_SIZE: u64 = 4096;

#[cfg(test)]
mod tests;
