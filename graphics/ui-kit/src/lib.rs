#![cfg_attr(not(test), no_std)]
//! KnightOS UI Kit (`knight-ui-kit`) — `#![no_std]` widget & layout helpers
//! rendering crisp macOS-inspired controls into offscreen ARGB window buffers.

use knight_compositor::draw_text;
use knightos_gpu_hal::Rect;
use knightos_themes::GlassTheme;

pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub theme: GlassTheme,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32, theme: GlassTheme) -> Self {
        Self {
            pixels,
            width,
            height,
            theme,
        }
    }

    pub fn clear(&mut self, color: u32) {
        let len = (self.width as usize * self.height as usize).min(self.pixels.len());
        self.pixels[..len].fill(color);
    }

    pub fn fill_rect(&mut self, r: Rect, color: u32) {
        let rc = r.clamp(self.width, self.height);
        for row in 0..rc.h as usize {
            let off = (rc.y as usize + row) * self.width as usize + rc.x as usize;
            self.pixels[off..off + rc.w as usize].fill(color);
        }
    }

    pub fn stroke_rect(&mut self, r: Rect, color: u32) {
        let rc = r.clamp(self.width, self.height);
        if rc.is_empty() {
            return;
        }
        let x0 = rc.x as usize;
        let x1 = (rc.x + rc.w as i32 - 1) as usize;
        let y0 = rc.y as usize;
        let y1 = (rc.y + rc.h as i32 - 1) as usize;
        let w = self.width as usize;
        self.pixels[y0 * w + x0..=y0 * w + x1].fill(color);
        self.pixels[y1 * w + x0..=y1 * w + x1].fill(color);
        for y in y0..=y1 {
            self.pixels[y * w + x0] = color;
            self.pixels[y * w + x1] = color;
        }
    }

    pub fn label(&mut self, x: i32, y: i32, text: &str, color: u32) {
        draw_text(
            self.pixels,
            self.width,
            self.height,
            x,
            y,
            text,
            color,
            self.theme.scale_factor,
        );
    }

    pub fn button(&mut self, r: Rect, text: &str, active: bool) {
        let bg = if active {
            self.theme.accent
        } else {
            self.theme.window_title_bg
        };
        self.fill_rect(r, bg);
        self.stroke_rect(r, self.theme.window_border);
        let s = self.theme.scale_factor as i32;
        self.label(r.x + 8 * s, r.y + ((r.h as i32 - 8 * s) / 2).max(2), text, self.theme.text_primary);
    }

    pub fn toggle_switch(&mut self, x: i32, y: i32, label_str: &str, on: bool) {
        let s = self.theme.scale_factor as i32;
        self.label(x, y + 3 * s, label_str, self.theme.text_primary);
        let pill = Rect::new(x + 150 * s, y, (32 * s) as u32, (16 * s) as u32);
        self.fill_rect(
            pill,
            if on {
                self.theme.control_zoom
            } else {
                0xFF47_5569
            },
        );
        self.stroke_rect(pill, self.theme.window_border);
        let knob_x = if on { pill.x + 18 * s } else { pill.x + 2 * s };
        self.fill_rect(
            Rect::new(knob_x, y + 2 * s, (12 * s) as u32, (12 * s) as u32),
            0xFFFF_FFFF,
        );
    }

    pub fn progress_bar(&mut self, r: Rect, pct: u32) {
        self.fill_rect(r, 0xFF1E_293B);
        self.stroke_rect(r, self.theme.window_border);
        let fill_w = ((r.w.saturating_sub(2)) * pct.min(100)) / 100;
        self.fill_rect(
            Rect::new(r.x + 1, r.y + 1, fill_w, r.h.saturating_sub(2)),
            self.theme.accent,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_kit_renders_widgets_without_overflow() {
        let mut buf = vec![0u32; 256 * 128];
        let mut c = Canvas::new(&mut buf, 256, 128, GlassTheme::dark(1));
        c.clear(0xFF16_2032);
        c.button(Rect::new(10, 10, 90, 24), "Launch", true);
        c.toggle_switch(10, 44, "IOMMU", true);
        c.progress_bar(Rect::new(10, 74, 180, 14), 60);
        assert_eq!(buf[12 * 256 + 12], GlassTheme::dark(1).accent);
    }
}
