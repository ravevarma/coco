//! GDT, TSS and IDT descriptor encodings.

/// 64-bit kernel code segment (L=1, P=1, DPL=0, execute/read).
pub const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF;
/// Kernel data segment (P=1, DPL=0, read/write).
pub const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF;

/// Encodes a 64-bit available TSS descriptor (two GDT slots).
#[must_use]
pub const fn tss_descriptor(base: u64, limit: u32) -> [u64; 2] {
    let limit = limit as u64;
    let low = (limit & 0xFFFF)
        | ((base & 0xFF_FFFF) << 16)
        | (0x89 << 40) // P=1, DPL=0, type=0b1001 (64-bit TSS available)
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    [low, base >> 32]
}

/// 64-bit Task State Segment.
#[derive(Clone, Copy, Debug)]
#[repr(C, packed(4))]
pub struct Tss {
    reserved0: u32,
    /// Stack pointers for privilege levels 0-2.
    pub rsp: [u64; 3],
    reserved1: u64,
    /// Interrupt stack table (IST1..IST7 at indices 0..6).
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    /// Offset of the I/O permission bitmap (>= size disables it).
    pub iomap_base: u16,
}

impl Tss {
    /// An empty TSS with the I/O bitmap disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: core::mem::size_of::<Self>() as u16,
        }
    }
}

impl Default for Tss {
    fn default() -> Self {
        Self::new()
    }
}

/// 16-byte IDT gate descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl Gate {
    /// A non-present gate (vector raises #GP / #NP if used).
    pub const MISSING: Self =
        Self { offset_low: 0, selector: 0, ist: 0, attributes: 0, offset_mid: 0, offset_high: 0, reserved: 0 };

    /// Interrupt gate (IF cleared on entry) at `handler`, using IST slot
    /// `ist` (0 = none, 1..=7) and descriptor privilege `dpl`.
    #[must_use]
    pub const fn interrupt(handler: u64, selector: u16, ist: u8, dpl: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            ist: ist & 0x7,
            attributes: 0x8E | ((dpl & 0x3) << 5),
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }

    /// Handler address encoded in the gate.
    #[must_use]
    pub const fn handler(&self) -> u64 {
        (self.offset_low as u64) | ((self.offset_mid as u64) << 16) | ((self.offset_high as u64) << 32)
    }

    /// IST index.
    #[must_use]
    pub const fn ist(&self) -> u8 {
        self.ist
    }
}

/// Operand of `lgdt` / `lidt`.
#[derive(Clone, Copy, Debug)]
#[repr(C, packed(2))]
pub struct DescriptorTablePointer {
    /// Size in bytes minus one.
    pub limit: u16,
    /// Linear base address.
    pub base: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_roundtrip() {
        let h = 0xFFFF_FFFF_8012_3456;
        let g = Gate::interrupt(h, 0x08, 1, 0);
        assert_eq!(g.handler(), h);
        assert_eq!(g.ist(), 1);
        assert_eq!(g.attributes, 0x8E);
        assert_eq!(Gate::interrupt(h, 0x08, 9, 3).ist(), 1);
        assert_eq!(Gate::interrupt(h, 0x08, 0, 3).attributes, 0xEE);
        assert_eq!(core::mem::size_of::<Gate>(), 16);
    }

    #[test]
    fn tss_layout() {
        assert_eq!(core::mem::size_of::<Tss>(), 104);
        assert_eq!(core::mem::offset_of!(Tss, ist), 36);
        assert_eq!({ Tss::new().iomap_base }, 104);
        let [lo, hi] = tss_descriptor(0xFFFF_FFFF_8000_1234, 103);
        assert_eq!(lo & 0xFFFF, 103);
        assert_eq!((lo >> 40) & 0xFF, 0x89);
        assert_eq!(((lo >> 16) & 0xFF_FFFF) | (((lo >> 56) & 0xFF) << 24) | (hi << 32), 0xFFFF_FFFF_8000_1234);
    }

    #[test]
    fn segment_descriptors() {
        // L bit (53) set and D bit (54) clear for the 64-bit code segment.
        assert_eq!((KERNEL_CODE >> 53) & 3, 1);
        assert_eq!((KERNEL_CODE >> 47) & 1, 1);
        assert_eq!((KERNEL_DATA >> 47) & 1, 1);
        assert_eq!(core::mem::size_of::<DescriptorTablePointer>(), 10);
    }
}
