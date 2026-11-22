//! Kernel console: COM1 serial plus the framebuffer text console.
//!
//! Lock rule L2: normal code uses [`print`] (blocking lock). Trap and panic
//! paths use [`emergency_print`], which never blocks: if the console lock
//! is held by the interrupted context it writes straight to the UART.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use coco_arch::serial::{COM1, SerialPort};
use coco_sync::SpinLock;

use crate::fb::{FbConsole, Framebuffer};

struct Console {
    serial: Option<SerialPort>,
    fb: Option<FbConsole>,
}

static CONSOLE: SpinLock<Console> = SpinLock::new(Console { serial: None, fb: None });
static SERIAL_PRESENT: AtomicBool = AtomicBool::new(false);

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(port) = &self.serial {
            port.write_bytes(s.as_bytes());
        }
        if let Some(fb) = &mut self.fb {
            fb.write_bytes(s.as_bytes());
        }
        Ok(())
    }
}

/// Probes COM1. Called once, first thing at kernel entry.
pub fn init_serial() -> bool {
    // SAFETY: after ExitBootServices the kernel owns all legacy I/O ports;
    // nothing else drives COM1.
    let port = unsafe { SerialPort::init(COM1) };
    let present = port.is_some();
    CONSOLE.lock().serial = port;
    SERIAL_PRESENT.store(present, Ordering::Release);
    present
}

/// Attaches the framebuffer console (draws the splash).
pub fn attach_framebuffer(fb: Framebuffer) {
    let console = FbConsole::new(fb);
    CONSOLE.lock().fb = Some(console);
}

/// Updates the splash progress bar.
pub fn progress(done: usize, total: usize) {
    if let Some(fb) = &mut CONSOLE.lock().fb {
        fb.progress(done, total);
    }
}

/// Formats to every console device.
pub fn print(args: fmt::Arguments<'_>) {
    let _ = CONSOLE.lock().write_fmt(args);
}

/// Non-blocking print for trap/panic context.
pub fn emergency_print(args: fmt::Arguments<'_>) {
    if let Some(mut c) = CONSOLE.try_lock() {
        let _ = c.write_fmt(args);
    } else if SERIAL_PRESENT.load(Ordering::Acquire) {
        // SAFETY: COM1 was probed by `init_serial` (SERIAL_PRESENT); bytes
        // may interleave with the interrupted writer, nothing worse.
        let mut port = unsafe { SerialPort::assume_init(COM1) };
        let _ = port.write_fmt(args);
    }
}

/// Runs `f` while holding the console lock (self-test of lock rule L2:
/// a fault inside `f` must still be reported, not deadlock).
pub fn with_console_locked(f: impl FnOnce()) {
    let _guard = CONSOLE.lock();
    f();
}

/// Paints the fatal-error banner if the console is free.
pub fn emergency_banner(text: &str) {
    if let Some(mut c) = CONSOLE.try_lock()
        && let Some(fb) = &mut c.fb
    {
        fb.alert(text);
    }
}

/// `println!`-style macro writing to all consoles.
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {
        $crate::console::print(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// Non-blocking variant for trap and panic paths.
#[macro_export]
macro_rules! kprintln_emergency {
    ($($arg:tt)*) => {
        $crate::console::emergency_print(format_args!("{}\n", format_args!($($arg)*)))
    };
}
