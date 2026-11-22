//! System-call entry (SYSCALL/SYSRET) and dispatch.
//!
//! Entry stub: `swapgs` reaches the per-CPU block only long enough to save
//! the user RSP and load the thread's kernel stack, then swaps back, so GS
//! always holds the user value while kernel code runs (the kernel never
//! uses GS). The arguments are saved in a [`SyscallFrame`]; interrupts are
//! re-enabled for the duration of the call (the kernel is preemptible
//! except while holding a lock). Every caller-saved register is restored
//! from the frame on return, so no kernel values leak to user space; the
//! result is returned in `rax`.

use coco_abi::layout::{PAGE_SIZE, USER_TOP};
use coco_abi::syscall::{self as abi, Error, validate_user_range};
use coco_arch::cpu as arch;

use crate::{console, interrupts, ipc, process, sched};

/// Registers saved by the entry stub (lowest address first).
#[derive(Debug)]
#[repr(C)]
pub struct SyscallFrame {
    pub r9: u64,
    pub r8: u64,
    pub r10: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    /// System-call number.
    pub rax: u64,
    /// User RFLAGS (restored by `sysret`).
    pub r11: u64,
    /// User return address (restored by `sysret`).
    pub rcx: u64,
    pub user_rsp: u64,
}

core::arch::global_asm!(
    r#"
.section .text
.global coco_syscall_entry
coco_syscall_entry:
    swapgs
    mov gs:[8], rsp
    mov rsp, gs:[0]
    push qword ptr gs:[8]
    swapgs
    push rcx
    push r11
    push rax
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9
    mov rdi, rsp
    sti
    call {dispatch}
    cli
    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    add rsp, 8
    pop r11
    pop rcx
    pop rsp
    sysretq
"#,
    dispatch = sym syscall_dispatch,
);

unsafe extern "C" {
    static coco_syscall_entry: [u8; 0];
}

/// Address of the SYSCALL entry stub (for IA32_LSTAR).
pub fn entry_address() -> u64 {
    (&raw const coco_syscall_entry) as u64
}

fn smap_enabled() -> bool {
    arch::read_cr4() & arch::CR4_SMAP != 0
}

/// Checks that `[ptr, ptr + len)` is user space mapped user-writable in
/// the current process.
///
/// # Errors
/// [`Error::BadAddress`].
pub fn check_user_writable(ptr: u64, len: u64) -> Result<(), Error> {
    validate_user_range(ptr, len)?;
    if !process::user_range_mapped(ptr, len, true) {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// Copies `src` to user address `dst` after the same checks as
/// [`check_user_writable`].
///
/// # Errors
/// [`Error::BadAddress`].
pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), Error> {
    let len = src.len() as u64;
    check_user_writable(dst, len)?;
    let smap = smap_enabled();
    // SAFETY: the destination was validated as user-writable mapped memory
    // of the current single-threaded process; `src` is a kernel buffer.
    // The SMAP window is opened only around the copy, and only when SMAP
    // (and therefore STAC) is supported.
    unsafe {
        core::arch::asm!(
            "test {smap}, {smap}",
            "jz 2f",
            "stac",
            "2:",
            "rep movsb",
            "test {smap}, {smap}",
            "jz 3f",
            "clac",
            "3:",
            smap = in(reg) u64::from(smap),
            inout("rsi") src.as_ptr() => _,
            inout("rdi") dst => _,
            inout("rcx") len => _,
            options(nostack),
        );
    }
    Ok(())
}

/// Copies `dst.len()` bytes from user address `src` after validating that
/// the whole range is inside user space and mapped user-accessible.
///
/// # Errors
/// [`Error::BadAddress`].
pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), Error> {
    let len = dst.len() as u64;
    validate_user_range(src, len)?;
    if !process::user_range_mapped(src, len, false) {
        return Err(Error::BadAddress);
    }
    let smap = smap_enabled();
    // SAFETY: the source range was validated to be user-accessible mapped
    // memory of the current (single-threaded) process, so it cannot be
    // unmapped concurrently; `dst` is a kernel buffer of `len` bytes. STAC
    // opens the SMAP window only around this copy (CLAC closes it) and is
    // executed only when SMAP (and therefore STAC) is supported.
    unsafe {
        core::arch::asm!(
            "test {smap}, {smap}",
            "jz 2f",
            "stac",
            "2:",
            "rep movsb",
            "test {smap}, {smap}",
            "jz 3f",
            "clac",
            "3:",
            smap = in(reg) u64::from(smap),
            inout("rsi") src => _,
            inout("rdi") dst.as_mut_ptr() => _,
            inout("rcx") len => _,
            options(nostack),
        );
    }
    Ok(())
}

fn debug_write(ptr: u64, len: u64) -> Result<u64, Error> {
    if len > abi::MAX_DEBUG_WRITE {
        return Err(Error::InvalidArgument);
    }
    let mut buf = [0u8; abi::MAX_DEBUG_WRITE as usize];
    let dst = buf.get_mut(..len as usize).ok_or(Error::InvalidArgument)?;
    copy_from_user(dst, ptr)?;
    // Neutralise control bytes (terminal escape injection) except newline.
    for b in dst.iter_mut() {
        if !(*b == b'\n' || (0x20..0x7F).contains(b)) {
            *b = b'?';
        }
    }
    let text = core::str::from_utf8(dst).unwrap_or("?");
    console::print(format_args!("{text}"));
    Ok(len)
}

fn dispatch(f: &SyscallFrame) -> Result<u64, Error> {
    match f.rax {
        abi::DEBUG_WRITE => debug_write(f.rdi, f.rsi),
        abi::EXIT => process::exit_current(f.rdi),
        abi::YIELD => {
            sched::yield_now();
            Ok(0)
        }
        abi::GET_PID => Ok(sched::current_process().unwrap_or(0)),
        abi::UPTIME_TICKS => Ok(interrupts::ticks()),
        abi::SLEEP_TICKS => {
            if f.rdi > abi::MAX_SLEEP_TICKS {
                return Err(Error::InvalidArgument);
            }
            if f.rdi == 0 {
                sched::yield_now();
            } else {
                sched::sleep(f.rdi);
            }
            Ok(0)
        }
        abi::CHANNEL_CREATE => ipc::sys_channel_create(f.rdi),
        abi::CHANNEL_SEND => ipc::sys_channel_send(f.rdi, f.rsi, f.rdx, f.r10, f.r8),
        abi::CHANNEL_RECV => ipc::sys_channel_recv(f.rdi, f.rsi, f.rdx, f.r10, f.r8),
        abi::HANDLE_CLOSE => ipc::sys_handle_close(f.rdi),
        abi::HANDLE_DUPLICATE => ipc::sys_handle_duplicate(f.rdi, f.rsi),
        abi::WAIT_READABLE => ipc::sys_wait_readable(f.rdi, f.rsi),
        abi::HANDLE_RIGHTS => ipc::sys_handle_rights(f.rdi),
        _ => Err(Error::NoSuchCall),
    }
}

/// Rust side of the SYSCALL entry (interrupts enabled, kernel stack).
extern "C" fn syscall_dispatch(f: &mut SyscallFrame) -> i64 {
    // SYSRET to a non-canonical RCX faults in ring 0 on some CPUs: refuse
    // to return anywhere outside user space.
    if f.rcx >= USER_TOP || f.user_rsp > USER_TOP + PAGE_SIZE {
        process::kill_current("invalid syscall return state", f.rcx, f.user_rsp, 0);
    }
    match dispatch(f) {
        Ok(v) => i64::try_from(v).unwrap_or(i64::MAX),
        Err(e) => e.code(),
    }
}
