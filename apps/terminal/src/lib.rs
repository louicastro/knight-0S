#![cfg_attr(not(test), no_std)]
//! KnightOS Native Terminal App View (`knight-terminal`).

use knight_ui_kit::Canvas;
use knightos_gpu_hal::Rect;
use knightos_themes::GlassTheme;

pub struct TerminalApp;

impl TerminalApp {
    pub fn render(pixels: &mut [u32], w: u32, h: u32, theme: GlassTheme, arch: &str, cpus: u32, mib: u64) {
        let mut c = Canvas::new(pixels, w, h, theme);
        c.clear(0xEE0D_1321);
        let s = theme.scale_factor as i32;
        c.label(10 * s, 10 * s, "KnightOS Terminal (Ring 3 / EL0)", theme.accent);
        c.label(10 * s, 26 * s, "knight> uname -a", theme.text_primary);
        let mut line = [0u8; 48];
        let mut idx = 0usize;
        for b in b"KnightOS v0.4 (" {
            line[idx] = *b;
            idx += 1;
        }
        for b in arch.as_bytes() {
            if idx < line.len() {
                line[idx] = *b;
                idx += 1;
            }
        }
        for b in b") Microkernel" {
            if idx < line.len() {
                line[idx] = *b;
                idx += 1;
            }
        }
        if let Ok(txt) = core::str::from_utf8(&line[..idx]) {
            c.label(10 * s, 40 * s, txt, theme.text_secondary);
        }
        c.label(10 * s, 56 * s, "knight> storage --status", theme.text_primary);
        c.label(10 * s, 70 * s, "KnightFS v2 + VirtIO/NVMe: READY", 0xFF34_D399);
        let ram_pct = ((100usize).min((mib as usize / 16).max(20))) as u32;
        let _ = cpus;
        c.label(10 * s, 90 * s, "Memory Headroom:", theme.text_secondary);
        c.progress_bar(Rect::new(10 * s, 104 * s, w.saturating_sub(24 * s as u32), (12 * s) as u32), ram_pct);
    }
}
