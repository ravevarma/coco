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

fn sys5(n: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    // SAFETY: `syscall` transfers to the kernel, which validates every
    // argument; only rax (result), rcx and r11 (clobbered by the
    // instruction) change in this context. The kernel may read or write
    // the buffers passed in, so this is not `nomem`.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") n as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

fn sys(n: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    sys5(n, a0, a1, a2, 0, 0)
}

fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: rdtsc is permitted in user mode (CR4.TSD clear).
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nostack, nomem, preserves_flags)) };
    (u64::from(hi) << 32) | u64::from(lo)
}

fn channel_create() -> Result<(u64, u64), i64> {
    let mut out = [0u64; 2];
    let r = sys(syscall::CHANNEL_CREATE, out.as_mut_ptr() as u64, 0, 0);
    if r < 0 { Err(r) } else { Ok((out[0], out[1])) }
}

fn send(h: u64, data: &[u8], handles: &[u64]) -> i64 {
    sys5(
        syscall::CHANNEL_SEND,
        h,
        data.as_ptr() as u64,
        data.len() as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
    )
}

/// Returns (bytes, handle count) or the error code.
fn recv(h: u64, buf: &mut [u8], handles: &mut [u64]) -> Result<(usize, usize), i64> {
    let r = sys5(
        syscall::CHANNEL_RECV,
        h,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        handles.as_mut_ptr() as u64,
        handles.len() as u64,
    );
    if r < 0 { Err(r) } else { Ok(((r as u64 & 0xFFFF_FFFF) as usize, (r as u64 >> 32) as usize)) }
}

fn close(h: u64) -> i64 {
    sys(syscall::HANDLE_CLOSE, h, 0, 0)
}

fn wait(h: u64, timeout: u64) -> i64 {
    sys(syscall::WAIT_READABLE, h, timeout, 0)
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

fn echo_service(h: u64) -> ! {
    let mut buf = [0u8; 256];
    loop {
        if wait(h, 0) < 0 {
            exit(2);
        }
        match recv(h, &mut buf, &mut []) {
            Ok((n, _)) => {
                let reply = &mut buf[..n];
                reply.make_ascii_uppercase();
                if send(h, reply, &[]) < 0 {
                    exit(3);
                }
            }
            Err(e) if e == Error::PeerClosed.code() => exit(0),
            Err(_) => exit(4),
        }
    }
}

fn echo_client(h: u64) -> ! {
    const ROUNDS: u64 = 1000;
    let mut total = 0u64;
    let mut msg = *b"hello coco ipc #00000";
    let mut buf = [0u8; 64];
    for i in 0..ROUNDS {
        let mut b = [0u8; 20];
        let digits = fmt_u64(i, &mut b).as_bytes();
        let start = msg.len() - digits.len();
        msg[start..].copy_from_slice(digits);
        let t0 = rdtsc();
        if send(h, &msg, &[]) < 0 || wait(h, 0) < 0 {
            exit(10);
        }
        let Ok((n, _)) = recv(h, &mut buf, &mut []) else { exit(11) };
        total += rdtsc().wrapping_sub(t0);
        let mut expect = msg;
        expect.make_ascii_uppercase();
        if buf[..n] != expect[..] {
            exit(12);
        }
    }
    let mut b = [0u8; 20];
    write("init: ipc echo: 1000 round trips verified, mean ");
    write(fmt_u64(total / ROUNDS, &mut b));
    write(" cycles per round trip\n");
    close(h);
    exit(0)
}

/// User-mode capability tests; exits with the number of the first failed
/// check (0 = all passed).
fn cap_tests() -> u64 {
    use coco_abi::syscall::rights::{ALL, READ, WRITE};
    let e = |err: Error| err.code();
    let check = |n: u64, ok: bool| if ok { Ok(()) } else { Err(n) };
    let run = || -> Result<(), u64> {
        let (h0, h1) = channel_create().map_err(|_| 1u64)?;
        check(1, h0 != 0 && h1 != 0 && h0 != h1)?;
        check(2, sys(syscall::HANDLE_RIGHTS, h0, 0, 0) == ALL as i64)?;
        let mut buf = [0u8; 300];
        let mut hs = [0u64; 4];
        check(3, send(h0, b"hi", &[]) == 0 && recv(h1, &mut buf, &mut hs) == Ok((2, 0)) && &buf[..2] == b"hi")?;
        check(4, recv(h1, &mut buf, &mut hs) == Err(e(Error::WouldBlock)))?;
        let ro = sys(syscall::HANDLE_DUPLICATE, h0, READ, 0);
        check(5, ro > 0 && send(ro as u64, b"x", &[]) == e(Error::AccessDenied))?;
        let ro = ro as u64;
        check(6, sys(syscall::HANDLE_DUPLICATE, ro, READ, 0) == e(Error::AccessDenied))?;
        check(7, sys(syscall::HANDLE_DUPLICATE, h0, READ | (1 << 9), 0) == e(Error::InvalidArgument))?;
        check(
            8,
            close(ro) == 0
                && sys(syscall::HANDLE_RIGHTS, ro, 0, 0) == e(Error::BadHandle)
                && close(ro) == e(Error::BadHandle),
        )?;
        check(
            9,
            [0, h0 + (1 << 32), u64::MAX, h0 ^ 0xFF]
                .iter()
                .all(|&v| sys(syscall::HANDLE_RIGHTS, v, 0, 0) == e(Error::BadHandle)),
        )?;
        check(10, send(h0, &buf[..257], &[]) == e(Error::TooLarge))?;
        for _ in 0..syscall::MAX_QUEUED_MESSAGES {
            check(11, send(h0, b"q", &[]) == 0)?;
        }
        check(11, send(h0, b"q", &[]) == e(Error::QueueFull))?;
        for _ in 0..syscall::MAX_QUEUED_MESSAGES {
            check(11, recv(h1, &mut buf, &mut hs) == Ok((1, 0)))?;
        }
        // Capability transfer: move c1 through (h0, h1) and use it.
        let (c0, c1) = channel_create().map_err(|_| 12u64)?;
        check(12, send(h0, b"cap", &[c1]) == 0)?;
        check(12, sys(syscall::HANDLE_RIGHTS, c1, 0, 0) == e(Error::BadHandle))?; // moved away
        check(12, recv(h1, &mut buf, &mut hs) == Ok((3, 1)))?;
        let r1 = hs[0];
        check(12, send(c0, b"via", &[]) == 0 && recv(r1, &mut buf, &mut hs) == Ok((3, 0)) && &buf[..3] == b"via")?;
        let nt = sys(syscall::HANDLE_DUPLICATE, c0, READ | WRITE, 0) as u64;
        check(13, send(h0, b"", &[nt]) == e(Error::AccessDenied) && sys(syscall::HANDLE_RIGHTS, nt, 0, 0) >= 0)?;
        check(
            14,
            send(h0, b"", &[h1]) == e(Error::InvalidArgument) && send(h0, b"", &[h0]) == e(Error::InvalidArgument),
        )?;
        check(15, send(h0, b"", &[c0, c0]) == e(Error::InvalidArgument))?;
        check(16, send(h0, b"", &[]) == 0)?;
        check(16, sys5(syscall::CHANNEL_SEND, h0, 0x10, 4, 0, 0) == e(Error::BadAddress))?;
        check(16, sys5(syscall::CHANNEL_RECV, h1, 0xFFFF_FFFF_8000_0000, 16, 0, 0) == e(Error::BadAddress))?;
        check(16, recv(h1, &mut buf, &mut hs) == Ok((0, 0)))?; // message survived the bad receive
        check(20, send(h0, b"0123456789", &[]) == 0)?;
        check(20, recv(h1, &mut buf[..4], &mut hs) == Err(e(Error::TooLarge)))?;
        check(20, recv(h1, &mut buf[..16], &mut hs) == Ok((10, 0)))?;
        check(17, close(h1) == 0 && send(h0, b"x", &[]) == e(Error::PeerClosed))?;
        check(17, recv(h0, &mut buf, &mut hs) == Err(e(Error::PeerClosed)) && wait(h0, 1) == 0)?;
        let (w0, w1) = channel_create().map_err(|_| 18u64)?;
        check(18, wait(w1, 2) == e(Error::TimedOut))?;
        // Handle-table limit: create channels until refused.
        let mut made = 0u64;
        loop {
            match channel_create() {
                Ok(_) => made += 2,
                Err(code) => {
                    check(19, code == e(Error::NoResources))?;
                    break;
                }
            }
            check(19, made <= syscall::MAX_HANDLES as u64)?;
        }
        check(19, made >= 200)?;
        // (21) Reference cycles across channels are refused. The handle
        // table is full now, so free some slots first.
        check(21, close(w0) == 0 && close(w1) == 0 && close(nt) == 0 && close(r1) == 0)?;
        let (a0, a1) = channel_create().map_err(|_| 21u64)?;
        let (b0, b1) = channel_create().map_err(|_| 21u64)?;
        check(21, send(a0, b"", &[b1]) == 0)?; // a holds an endpoint of b
        check(21, send(b0, b"", &[a1]) == e(Error::InvalidArgument))?; // b -> a would close a cycle
        check(21, sys(syscall::HANDLE_RIGHTS, a1, 0, 0) >= 0)?; // refused send moved nothing
        let _ = (c0, a0, b0);
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(n) => n,
    }
}

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
        let n = match r % 20 {
            k @ 0..=17 => k, // includes every IPC/capability call
            _ => rnd(),      // completely random number
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
        // A fuzzed wait must not block forever (timeout 0 = forever).
        let (a0, a1) = if n == syscall::WAIT_READABLE { (a0, 1) } else { (a0, a1) };
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
        init_mode::ECHO_SERVICE => echo_service(arg),
        init_mode::ECHO_CLIENT => echo_client(arg),
        init_mode::CAP_TESTS => {
            let failed = cap_tests();
            if failed == 0 {
                write("init: capability tests: 21 checks passed\n");
            } else {
                write("init: capability tests: check ");
                write(fmt_u64(failed, &mut b));
                write(" FAILED\n");
            }
            exit(failed)
        }
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
