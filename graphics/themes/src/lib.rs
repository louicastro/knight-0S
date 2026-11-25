#![cfg_attr(not(test), no_std)]
//! KnightOS Desktop Design Tokens (`knightos-themes`).
//!
//! Defines light/dark translucent glass material colors, specular borders,
//! corner radii, traffic-light window controls, and HiDPI layout metrics.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy)]
pub struct GlassTheme {
    pub mode: ThemeMode,
    pub wallpaper_top: u32,
    pub wallpaper_bottom: u32,
    pub menubar_bg: u32,
    pub menubar_text: u32,
    pub dock_bg: u32,
    pub dock_border: u32,
    pub window_bg: u32,
    pub window_title_bg: u32,
    pub window_border: u32,
    pub window_shadow: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub accent: u32,
    pub control_close: u32,
    pub control_min: u32,
    pub control_zoom: u32,
    pub corner_radius: u32,
    pub dock_radius: u32,
    pub scale_factor: u32,
}

impl GlassTheme {
    pub const fn dark(scale_factor: u32) -> Self {
        let s = if scale_factor == 0 { 1 } else { scale_factor };
        Self {
            mode: ThemeMode::Dark,
            wallpaper_top: 0xFF0E_172A,
            wallpaper_bottom: 0xFF1A_2E4C,
            menubar_bg: 0xC814_1B2D,
            menubar_text: 0xFFF1_F5F9,
            dock_bg: 0xBA1E_293B,
            dock_border: 0x55FF_FFFF,
            window_bg: 0xDE16_2032,
            window_title_bg: 0xEA20_2B40,
            window_border: 0x48FF_FFFF,
            window_shadow: 0x5000_0000,
            text_primary: 0xFFF8_FAFC,
            text_secondary: 0xFF94_A3B8,
            accent: 0xFF0A_84FF,
            control_close: 0xFFFF_5F57,
            control_min: 0xFFFE_BC2E,
            control_zoom: 0xFF28_C840,
            corner_radius: 12 * s,
            dock_radius: 16 * s,
            scale_factor: s,
        }
    }

    pub const fn light(scale_factor: u32) -> Self {
        let s = if scale_factor == 0 { 1 } else { scale_factor };
        Self {
            mode: ThemeMode::Light,
            wallpaper_top: 0xFFD9_E8FC,
            wallpaper_bottom: 0xFF93_C5FD,
            menubar_bg: 0xC8F8_FAFC,
            menubar_text: 0xFF0F_172A,
            dock_bg: 0xC8F1_F5F9,
            dock_border: 0x77FF_FFFF,
            window_bg: 0xE6F8_FAFC,
            window_title_bg: 0xF0E2_E8F0,
            window_border: 0x88FF_FFFF,
            window_shadow: 0x3800_0000,
            text_primary: 0xFF0F_172A,
            text_secondary: 0xFF47_5569,
            accent: 0xFF00_7AFF,
            control_close: 0xFFFF_5F57,
            control_min: 0xFFFE_BC2E,
            control_zoom: 0xFF28_C840,
            corner_radius: 12 * s,
            dock_radius: 16 * s,
            scale_factor: s,
        }
    }
}
