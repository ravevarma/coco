//! x86_64 architecture layer for coco.
//!
//! Every function that executes a privileged or state-changing instruction
//! is `unsafe` and documents what the caller must guarantee. Pure data
//! encodings ([`desc`]) are safe and unit-tested on the host.
#![no_std]

pub mod cpu;
pub mod desc;
pub mod serial;
