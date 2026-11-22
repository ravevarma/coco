//! System-call ABI between coco user programs and the kernel.
//!
//! Calling convention (x86_64 `syscall`): number in `rax`, arguments in
//! `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`; result in `rax`. Non-negative
//! results are success values; negative results are [`Error`] codes.
//! `rcx` and `r11` are clobbered by the instruction itself.

use crate::layout::{USER_BASE, USER_TOP};

/// Write bytes to the kernel debug console: `(ptr, len) -> len`.
pub const DEBUG_WRITE: u64 = 1;
/// Terminate the calling process: `(code) -> !`.
pub const EXIT: u64 = 2;
/// Give up the rest of the time slice: `() -> 0`.
pub const YIELD: u64 = 3;
/// Process id of the caller: `() -> pid`.
pub const GET_PID: u64 = 4;
/// Timer ticks since boot: `() -> ticks`.
pub const UPTIME_TICKS: u64 = 5;
/// Sleep for at least `n` ticks (`n == 0` yields): `(n) -> 0`.
pub const SLEEP_TICKS: u64 = 6;

/// Largest single `DEBUG_WRITE`.
pub const MAX_DEBUG_WRITE: u64 = 4096;
/// Longest accepted `SLEEP_TICKS` argument (10 minutes at 100 Hz).
pub const MAX_SLEEP_TICKS: u64 = 60_000;

/// System-call error codes (returned negated in `rax`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    /// Unknown system-call number.
    NoSuchCall = -1,
    /// A pointer argument is outside user space or unmapped.
    BadAddress = -2,
    /// An argument is out of range.
    InvalidArgument = -3,
}

impl Error {
    /// The value placed in `rax`.
    #[must_use]
    pub const fn code(self) -> i64 {
        self as i64
    }

    /// Decodes a negative `rax` value.
    #[must_use]
    pub const fn from_code(code: i64) -> Option<Self> {
        match code {
            -1 => Some(Self::NoSuchCall),
            -2 => Some(Self::BadAddress),
            -3 => Some(Self::InvalidArgument),
            _ => None,
        }
    }
}

/// Checks that `[ptr, ptr + len)` lies entirely inside the user range
/// `[USER_BASE, USER_TOP)` without overflow. Zero-length ranges are valid
/// anywhere inside or at the end of the range. This is the first,
/// structural check; the kernel additionally verifies the pages are
/// mapped user-accessible before touching them.
///
/// # Errors
/// [`Error::BadAddress`] if the range is outside user space.
pub const fn validate_user_range(ptr: u64, len: u64) -> Result<(), Error> {
    let Some(end) = ptr.checked_add(len) else {
        return Err(Error::BadAddress);
    };
    if ptr < USER_BASE || end > USER_TOP {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// Behaviours of the bundled `init` test program, passed in `rdi` at entry.
pub mod init_mode {
    /// Print a greeting and exit(0).
    pub const HELLO: u64 = 0;
    /// Spin (no syscalls) for `arg` ticks of uptime, then exit — proves
    /// preemption: other processes must keep running meanwhile.
    pub const SPIN: u64 = 1;
    /// Dereference address 0x10 (unmapped): must be killed.
    pub const FAULT_NULL: u64 = 2;
    /// Read kernel memory: must be killed (supervisor page).
    pub const FAULT_KERNEL_READ: u64 = 3;
    /// Execute `cli` (privileged): must be killed (#GP).
    pub const FAULT_PRIVILEGED: u64 = 4;
    /// Jump into its own data/stack (NX): must be killed.
    pub const FAULT_NX: u64 = 5;
    /// Write to its own read-only text: must be killed.
    pub const FAULT_WRITE_TEXT: u64 = 6;
    /// Issue many random system calls with hostile arguments; must survive
    /// (every call returns an error or succeeds) and exit(0).
    pub const SYSCALL_FUZZ: u64 = 7;
    /// Overflow its stack into the guard page: must be killed.
    pub const FAULT_STACK_OVERFLOW: u64 = 8;
    /// Exit immediately with code `arg` (process churn tests).
    pub const EXIT_CODE: u64 = 9;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_range_checks() {
        assert_eq!(validate_user_range(USER_BASE, 16), Ok(()));
        assert_eq!(validate_user_range(USER_TOP - 16, 16), Ok(()));
        assert_eq!(validate_user_range(USER_TOP, 0), Ok(()));
        assert_eq!(validate_user_range(USER_TOP - 16, 17), Err(Error::BadAddress));
        assert_eq!(validate_user_range(0, 1), Err(Error::BadAddress));
        assert_eq!(validate_user_range(USER_BASE - 1, 1), Err(Error::BadAddress));
        assert_eq!(validate_user_range(u64::MAX, 2), Err(Error::BadAddress));
        assert_eq!(validate_user_range(0xFFFF_8000_0000_0000, 8), Err(Error::BadAddress));
        assert_eq!(validate_user_range(USER_BASE, u64::MAX), Err(Error::BadAddress));
    }

    #[test]
    fn error_codes_roundtrip() {
        for e in [Error::NoSuchCall, Error::BadAddress, Error::InvalidArgument] {
            assert!(e.code() < 0);
            assert_eq!(Error::from_code(e.code()), Some(e));
        }
        assert_eq!(Error::from_code(0), None);
        assert_eq!(Error::from_code(-99), None);
    }
}
