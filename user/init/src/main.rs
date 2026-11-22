//! coco `init`: the first user-space program.
//!
//! Until coco has a filesystem and process spawning from user space, this
//! one binary doubles as the user-mode test workload: the kernel starts it
//! with a mode in `rdi` and an argument in `rsi` (see
//! `coco_abi::syscall::init_mode`).
#![no_std]
#![no_main]

use core::arch::asm;
use core::hint::black_box;

use coco_abi::syscall::{self, Error, init_mode};

fn sys(n: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: `syscall` transfers to the kernel, which validates every
    // argument; only rax (result), rcx and r11 (clobbered by the
    // instruction) change in this context.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") n as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

fn write(s: &str) {
    sys(syscall::DEBUG_WRITE, s.as_ptr() as u64, s.len() as u64, 0);
}

fn exit(code: u64) -> ! {
    sys(syscall::EXIT, code, 0, 0);
    // EXIT never returns; if it did, fault deliberately.
    loop {
        core::hint::spin_loop();
    }
}

fn uptime() -> u64 {
    sys(syscall::UPTIME_TICKS, 0, 0, 0) as u64
}

/// Writes `n` in decimal into `buf`, returning the used tail.
fn fmt_u64(mut n: u64, buf: &mut [u8; 20]) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 || i == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

fn raw_read(addr: u64) -> u64 {
    let v: u64;
    // SAFETY: deliberately faulting probe for isolation tests; the kernel
    // terminates this process on the resulting exception.
    unsafe { asm!("mov {v}, qword ptr [{a}]", a = in(reg) addr, v = out(reg) v, options(nostack, readonly)) };
    v
}

#[inline(never)]
#[allow(unconditional_recursion, reason = "deliberate user stack overflow test")]
fn recurse(depth: u64) -> u64 {
    let frame = black_box([depth; 64]);
    recurse(black_box(depth + 1)).wrapping_add(frame[3])
}

static RODATA_RET: [u8; 1] = [0xC3];

fn syscall_fuzz(seed: u64) -> ! {
    let mut x = seed | 1;
    let mut rnd = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let buf = [b'.'; 64];
    let hostile = [0u64, 0x10, 0xFFFF_FFFF_8000_0000, 0xFFFF_8000_0000_0000, u64::MAX, 0x7FFF_FFFF_F000, 0x40_0000];
    let (mut ok, mut errs) = (0u64, 0u64);
    for _ in 0..20_000 {
        let r = rnd();
        let n = match r % 13 {
            k @ 0..=11 => k,
            _ => rnd(), // completely random number
        };
        if n == syscall::EXIT {
            continue;
        }
        let pick = |v: u64| match v % 4 {
            0 => hostile[(v >> 8) as usize % hostile.len()],
            1 => buf.as_ptr() as u64 + (v >> 8) % 64,
            2 => v >> 3,
            _ => v,
        };
        let (a0, a1) = (pick(rnd()), pick(rnd()));
        // Sleep a real tick only rarely (1/64); otherwise sleep(0) = yield.
        let (a0, a1) = if n == syscall::SLEEP_TICKS { (u64::from(a0 % 64 == 0), a1) } else { (a0, a1) };
        // Hostile pointers are kept; lengths are capped at 1 byte so valid
        // writes do not flood the console (oversized lengths are tested by
        // the kernel's own argument checks via random syscall numbers).
        let (a0, a1) =
            if n == syscall::DEBUG_WRITE && a1 > 1 && a1 <= syscall::MAX_DEBUG_WRITE { (a0, a1 % 2) } else { (a0, a1) };
        let ret = sys(n, a0, a1, rnd());
        if ret >= 0 {
            ok += 1;
        } else if Error::from_code(ret).is_some() {
            errs += 1;
        } else {
            write("init: syscall fuzz: invalid error code\n");
            exit(1);
        }
    }
    let mut b = [0u8; 20];
    write("\ninit: syscall fuzz done, ok=");
    write(fmt_u64(ok, &mut b));
    write(" errors=");
    write(fmt_u64(errs, &mut b));
    write("\n");
    exit(0)
}

/// User entry: `mode` in rdi, `arg` in rsi.
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(mode: u64, arg: u64) -> ! {
    let mut b = [0u8; 20];
    match mode {
        init_mode::HELLO => {
            write("init: hello from user space, pid ");
            write(fmt_u64(sys(syscall::GET_PID, 0, 0, 0) as u64, &mut b));
            write("\n");
            exit(0)
        }
        init_mode::SPIN => {
            let start = uptime();
            let mut acc = 0u64;
            while uptime().wrapping_sub(start) < arg {
                for i in 0..10_000u64 {
                    acc = black_box(acc.wrapping_mul(31).wrapping_add(i));
                }
            }
            black_box(acc);
            write("init: spinner finished\n");
            exit(0)
        }
        init_mode::FAULT_NULL => {
            black_box(raw_read(0x10));
        }
        init_mode::FAULT_KERNEL_READ => {
            black_box(raw_read(0xFFFF_FFFF_8000_0000));
        }
        init_mode::FAULT_PRIVILEGED => {
            // SAFETY: deliberate privileged instruction; #GP kills us.
            unsafe { asm!("cli", options(nostack, nomem)) };
        }
        init_mode::FAULT_NX => {
            let code = black_box([0xC3u8; 16]);
            // SAFETY: deliberate jump into the (NX) stack; the fetch faults.
            unsafe { asm!("call {0}", in(reg) code.as_ptr(), clobber_abi("C")) };
            let _ = RODATA_RET;
        }
        init_mode::FAULT_WRITE_TEXT => {
            // SAFETY: deliberate write to read-only text; the write faults.
            unsafe { asm!("mov byte ptr [{0}], 0", in(reg) _start as *const () as u64, options(nostack)) };
        }
        init_mode::SYSCALL_FUZZ => syscall_fuzz(arg),
        init_mode::FAULT_STACK_OVERFLOW => {
            black_box(recurse(0));
        }
        init_mode::EXIT_CODE => exit(arg),
        _ => {}
    }
    write("init: expected fault did not happen\n");
    exit(255)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    write("init: panic\n");
    exit(101)
}
