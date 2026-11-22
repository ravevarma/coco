//! Linear framebuffer driver, text console and boot splash.

use coco_abi::{FramebufferInfo, PixelFormat};

use crate::font;

/// coco palette (0xRRGGBB).
pub mod palette {
    /// Splash background (deep ink).
    pub const BACKGROUND: u32 = 0x14_16_1D;
    /// Coconut shell, outer ring.
    pub const SHELL_DARK: u32 = 0x5C_3A_22;
    /// Coconut shell face.
    pub const SHELL: u32 = 0x8A_57_33;
    /// Coconut pores.
    pub const PORE: u32 = 0x2A_19_0F;
    /// Wordmark / cream.
    pub const CREAM: u32 = 0xF3_E9_D2;
    /// Accent (progress, highlights).
    pub const AMBER: u32 = 0xE8_A3_3D;
    /// Progress track.
    pub const TRACK: u32 = 0x2A_2E_38;
    /// Console text.
    pub const TEXT: u32 = 0xC9_CE_D8;
    /// Fatal error banner.
    pub const ALERT: u32 = 0xC8_3C_3C;
}

/// Exclusive handle to the linear framebuffer.
///
/// Invariant: `base` points to `stride * height` writable `u32` pixels that
/// are mapped (direct map) for the kernel's lifetime and accessed only
/// through this handle (owned by the console lock).
#[derive(Debug)]
pub struct Framebuffer {
    base: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
    format: PixelFormat,
}

// SAFETY: the framebuffer is plain memory owned exclusively by this handle;
// moving the handle between contexts is safe (access is serialized by the
// console lock).
unsafe impl Send for Framebuffer {}

impl Framebuffer {
    /// Wraps a validated framebuffer description.
    ///
    /// # Safety
    /// `info` must have passed `BootInfo::validate_header` (so the pixel
    /// range lies inside the direct map) and no other handle may exist.
    pub unsafe fn new(info: &FramebufferInfo) -> Option<Self> {
        let format = PixelFormat::from_raw(info.format)?;
        if format == PixelFormat::None {
            return None;
        }
        Some(Self {
            base: info.virt_base as *mut u32,
            width: info.width as usize,
            height: info.height as usize,
            stride: info.stride as usize,
            format,
        })
    }

    /// Width in pixels.
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub const fn height(&self) -> usize {
        self.height
    }

    fn encode(&self, rgb: u32) -> u32 {
        match self.format {
            PixelFormat::Bgrx | PixelFormat::None => rgb & 0x00FF_FFFF,
            PixelFormat::Rgbx => ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF),
        }
    }

    /// Fills a rectangle (clipped to the screen).
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, rgb: u32) {
        let px = self.encode(rgb);
        let x_end = x.saturating_add(w).min(self.width);
        let y_end = y.saturating_add(h).min(self.height);
        for row in y.min(self.height)..y_end {
            for col in x.min(self.width)..x_end {
                let pixel = self.base.wrapping_add(row * self.stride + col);
                // SAFETY: row < height and col < width, so the offset is
                // < stride * height: inside the mapping (type invariant).
                unsafe { pixel.write_volatile(px) };
            }
        }
    }

    /// Fills a disc of radius `r` centred on (`cx`, `cy`) (clipped).
    pub fn fill_circle(&mut self, cx: usize, cy: usize, r: usize, rgb: u32) {
        let (cx, cy, r) = (cx as i64, cy as i64, r as i64);
        for dy in -r..=r {
            // Integer half-chord, no floating point in the kernel.
            let mut half = 0i64;
            while (half + 1) * (half + 1) + dy * dy <= r * r {
                half += 1;
            }
            let y = cy + dy;
            let x_start = (cx - half).max(0);
            let x_end = cx + half + 1;
            if y >= 0 && x_end > x_start {
                self.fill_rect(x_start as usize, y as usize, (x_end - x_start) as usize, 1, rgb);
            }
        }
    }

    /// Draws `text` with the built-in font at integer `scale`.
    pub fn draw_text(&mut self, x: usize, y: usize, text: &str, scale: usize, rgb: u32) {
        let mut cx = x;
        for byte in text.bytes() {
            self.draw_glyph(cx, y, byte, scale, rgb, None);
            cx = cx.saturating_add((font::WIDTH + 1) * scale);
        }
    }

    fn draw_glyph(&mut self, x: usize, y: usize, byte: u8, scale: usize, rgb: u32, bg: Option<u32>) {
        for (row, bits) in font::glyph(byte).iter().enumerate() {
            for col in 0..font::WIDTH {
                let on = bits & (1 << (font::WIDTH - 1 - col)) != 0;
                let color = if on { Some(rgb) } else { bg };
                if let Some(c) = color {
                    self.fill_rect(x + col * scale, y + row * scale, scale, scale, c);
                }
            }
        }
    }

    /// Scrolls the band `[y, y + h)` up by `dy` pixels, filling with `rgb`.
    fn scroll(&mut self, y: usize, h: usize, dy: usize, rgb: u32) {
        let y_end = y.saturating_add(h).min(self.height);
        if dy >= y_end.saturating_sub(y) {
            self.fill_rect(0, y, self.width, h, rgb);
            return;
        }
        for row in y..y_end - dy {
            let dst = self.base.wrapping_add(row * self.stride);
            let src = self.base.wrapping_add((row + dy) * self.stride);
            // SAFETY: both rows are < height and each copy is `width <=
            // stride` pixels, so both ranges are inside the mapping; dy > 0
            // means the rows differ and the ranges do not overlap.
            unsafe { core::ptr::copy_nonoverlapping(src, dst, self.width) };
        }
        self.fill_rect(0, y_end - dy, self.width, dy, rgb);
    }
}

/// Glyph scale used by the console.
const CONSOLE_SCALE: usize = 2;
const CELL_W: usize = (font::WIDTH + 1) * CONSOLE_SCALE;
const CELL_H: usize = (font::HEIGHT + 2) * CONSOLE_SCALE;

/// Scrolling text console in the lower part of the screen, below the splash.
#[derive(Debug)]
pub struct FbConsole {
    fb: Framebuffer,
    top: usize,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    progress_y: usize,
}

impl FbConsole {
    /// Draws the boot splash and creates a console beneath it.
    pub fn new(mut fb: Framebuffer) -> Self {
        let (w, h) = (fb.width(), fb.height());
        fb.fill_rect(0, 0, w, h, palette::BACKGROUND);
        // Coconut mark: dark rim, shell face, three pores.
        let r = (w.min(h) / 10).max(12);
        let cx = w / 2;
        let cy = h * 3 / 10;
        fb.fill_circle(cx, cy, r, palette::SHELL_DARK);
        fb.fill_circle(cx, cy, r * 7 / 8, palette::SHELL);
        let pore = (r / 7).max(2);
        fb.fill_circle(cx - r / 3, cy - r / 3, pore, palette::PORE);
        fb.fill_circle(cx + r / 3, cy - r / 3, pore, palette::PORE);
        fb.fill_circle(cx, cy - r / 8, pore, palette::PORE);
        // Wordmark.
        let scale = (r / 10).max(2);
        let text_w = 4 * (font::WIDTH + 1) * scale - scale;
        let wy = cy + r + r / 3;
        fb.draw_text(cx.saturating_sub(text_w / 2), wy, "coco", scale, palette::CREAM);
        // Progress track.
        let progress_y = wy + font::HEIGHT * scale + r / 3;
        fb.fill_rect(w / 3, progress_y, w / 3, 4, palette::TRACK);
        let top = progress_y + 4 + CELL_H * 2;
        let rows = h.saturating_sub(top) / CELL_H;
        let cols = w.saturating_sub(2 * CELL_W) / CELL_W;
        Self { fb, top, cols, rows, col: 0, row: 0, progress_y }
    }

    /// Shows boot progress (`done` of `total` stages).
    pub fn progress(&mut self, done: usize, total: usize) {
        let w = self.fb.width();
        let filled = (w / 3) * done.min(total) / total.max(1);
        self.fb.fill_rect(w / 3, self.progress_y, filled, 4, palette::AMBER);
    }

    /// Paints a full-width alert banner at the top of the screen.
    pub fn alert(&mut self, text: &str) {
        let w = self.fb.width();
        self.fb.fill_rect(0, 0, w, CELL_H * 2, palette::ALERT);
        self.fb.draw_text(CELL_W, CELL_H / 2, text, CONSOLE_SCALE, palette::CREAM);
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.rows == 0 {
            return;
        }
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.fb.scroll(self.top, self.rows * CELL_H, CELL_H, palette::BACKGROUND);
        }
    }

    /// Writes bytes (non-printable bytes render as a box).
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'\n' => self.newline(),
                b'\r' => self.col = 0,
                _ => {
                    if self.rows == 0 || self.cols == 0 {
                        continue;
                    }
                    if self.col >= self.cols {
                        self.newline();
                    }
                    let x = CELL_W + self.col * CELL_W;
                    let y = self.top + self.row * CELL_H;
                    self.fb.draw_glyph(x, y, b, CONSOLE_SCALE, palette::TEXT, Some(palette::BACKGROUND));
                    self.col += 1;
                }
            }
        }
    }
}
