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

/// Create a channel: `(out_ptr) -> 0`; writes two handles (`[u64; 2]`,
/// one per endpoint, rights READ|WRITE|TRANSFER|DUPLICATE) to `out_ptr`.
pub const CHANNEL_CREATE: u64 = 10;
/// Send: `(handle, data_ptr, data_len, handles_ptr, handle_count) -> 0`.
/// Requires WRITE; every transferred handle needs TRANSFER and is moved
/// (removed from the sender's table) only if the send succeeds.
pub const CHANNEL_SEND: u64 = 11;
/// Receive without blocking: `(handle, buf_ptr, buf_cap, handles_ptr,
/// handles_cap) -> data_len | handle_count << 32`. Requires READ.
pub const CHANNEL_RECV: u64 = 12;
/// Close a handle: `(handle) -> 0`.
pub const HANDLE_CLOSE: u64 = 13;
/// Duplicate with equal or fewer rights: `(handle, rights) -> new handle`.
/// Requires DUPLICATE.
pub const HANDLE_DUPLICATE: u64 = 14;
/// Block until the endpoint has a message or its peer closed:
/// `(handle, timeout_ticks; 0 = forever) -> 0`. Requires READ.
pub const WAIT_READABLE: u64 = 15;
/// Rights of a handle: `(handle) -> rights`.
pub const HANDLE_RIGHTS: u64 = 16;

/// Largest channel message payload.
pub const MAX_MESSAGE_BYTES: u64 = 256;
/// Most handles carried by one message.
pub const MAX_MESSAGE_HANDLES: u64 = 4;
/// Messages queued per endpoint before `QueueFull`.
pub const MAX_QUEUED_MESSAGES: usize = 64;
/// Handles one process may hold.
pub const MAX_HANDLES: usize = 256;

/// Capability rights carried by a handle.
pub mod rights {
    /// Receive / wait.
    pub const READ: u64 = 1 << 0;
    /// Send.
    pub const WRITE: u64 = 1 << 1;
    /// May be moved to another process inside a message.
    pub const TRANSFER: u64 = 1 << 2;
    /// May be duplicated (with equal or fewer rights).
    pub const DUPLICATE: u64 = 1 << 3;
    /// Every defined right.
    pub const ALL: u64 = READ | WRITE | TRANSFER | DUPLICATE;
}

/// Handle values: `generation << 32 | (slot + 1)`. Zero is never valid; a
/// closed slot's generation changes, so stale values are rejected.
pub mod handle {
    /// Encodes a handle value.
    #[must_use]
    pub const fn encode(slot: u32, generation: u32) -> u64 {
        ((generation as u64) << 32) | (slot as u64).saturating_add(1)
    }

    /// Decodes a handle value into `(slot, generation)`.
    #[must_use]
    pub const fn decode(value: u64) -> Option<(u32, u32)> {
        match ((value & 0xFFFF_FFFF) as u32).checked_sub(1) {
            Some(slot) => Some((slot, (value >> 32) as u32)),
            None => None,
        }
    }
}

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
    /// The handle does not name a live capability of the caller.
    BadHandle = -4,
    /// The handle lacks a right the operation needs.
    AccessDenied = -5,
    /// Nothing to receive right now.
    WouldBlock = -6,
    /// The other endpoint has been closed.
    PeerClosed = -7,
    /// The receiving endpoint's queue is full.
    QueueFull = -8,
    /// Message or buffer larger than allowed.
    TooLarge = -9,
    /// The wait timed out.
    TimedOut = -10,
    /// A kernel or per-process limit was reached.
    NoResources = -11,
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
            -4 => Some(Self::BadHandle),
            -5 => Some(Self::AccessDenied),
            -6 => Some(Self::WouldBlock),
            -7 => Some(Self::PeerClosed),
            -8 => Some(Self::QueueFull),
            -9 => Some(Self::TooLarge),
            -10 => Some(Self::TimedOut),
            -11 => Some(Self::NoResources),
            _ => None,
        }
    }
}

/// Checks that `[ptr, ptr + len)` lies entirely inside the user range
/// `[USER_BASE, USER_TOP)` without overflow. Zero-length ranges are valid
/// at any address: they never access memory (an empty Rust slice carries a
/// dangling, non-null pointer such as `0x8`). This is the first,
/// structural check; the kernel additionally verifies the pages are
/// mapped user-accessible before touching them.
///
/// # Errors
/// [`Error::BadAddress`] if the range is outside user space.
pub const fn validate_user_range(ptr: u64, len: u64) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }
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
    /// Echo service: serve the channel whose handle is `arg`, replying to
    /// every message with its upper-cased bytes; exit(0) on peer close.
    pub const ECHO_SERVICE: u64 = 10;
    /// Echo client: 1,000 round trips over handle `arg`, verify replies,
    /// report the mean round-trip time, close, exit(0).
    pub const ECHO_CLIENT: u64 = 11;
    /// Capability tests from user mode; exit(0) or the failing check number.
    pub const CAP_TESTS: u64 = 12;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_range_checks() {
        assert_eq!(validate_user_range(USER_BASE, 16), Ok(()));
        assert_eq!(validate_user_range(USER_TOP - 16, 16), Ok(()));
        assert_eq!(validate_user_range(USER_TOP, 0), Ok(()));
        // Empty ranges never touch memory: any address is acceptable.
        assert_eq!(validate_user_range(0, 0), Ok(()));
        assert_eq!(validate_user_range(8, 0), Ok(()));
        assert_eq!(validate_user_range(u64::MAX, 0), Ok(()));
        assert_eq!(validate_user_range(USER_TOP - 16, 17), Err(Error::BadAddress));
        assert_eq!(validate_user_range(0, 1), Err(Error::BadAddress));
        assert_eq!(validate_user_range(USER_BASE - 1, 1), Err(Error::BadAddress));
        assert_eq!(validate_user_range(u64::MAX, 2), Err(Error::BadAddress));
        assert_eq!(validate_user_range(0xFFFF_8000_0000_0000, 8), Err(Error::BadAddress));
        assert_eq!(validate_user_range(USER_BASE, u64::MAX), Err(Error::BadAddress));
    }

    #[test]
    fn handle_encoding() {
        assert_eq!(handle::decode(0), None);
        assert_eq!(handle::decode(handle::encode(0, 0)), Some((0, 0)));
        assert_eq!(handle::decode(handle::encode(255, 7)), Some((255, 7)));
        assert_eq!(handle::decode(handle::encode(u32::MAX - 1, u32::MAX)), Some((u32::MAX - 1, u32::MAX)));
        assert_ne!(handle::encode(3, 1), handle::encode(3, 2));
        assert_eq!(rights::ALL, 0xF);
    }

    #[test]
    fn error_codes_roundtrip() {
        for e in [
            Error::NoSuchCall,
            Error::BadAddress,
            Error::InvalidArgument,
            Error::BadHandle,
            Error::AccessDenied,
            Error::WouldBlock,
            Error::PeerClosed,
            Error::QueueFull,
            Error::TooLarge,
            Error::TimedOut,
            Error::NoResources,
        ] {
            assert!(e.code() < 0);
            assert_eq!(Error::from_code(e.code()), Some(e));
        }
        assert_eq!(Error::from_code(0), None);
        assert_eq!(Error::from_code(-99), None);
    }
}
