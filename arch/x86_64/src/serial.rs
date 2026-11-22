//! Polled 16550-compatible UART driver (early console).

use crate::cpu::{inb, outb};

/// Standard COM1 I/O base.
pub const COM1: u16 = 0x3F8;

/// A probed and initialized UART.
#[derive(Debug)]
pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    /// Probes and initializes the UART at `base` (115200 8N1, FIFOs on).
    ///
    /// Returns `None` if no UART responds (common on laptops), detected via
    /// the scratch register and an internal-loopback echo test.
    ///
    /// # Safety
    /// The caller must own the I/O ports `base..base+8` (no other driver or
    /// firmware component may use them concurrently).
    pub unsafe fn init(base: u16) -> Option<Self> {
        let write = |off: u16, v: u8| {
            // SAFETY: the caller of `init` owns ports base..base+8; 16550
            // register accesses have no memory side effects.
            unsafe { outb(base.wrapping_add(off), v) }
        };
        let read = |off: u16| {
            // SAFETY: as for `write`.
            unsafe { inb(base.wrapping_add(off)) }
        };
        write(7, 0xA5); // scratch register probe
        if read(7) != 0xA5 {
            return None;
        }
        write(1, 0x00); // disable UART interrupts
        write(3, 0x80); // DLAB on
        write(0, 0x01); // divisor 1 = 115200 baud
        write(1, 0x00);
        write(3, 0x03); // 8N1, DLAB off
        write(2, 0xC7); // FIFO on, cleared, 14-byte threshold
        write(4, 0x1E); // internal loopback
        write(0, 0xAE);
        // Wait (bounded) for the looped-back byte to arrive.
        for _ in 0..10_000u32 {
            if read(5) & 1 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if read(0) != 0xAE {
            return None;
        }
        write(4, 0x0F); // normal mode, DTR/RTS/OUT1/OUT2
        Some(Self { base })
    }

    /// Re-creates a handle to a UART that [`SerialPort::init`] already
    /// probed successfully (used by fault handlers that cannot take locks).
    ///
    /// # Safety
    /// `init(base)` must have returned `Some` earlier, and concurrent use
    /// with another handle may interleave output bytes (but is otherwise
    /// harmless for a polled UART).
    #[must_use]
    pub const unsafe fn assume_init(base: u16) -> Self {
        Self { base }
    }

    /// Writes one byte, waiting (bounded) for the transmitter.
    pub fn write_byte(&self, byte: u8) {
        // Bounded wait: never hang the kernel on a stuck UART.
        for _ in 0..100_000u32 {
            // SAFETY: `self` proves the ports were probed and are owned.
            if unsafe { inb(self.base.wrapping_add(5)) } & 0x20 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // SAFETY: as above.
        unsafe { outb(self.base, byte) };
    }

    /// Writes bytes, translating `\n` into `\r\n`.
    pub fn write_bytes(&self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
    }
}

impl core::fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}
