//! x86_64 architecture layer for coco.
//!
//! Every function that executes a privileged or state-changing instruction
//! is `unsafe` and documents what the caller must guarantee. Pure data
//! encodings ([`desc`]) are safe and unit-tested on the host.
#![no_std]

#[cfg(target_arch = "x86_64")]
pub mod cpu;
pub mod desc;
#[cfg(target_arch = "x86_64")]
pub mod serial;
