//! Architecture backends. Each provides the same functions:
//! `NAME`, `putc`, `counter`, `counter_hz`, `start_timer`,
//! `wait_for_interrupt`, `breakpoint`, `kernel_image`, `exit`, and an entry
//! point that discovers the [`crate::Platform`] and calls
//! [`crate::kernel_main`].

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
mod riscv;
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
pub use riscv::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

#[cfg(target_arch = "x86")]
mod x86;
#[cfg(target_arch = "x86")]
pub use x86::*;
