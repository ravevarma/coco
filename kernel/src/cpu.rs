//! Per-CPU descriptor tables (GDT, TSS, IDT) and CPU exception handling.
//!
//! Exception entry stubs are written in assembly: each pushes a uniform
//! [`TrapFrame`] and calls [`trap_dispatch`]. Double fault, NMI and machine
//! check run on dedicated IST stacks so they are delivered even when the
//! interrupted kernel stack is exhausted (e.g. hit its guard page).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use coco_abi::layout::{BOOT_STACK_GUARD, PAGE_SIZE};
use coco_arch::cpu;
use coco_arch::desc::{
    DescriptorTablePointer, Gate, KERNEL_CODE, KERNEL_DATA, Tss, USER_CODE32, USER_CODE64, USER_DATA, tss_descriptor,
};

use crate::kprintln_emergency;

/// Kernel code selector.
pub const KERNEL_CS: u16 = 0x08;
/// Kernel data selector.
pub const KERNEL_DS: u16 = 0x10;
/// TSS selector.
pub const TSS_SEL: u16 = 0x30;
/// SYSRET selector base (STAR[63:48]); user SS = base+8, user CS = base+16.
pub const USER_SELECTOR_BASE: u16 = 0x18;
/// User data selector with RPL 3.
pub const USER_DS: u16 = 0x20 | 3;
/// User 64-bit code selector with RPL 3.
pub const USER_CS: u16 = 0x28 | 3;

const IST_STACK_SIZE: usize = 16 * 1024;
const IST_DOUBLE_FAULT: u8 = 1;
const IST_NMI: u8 = 2;
const IST_MACHINE_CHECK: u8 = 3;

#[repr(C, align(16))]
struct CpuTables {
    gdt: [u64; 8],
    tss: Tss,
    idt: [Gate; 256],
}

/// Interrupt-stack-table stacks, each preceded by a guard page that
/// [`arm_ist_guards`] unmaps once the kernel controls paging. Kept separate
/// from [`CpuTables`] so they are all-zero and land in `.bss`.
#[repr(C, align(4096))]
struct IstStacks {
    guard_double_fault: [u8; PAGE_SIZE as usize],
    double_fault: [u8; IST_STACK_SIZE],
    guard_nmi: [u8; PAGE_SIZE as usize],
    nmi: [u8; IST_STACK_SIZE],
    guard_machine_check: [u8; PAGE_SIZE as usize],
    machine_check: [u8; IST_STACK_SIZE],
}

/// Invariant (C2): only the CPU reads/writes these stacks (exception
/// delivery and handlers running on them); Rust code only computes their
/// addresses and never forms references to their contents.
struct StacksCell(UnsafeCell<IstStacks>);

// SAFETY: invariant C2 — no Rust-level access to the contents exists.
unsafe impl Sync for StacksCell {}

static IST_STACKS: StacksCell = StacksCell(UnsafeCell::new(IstStacks {
    guard_double_fault: [0; PAGE_SIZE as usize],
    guard_nmi: [0; PAGE_SIZE as usize],
    guard_machine_check: [0; PAGE_SIZE as usize],
    double_fault: [0; IST_STACK_SIZE],
    nmi: [0; IST_STACK_SIZE],
    machine_check: [0; IST_STACK_SIZE],
}));

/// Interior-mutable holder for the boot CPU's tables.
///
/// Invariant (C1): mutated only inside [`init`], which runs exactly once on
/// the boot CPU with interrupts disabled before any other code can observe
/// the tables; afterwards Rust code never forms references to them (the
/// CPU itself reads the tables and writes the TSS busy bit).
struct TablesCell(UnsafeCell<CpuTables>);

// SAFETY: see invariant C1 — no concurrent Rust access is possible.
unsafe impl Sync for TablesCell {}

static CPU0: TablesCell =
    TablesCell(UnsafeCell::new(CpuTables { gdt: [0; 8], tss: Tss::new(), idt: [Gate::MISSING; 256] }));
static INITIALIZED: AtomicBool = AtomicBool::new(false);
/// Number of #BP exceptions handled (observable by self-tests).
pub static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);
/// Nesting depth of hardware-interrupt handlers (vectors >= 32).
static IN_INTERRUPT: AtomicU32 = AtomicU32::new(0);

/// True while a hardware-interrupt handler is running on this CPU.
pub fn in_interrupt() -> bool {
    IN_INTERRUPT.load(Ordering::Relaxed) > 0
}

/// First vector available for hardware interrupts.
pub const FIRST_IRQ_VECTOR: u8 = 32;
/// Bytes between consecutive IRQ entry stubs (`.p2align 4`).
const IRQ_STUB_STRIDE: u64 = 16;

/// Register state saved by the exception entry stubs (lowest address first).
#[derive(Debug)]
#[repr(C)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// Exception entry stubs. Vectors 8, 10-14, 17, 21, 29 and 30 push a CPU
// error code; the others push a zero so every frame has the same layout.
// Stack alignment: the CPU aligns RSP to 16 before pushing its 5-word frame;
// error code + vector + 15 GPRs = 17 more words, so RSP is 16-byte aligned
// at the `call`, as the System V ABI requires.
core::arch::global_asm!(
    r#"
.macro TRAP_NOERR n
    .p2align 4
trap_stub_\n:
    push 0
    push \n
    jmp trap_common
.endm
.macro TRAP_ERR n
    .p2align 4
trap_stub_\n:
    push \n
    jmp trap_common
.endm

.section .text
trap_common:
    push rax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    cld
    call {dispatch}
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    add rsp, 16
    iretq

TRAP_NOERR 0
TRAP_NOERR 1
TRAP_NOERR 2
TRAP_NOERR 3
TRAP_NOERR 4
TRAP_NOERR 5
TRAP_NOERR 6
TRAP_NOERR 7
TRAP_ERR 8
TRAP_NOERR 9
TRAP_ERR 10
TRAP_ERR 11
TRAP_ERR 12
TRAP_ERR 13
TRAP_ERR 14
TRAP_NOERR 15
TRAP_NOERR 16
TRAP_ERR 17
TRAP_NOERR 18
TRAP_NOERR 19
TRAP_NOERR 20
TRAP_ERR 21
TRAP_NOERR 22
TRAP_NOERR 23
TRAP_NOERR 24
TRAP_NOERR 25
TRAP_NOERR 26
TRAP_NOERR 27
TRAP_NOERR 28
TRAP_ERR 29
TRAP_ERR 30
TRAP_NOERR 31

// Hardware-interrupt stubs for vectors 32..=255, 16 bytes apart:
// push 0 (error code), push vector, jmp trap_common.
    .p2align 4
.global coco_irq_stubs
coco_irq_stubs:
.set irqvec, 32
.rept 224
    .p2align 4
    push 0
    push irqvec
    jmp trap_common
    .set irqvec, irqvec + 1
.endr

.section .rodata
.p2align 3
.global coco_trap_stubs
coco_trap_stubs:
    .quad trap_stub_0, trap_stub_1, trap_stub_2, trap_stub_3
    .quad trap_stub_4, trap_stub_5, trap_stub_6, trap_stub_7
    .quad trap_stub_8, trap_stub_9, trap_stub_10, trap_stub_11
    .quad trap_stub_12, trap_stub_13, trap_stub_14, trap_stub_15
    .quad trap_stub_16, trap_stub_17, trap_stub_18, trap_stub_19
    .quad trap_stub_20, trap_stub_21, trap_stub_22, trap_stub_23
    .quad trap_stub_24, trap_stub_25, trap_stub_26, trap_stub_27
    .quad trap_stub_28, trap_stub_29, trap_stub_30, trap_stub_31
.section .text
"#,
    dispatch = sym trap_dispatch,
);

unsafe extern "C" {
    /// Addresses of the 32 exception entry stubs (defined above).
    static coco_trap_stubs: [u64; 32];
    /// First of 224 hardware-interrupt stubs (defined above).
    static coco_irq_stubs: [u8; 0];
}

/// Entry address of the IRQ stub for `vector` (>= 32).
fn irq_stub(vector: u8) -> u64 {
    let base = (&raw const coco_irq_stubs) as u64;
    base + u64::from(vector - FIRST_IRQ_VECTOR) * IRQ_STUB_STRIDE
}

/// Virtual addresses of the three IST guard pages.
pub fn ist_guard_pages() -> [u64; 3] {
    let base = IST_STACKS.0.get() as u64;
    [
        base + core::mem::offset_of!(IstStacks, guard_double_fault) as u64,
        base + core::mem::offset_of!(IstStacks, guard_nmi) as u64,
        base + core::mem::offset_of!(IstStacks, guard_machine_check) as u64,
    ]
}

/// Unmaps the IST guard pages so an IST stack overflow faults instead of
/// silently corrupting the neighbouring stack. The backing frames stay part
/// of the kernel image (never reused).
pub fn arm_ist_guards(mem: &mut crate::mm::KernelMemory) -> bool {
    let mut ok = true;
    for page in ist_guard_pages() {
        match mem.mapper.unmap(page) {
            Ok((_phys, _size, flush)) => cpu::invlpg(flush.addr()),
            Err(_) => ok = false,
        }
    }
    ok
}

/// Security features enabled by [`init`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Hardening {
    /// SMEP enabled.
    pub smep: bool,
    /// SMAP enabled.
    pub smap: bool,
    /// UMIP enabled.
    pub umip: bool,
}

/// One past the highest address of the IST stack at `offset` in [`IstStacks`].
fn stack_top(offset: usize) -> u64 {
    (IST_STACKS.0.get() as u64).wrapping_add((offset + IST_STACK_SIZE) as u64)
}

/// Errors from [`init`].
#[derive(Debug)]
pub enum CpuInitError {
    /// `init` was called twice.
    AlreadyInitialized,
    #[allow(dead_code, reason = "payload is read through the Debug impl")]
    /// The assembled IRQ stubs do not have the expected layout.
    BadIrqStubs(u8),
}

/// Builds and loads the GDT, TSS and IDT for the boot CPU.
///
/// # Errors
/// [`CpuInitError::AlreadyInitialized`] on a second call.
pub fn init() -> Result<Hardening, CpuInitError> {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return Err(CpuInitError::AlreadyInitialized);
    }
    cpu::disable_interrupts();
    // SAFETY: invariant C1 — first and only call (guarded by INITIALIZED),
    // single CPU, interrupts disabled, so this is the only reference.
    let t = unsafe { &mut *CPU0.0.get() };

    t.tss.ist[usize::from(IST_DOUBLE_FAULT - 1)] = stack_top(core::mem::offset_of!(IstStacks, double_fault));
    t.tss.ist[usize::from(IST_NMI - 1)] = stack_top(core::mem::offset_of!(IstStacks, nmi));
    t.tss.ist[usize::from(IST_MACHINE_CHECK - 1)] = stack_top(core::mem::offset_of!(IstStacks, machine_check));

    let tss_addr = core::ptr::addr_of!(t.tss) as u64;
    let [tss_lo, tss_hi] = tss_descriptor(tss_addr, (core::mem::size_of::<Tss>() - 1) as u32);
    t.gdt = [0, KERNEL_CODE, KERNEL_DATA, USER_CODE32, USER_DATA, USER_CODE64, tss_lo, tss_hi];

    // SAFETY: `coco_trap_stubs` is an immutable table emitted by the
    // global_asm block above.
    let stubs = unsafe { &coco_trap_stubs };
    for (vector, (gate, &handler)) in t.idt.iter_mut().zip(stubs.iter()).enumerate() {
        let ist = match vector {
            2 => IST_NMI,
            8 => IST_DOUBLE_FAULT,
            18 => IST_MACHINE_CHECK,
            _ => 0,
        };
        *gate = Gate::interrupt(handler, KERNEL_CS, ist, 0);
    }
    for vector in FIRST_IRQ_VECTOR..=255 {
        let stub = irq_stub(vector);
        // SAFETY: `stub` lies in the kernel text emitted by global_asm
        // (readable); a single byte is read to verify the layout.
        let first = unsafe { (stub as *const u8).read_volatile() };
        if first != 0x6A {
            // Expected `push imm8` (0x6A): the stubs are not 16 bytes apart.
            return Err(CpuInitError::BadIrqStubs(vector));
        }
        if let Some(gate) = t.idt.get_mut(usize::from(vector)) {
            *gate = Gate::interrupt(stub, KERNEL_CS, 0, 0);
        }
    }

    let gdtr =
        DescriptorTablePointer { limit: (core::mem::size_of_val(&t.gdt) - 1) as u16, base: t.gdt.as_ptr() as u64 };
    let idtr =
        DescriptorTablePointer { limit: (core::mem::size_of_val(&t.idt) - 1) as u16, base: t.idt.as_ptr() as u64 };

    // SAFETY: the GDT above contains valid 64-bit code/data descriptors at
    // the selectors used; it lives in a static (never freed). The far
    // return reloads CS; data segments are reloaded with the data selector
    // (fs/gs zeroed, base MSRs untouched).
    unsafe {
        core::arch::asm!(
            "lgdt [{gdtr}]",
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov ss, {ds:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            gdtr = in(reg) &gdtr,
            cs = const KERNEL_CS as u64,
            ds = in(reg) u64::from(KERNEL_DS),
            tmp = out(reg) _,
        );
    }
    // SAFETY: selector 0x18 is the available 64-bit TSS descriptor built
    // above; `ltr` marks it busy (CPU-side write, allowed by C1).
    unsafe { core::arch::asm!("ltr {0:x}", in(reg) TSS_SEL, options(nostack, preserves_flags)) };
    // SAFETY: the IDT is fully populated for vectors 0-31 with valid stubs
    // and the rest are non-present gates; it lives in a static.
    unsafe { core::arch::asm!("lidt [{0}]", in(reg) &idtr, options(nostack, preserves_flags)) };

    let f = cpu::features();
    let mut cr4 = cpu::read_cr4();
    let hardening = Hardening { smep: f.smep, smap: f.smap, umip: f.umip };
    if f.smep {
        cr4 |= cpu::CR4_SMEP;
    }
    if f.smap {
        cr4 |= cpu::CR4_SMAP;
    }
    if f.umip {
        cr4 |= cpu::CR4_UMIP;
    }
    // x87/SSE are available to user mode and saved per thread with
    // FXSAVE; XSAVE-managed state (AVX...) and FSGSBASE are kept disabled
    // because the per-thread save area does not cover them.
    cr4 |= cpu::CR4_OSFXSR | cpu::CR4_OSXMMEXCPT;
    cr4 &= !(cpu::CR4_OSXSAVE | cpu::CR4_FSGSBASE);
    // SAFETY: only features CPUID reports are enabled. SMEP/SMAP cannot
    // fault the kernel (it touches user pages only inside STAC/CLAC
    // windows); UMIP affects CPL 3 only; the kernel itself never uses
    // SIMD/AVX (soft-float target).
    unsafe { cpu::write_cr4(cr4) };
    // SAFETY: CR0.EM clear / MP set is the documented FXSAVE/SSE setup.
    unsafe { cpu::write_cr0((cpu::read_cr0() & !cpu::CR0_EM) | cpu::CR0_MP) };
    init_syscall_msrs();
    Ok(hardening)
}

/// Per-CPU data reached with `swapgs` from the syscall entry stub:
/// offset 0 = kernel stack top of the current thread, offset 8 = scratch
/// for the user RSP.
///
/// Invariant (C5): written only with interrupts disabled (context switch);
/// read by the syscall stub with interrupts disabled (FMASK clears IF).
struct PerCpu(UnsafeCell<[u64; 2]>);

// SAFETY: invariant C5 — single CPU, accesses serialized by disabled
// interrupts.
unsafe impl Sync for PerCpu {}

static PER_CPU: PerCpu = PerCpu(UnsafeCell::new([0; 2]));

fn init_syscall_msrs() {
    let star = (u64::from(USER_SELECTOR_BASE) << 48) | (u64::from(KERNEL_CS) << 32);
    // RFLAGS cleared on entry: TF, IF, DF, IOPL, NT, AC (AC so user code
    // cannot pre-disable SMAP for the kernel).
    let fmask: u64 = 0x0004_7700;
    let entry = crate::syscall::entry_address();
    // SAFETY: EFER exists on every x86_64 CPU; CPL 0.
    let efer = unsafe { cpu::rdmsr(cpu::MSR_EFER) };
    // SAFETY: CPL 0; this and the MSR writes below only configure
    // SYSCALL/SYSRET and the GS bases. The entry point is the assembly stub
    // in `syscall.rs`, and PER_CPU is a static.
    unsafe { cpu::wrmsr(cpu::MSR_EFER, efer | cpu::EFER_SCE) };
    // SAFETY: as above.
    unsafe { cpu::wrmsr(cpu::MSR_STAR, star) };
    // SAFETY: as above.
    unsafe { cpu::wrmsr(cpu::MSR_LSTAR, entry) };
    // SAFETY: as above.
    unsafe { cpu::wrmsr(cpu::MSR_FMASK, fmask) };
    // SAFETY: as above.
    unsafe { cpu::wrmsr(cpu::MSR_KERNEL_GS_BASE, PER_CPU.0.get() as u64) };
    // SAFETY: as above.
    unsafe { cpu::wrmsr(cpu::MSR_GS_BASE, 0) };
}

/// Installs `top` as the stack used on entry to ring 0 (interrupts from
/// user mode via TSS.RSP0, and syscalls via the per-CPU slot).
/// Must be called with interrupts disabled (context switch).
pub fn set_kernel_stack(top: u64) {
    let tables = CPU0.0.get();
    // SAFETY: invariant C1 (amended): TSS.RSP0 is the one field updated
    // after `init`, only here, with interrupts disabled on the only CPU.
    // The TSS is packed, so the field is written unaligned through a raw
    // pointer without creating a reference.
    let rsp0 = unsafe { core::ptr::addr_of_mut!((*tables).tss.rsp) }.cast::<u64>();
    // SAFETY: as above; `rsp0` points at TSS.RSP0 inside the static.
    unsafe { rsp0.write_unaligned(top) };
    let slot = PER_CPU.0.get().cast::<u64>();
    // SAFETY: invariant C5; offset 0 of the per-CPU block.
    unsafe { slot.write(top) };
}

fn exception_name(vector: u64) -> &'static str {
    match vector {
        0 => "divide error (#DE)",
        1 => "debug (#DB)",
        2 => "non-maskable interrupt",
        3 => "breakpoint (#BP)",
        4 => "overflow (#OF)",
        5 => "bound range (#BR)",
        6 => "invalid opcode (#UD)",
        7 => "device not available (#NM)",
        8 => "DOUBLE FAULT (#DF)",
        10 => "invalid TSS (#TS)",
        11 => "segment not present (#NP)",
        12 => "stack-segment fault (#SS)",
        13 => "general protection (#GP)",
        14 => "PAGE FAULT (#PF)",
        16 => "x87 floating point (#MF)",
        17 => "alignment check (#AC)",
        18 => "MACHINE CHECK (#MC)",
        19 => "SIMD floating point (#XM)",
        20 => "virtualization (#VE)",
        21 => "control protection (#CP)",
        _ => "reserved exception",
    }
}

fn in_guard_page(addr: u64) -> bool {
    (BOOT_STACK_GUARD..BOOT_STACK_GUARD + PAGE_SIZE).contains(&addr)
}

/// Common Rust exception handler (called from `trap_common`).
#[unsafe(no_mangle)]
extern "C" fn trap_dispatch(frame: &mut TrapFrame) {
    match frame.vector {
        3 => {
            BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
            kprintln_emergency!("trap: breakpoint at {:#x} (handled, resuming)", frame.rip.wrapping_sub(1));
        }
        2 => kprintln_emergency!("trap: NMI at {:#x} (ignored)", frame.rip),
        v @ 32..=255 => {
            IN_INTERRUPT.fetch_add(1, Ordering::Relaxed);
            crate::interrupts::handle(v as u8);
            IN_INTERRUPT.fetch_sub(1, Ordering::Relaxed);
            if v == u64::from(crate::interrupts::TIMER_VECTOR) {
                // Preemption point: no locks are held (IrqSpinLock holders
                // run with interrupts disabled, so the tick cannot land
                // inside them) and the interrupted state is in `frame`
                // on this thread's own kernel stack.
                crate::sched::preempt();
            }
        }
        _ if frame.cs & 3 == 3 => {
            // Isolation: an exception raised in user mode terminates the
            // offending process; the kernel continues.
            let cr2 = if frame.vector == 14 { cpu::read_cr2() } else { 0 };
            crate::process::kill_current(exception_name(frame.vector), frame.rip, cr2, frame.error_code)
        }
        _ => fatal_exception(frame),
    }
}

fn fatal_exception(f: &TrapFrame) -> ! {
    let cr2 = cpu::read_cr2();
    crate::console::emergency_banner("coco: fatal CPU exception - system halted");
    kprintln_emergency!("");
    kprintln_emergency!("FATAL: {} (vector {}), error code {:#x}", exception_name(f.vector), f.vector, f.error_code);
    if f.vector == 14 {
        let e = f.error_code;
        kprintln_emergency!(
            "  fault address {:#x}: {} {} in {} mode{}{}",
            cr2,
            if e & 1 != 0 { "protection violation" } else { "page not present" },
            if e & (1 << 4) != 0 {
                "on instruction fetch"
            } else if e & 2 != 0 {
                "on write"
            } else {
                "on read"
            },
            if e & 4 != 0 { "user" } else { "kernel" },
            if e & 8 != 0 { ", reserved bit set" } else { "" },
            if e & (1 << 5) != 0 { ", protection key" } else { "" },
        );
    }
    if (f.vector == 14 || f.vector == 8) && in_guard_page(cr2) {
        kprintln_emergency!("  cause: kernel stack overflow (guard page {:#x} hit)", BOOT_STACK_GUARD);
    }
    kprintln_emergency!("  rip {:#018x}  cs {:#06x}  rflags {:#010x}", f.rip, f.cs, f.rflags);
    kprintln_emergency!("  rsp {:#018x}  ss {:#06x}  cr2 {:#018x}  cr3 {:#018x}", f.rsp, f.ss, cr2, cpu::read_cr3());
    kprintln_emergency!("  rax {:#018x} rbx {:#018x} rcx {:#018x} rdx {:#018x}", f.rax, f.rbx, f.rcx, f.rdx);
    kprintln_emergency!("  rsi {:#018x} rdi {:#018x} rbp {:#018x} r8  {:#018x}", f.rsi, f.rdi, f.rbp, f.r8);
    kprintln_emergency!("  r9  {:#018x} r10 {:#018x} r11 {:#018x} r12 {:#018x}", f.r9, f.r10, f.r11, f.r12);
    kprintln_emergency!("  r13 {:#018x} r14 {:#018x} r15 {:#018x}", f.r13, f.r14, f.r15);
    crate::fatal_stop()
}
