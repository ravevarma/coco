//! User processes: address spaces, ELF loading, ring-3 entry, exit/kill.
//!
//! A process owns a PML4 whose kernel half is a copy of the kernel's (all
//! kernel PML4 slots are pre-populated at scheduler start, so the copies
//! share every later kernel mapping) and whose user half is private. It has
//! exactly one thread in M3.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};

use coco_abi::layout::{PAGE_SIZE, USER_BASE, USER_STACK_SIZE, USER_STACK_TOP, USER_TOP};
use coco_elf::{Executable, Policy};
use coco_memory::paging::PageFlags;

use crate::sched::{self, Tid};
use crate::sync::IrqSpinLock;
use crate::{cpu, kprintln_emergency, mm};

/// Process identifier.
pub type Pid = u64;
/// Largest user image footprint accepted.
const MAX_USER_IMAGE: u64 = 64 << 20;

/// How a process ended (or that it has not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Still running.
    Running,
    /// Called `exit(code)`.
    Exited(u64),
    /// Terminated by the kernel for a CPU exception.
    Killed(&'static str),
}

struct Process {
    root: u64,
    thread: Tid,
    entry: u64,
    mode: u64,
    arg: u64,
    status: Status,
}

/// Process-creation errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// The ELF image violates the user loading policy.
    BadImage(coco_elf::ElfError),
    /// Out of frames or page-table memory.
    OutOfMemory,
    /// Scheduler refused the thread.
    Sched(sched::SchedError),
}

static PROCESSES: IrqSpinLock<BTreeMap<Pid, Process>> = IrqSpinLock::new(BTreeMap::new());
static NEXT_PID: AtomicU64 = AtomicU64::new(1);

fn mem<T>(f: impl FnOnce(&mut mm::KernelMemory) -> Option<T>) -> Result<T, SpawnError> {
    mm::with(f).flatten().ok_or(SpawnError::OutOfMemory)
}

/// Creates a process from the ELF `image`, entering it with `mode` in
/// `rdi` and `arg` in `rsi`.
///
/// # Errors
/// See [`SpawnError`]. On error every frame allocated so far is released.
pub fn spawn(image: &[u8], mode: u64, arg: u64) -> Result<Pid, SpawnError> {
    let policy = Policy { window_start: USER_BASE, window_end: USER_TOP, max_total_bytes: MAX_USER_IMAGE };
    let exe = Executable::parse(image, &policy).map_err(SpawnError::BadImage)?;
    let root = mem(mm::KernelMemory::new_address_space)?;
    let built = build_address_space(root, &exe);
    if let Err(e) = built {
        mm::with(|m| m.teardown_user(root));
        return Err(e);
    }
    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    PROCESSES.lock().insert(pid, Process { root, thread: 0, entry: exe.entry(), mode, arg, status: Status::Running });
    match sched::spawn_user("user", user_thread_main, pid, root) {
        Ok(tid) => {
            if let Some(p) = PROCESSES.lock().get_mut(&pid) {
                p.thread = tid;
            }
            Ok(pid)
        }
        Err(e) => {
            PROCESSES.lock().remove(&pid);
            mm::with(|m| m.teardown_user(root));
            Err(SpawnError::Sched(e))
        }
    }
}

fn build_address_space(root: u64, exe: &Executable<'_>) -> Result<(), SpawnError> {
    for seg in exe.segments() {
        let mut flags = PageFlags::EMPTY;
        if seg.writable() {
            flags = flags | PageFlags::WRITABLE;
        }
        if !seg.executable() {
            flags = flags | PageFlags::NO_EXECUTE;
        }
        let data = exe.file_data(seg);
        for p in 0..seg.page_count() {
            let page_va = seg.page_start() + p * PAGE_SIZE;
            let phys = mem(|m| m.map_user_page(root, page_va, flags).ok())?;
            // Copy the part of the file data that falls into this page.
            let page_start_in_seg = (page_va as i128) - (seg.vaddr as i128);
            let from = page_start_in_seg.max(0) as u64;
            let to = (page_start_in_seg + PAGE_SIZE as i128).min(data.len() as i128).max(0) as u64;
            if to > from {
                let dst_off = (seg.vaddr + from) - page_va;
                let src = data.get(from as usize..to as usize).unwrap_or(&[]);
                let dst = coco_abi::layout::phys_to_virt(phys + dst_off).ok_or(SpawnError::OutOfMemory)?;
                // SAFETY: `phys` is a fresh frame owned by this process;
                // `dst_off + src.len() <= PAGE_SIZE` by construction; the
                // source is the (immutable) boot image.
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
            }
        }
    }
    let stack_flags = PageFlags::WRITABLE | PageFlags::NO_EXECUTE;
    let mut va = USER_STACK_TOP - USER_STACK_SIZE; // guard page below stays unmapped
    while va < USER_STACK_TOP {
        mem(|m| m.map_user_page(root, va, stack_flags).ok())?;
        va += PAGE_SIZE;
    }
    Ok(())
}

/// First code of every user thread: enters ring 3 at the image entry.
extern "C" fn user_thread_main(pid: u64) -> ! {
    let (entry, mode, arg) = match PROCESSES.lock().get(&pid) {
        Some(p) => (p.entry, p.mode, p.arg),
        None => sched::exit_current(),
    };
    // rsp ≡ 8 (mod 16) at entry, as after a `call`.
    let user_rsp = USER_STACK_TOP - 8;
    // SAFETY: the current address space (CR3 set by the scheduler) maps
    // `entry` executable+user and the user stack; the iret frame selects
    // ring-3 code/stack segments and RFLAGS with only IF (and the reserved
    // bit 1) set. All other registers are zeroed so no kernel data leaks.
    unsafe {
        core::arch::asm!(
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            ss = in(reg) u64::from(cpu::USER_DS),
            rsp = in(reg) user_rsp,
            rflags = in(reg) 0x202u64,
            cs = in(reg) u64::from(cpu::USER_CS),
            rip = in(reg) entry,
            in("rdi") mode,
            in("rsi") arg,
            options(noreturn),
        )
    }
}

fn finish_current(status: Status) -> ! {
    coco_arch::cpu::disable_interrupts();
    if let Some(pid) = sched::current_process()
        && let Some(p) = PROCESSES.lock().get_mut(&pid)
    {
        p.status = status;
    }
    sched::exit_current()
}

/// `exit` system call.
pub fn exit_current(code: u64) -> ! {
    finish_current(Status::Exited(code))
}

/// Terminates the current process after a CPU exception in user mode.
pub fn kill_current(reason: &'static str, rip: u64, address: u64, error: u64) -> ! {
    let pid = sched::current_process();
    match pid {
        Some(pid) => {
            kprintln_emergency!(
                "user: pid {} killed: {} at rip {:#x} (address {:#x}, error {:#x}); kernel continues",
                pid,
                reason,
                rip,
                address,
                error
            );
            finish_current(Status::Killed(reason))
        }
        None => {
            kprintln_emergency!("FATAL: user-mode exception without a process: {} at {:#x}", reason, rip);
            crate::fatal_stop()
        }
    }
}

/// Waits for `pid` to end, then frees its address space and records.
/// Returns its final status (`None` for an unknown pid).
pub fn wait(pid: Pid) -> Option<Status> {
    loop {
        let (status, thread, root) = {
            let procs = PROCESSES.lock();
            let p = procs.get(&pid)?;
            (p.status, p.thread, p.root)
        };
        if status != Status::Running {
            // The process's thread has switched away for good (it set the
            // status with interrupts disabled immediately before), so its
            // address space is no longer live; join to free its stack.
            sched::join(thread);
            mm::with(|m| m.teardown_user(root));
            PROCESSES.lock().remove(&pid);
            return Some(status);
        }
        sched::sleep(1);
    }
}

/// Whether `[ptr, ptr + len)` is mapped user-accessible in the current
/// process (and `writable` if requested).
pub fn user_range_mapped(ptr: u64, len: u64, writable: bool) -> bool {
    let root = coco_arch::cpu::read_cr3() & 0x000F_FFFF_FFFF_F000;
    let mut page = ptr & !(PAGE_SIZE - 1);
    let end = ptr.saturating_add(len);
    while page < end {
        let ok = mm::with(|m| m.translate_in(root, page))
            .flatten()
            .is_some_and(|t| t.flags.contains(PageFlags::USER) && (!writable || t.flags.contains(PageFlags::WRITABLE)));
        if !ok {
            return false;
        }
        page += PAGE_SIZE;
    }
    true
}

/// Number of live (not yet waited-for) processes.
pub fn count() -> usize {
    PROCESSES.lock().len()
}
