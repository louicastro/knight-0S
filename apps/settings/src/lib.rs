#![cfg_attr(not(test), no_std)]
//! KnightOS Native System Settings App View (`knight-settings`).

use knight_ui_kit::Canvas;
use knightos_gpu_hal::Rect;
use knightos_themes::{GlassTheme, ThemeMode};

pub struct SettingsApp;

impl SettingsApp {
    pub fn render(pixels: &mut [u32], w: u32, h: u32, theme: GlassTheme, iommu_enforced: bool) {
        let mut c = Canvas::new(pixels, w, h, theme);
        c.clear(theme.window_bg);
        let s = theme.scale_factor as i32;
        c.label(10 * s, 10 * s, "System Settings", theme.text_primary);
        c.toggle_switch(10 * s, 28 * s, "Dark Glass Mode", theme.mode == ThemeMode::Dark);
        c.toggle_switch(10 * s, 50 * s, "HiDPI 2x Scaling", theme.scale_factor > 1);
        c.toggle_switch(10 * s, 72 * s, "IOMMU DMA Guard", iommu_enforced);
        c.button(Rect::new(10 * s, 96 * s, (120 * s) as u32, (22 * s) as u32), "Check Updates (A/B)", true);
    }
}
