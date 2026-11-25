#![cfg_attr(not(test), no_std)]
//! KnightOS Desktop Shell (`knight-shell`) — Menu bar, floating glass dock,
//! window hosting (`Terminal`, `Settings`), and `KnightCompositor` orchestration.

use knight_compositor::{ClientBitmap, KnightCompositor, Material, WindowNode};
use knight_settings::SettingsApp;
use knight_terminal::TerminalApp;
use knight_ui_kit::Canvas;
use knightos_gpu_hal::{GpuError, GpuFence, Rect, ScanoutBackend};
use knightos_themes::GlassTheme;

pub struct DesktopConfig<'a> {
    pub arch: &'a str,
    pub cpus: u32,
    pub usable_mib: u64,
    pub iommu_enforced: bool,
    pub theme: GlassTheme,
}

/// Builds and renders the KnightOS desktop (menu bar, dock, Terminal, Settings) onto `backend`.
pub fn render_desktop_session<B: ScanoutBackend>(
    backend: &mut B,
    backbuffer: &mut [u32],
    menubar_buf: &mut [u32],
    dock_buf: &mut [u32],
    term_buf: &mut [u32],
    settings_buf: &mut [u32],
    cfg: &DesktopConfig<'_>,
) -> Result<(GpuFence, usize), GpuError> {
    let info = backend.info();
    let w = info.width.min(1024);
    let h = info.height.min(768);
    let s = cfg.theme.scale_factor;
    let mut comp = KnightCompositor::new(w, h, cfg.theme);

    // 1. Top translucent macOS-inspired Menu Bar.
    let mb_h = (24 * s).min(h);
    let mut mb_canvas = Canvas::new(menubar_buf, w, mb_h, cfg.theme);
    mb_canvas.clear(cfg.theme.menubar_bg);
    mb_canvas.label(12 * s as i32, 8 * s as i32, "KnightOS", cfg.theme.menubar_text);
    mb_canvas.label(100 * s as i32, 8 * s as i32, "Terminal", cfg.theme.text_secondary);
    mb_canvas.label(184 * s as i32, 8 * s as i32, "Settings", cfg.theme.text_secondary);
    mb_canvas.label((w.saturating_sub(140 * s)) as i32, 8 * s as i32, "KnightFS OK", 0xFF34_D399);

    let mut mb_win = WindowNode::new(1, "MenuBar", Rect::new(0, 0, w, mb_h), Material::Glass);
    mb_win.decorate = false;
    mb_win.corner_radius = 0;
    mb_win.client_bitmap = Some(ClientBitmap {
        ptr: menubar_buf.as_ptr(),
        width: w,
        height: mb_h,
        stride_px: w,
    });
    comp.add_window(mb_win);

    // 2. Native Terminal Window (360 x 190).
    let tw = 360.min(w.saturating_sub(40));
    let th = 160.min(h.saturating_sub(80));
    TerminalApp::render(term_buf, tw, th, cfg.theme, cfg.arch, cfg.cpus, cfg.usable_mib);
    let mut term_win = WindowNode::new(
        2,
        "Terminal",
        Rect::new(24 * s as i32, (mb_h + 18 * s) as i32, tw, th + 28 * s),
        Material::Glass,
    );
    term_win.focused = true;
    term_win.client_bitmap = Some(ClientBitmap {
        ptr: term_buf.as_ptr(),
        width: tw,
        height: th,
        stride_px: tw,
    });
    comp.add_window(term_win);

    // 3. Native System Settings Window.
    let sw = 280.min(w.saturating_sub(40));
    let sh = 140.min(h.saturating_sub(80));
    SettingsApp::render(settings_buf, sw, sh, cfg.theme, cfg.iommu_enforced);
    let sx = (w as i32 - sw as i32 - 24).max(40);
    let sy = (mb_h + 44 * s) as i32;
    let mut set_win = WindowNode::new(3, "Settings", Rect::new(sx, sy, sw, sh + 28 * s), Material::Glass);
    set_win.client_bitmap = Some(ClientBitmap {
        ptr: settings_buf.as_ptr(),
        width: sw,
        height: sh,
        stride_px: sw,
    });
    comp.add_window(set_win);

    // 4. Floating Bottom Glass Dock.
    let dock_w = 320.min(w.saturating_sub(24));
    let dock_h = 44 * s;
    let mut dk_canvas = Canvas::new(dock_buf, dock_w, dock_h, cfg.theme);
    dk_canvas.clear(cfg.theme.dock_bg);
    dk_canvas.button(Rect::new(10, 8, 86, 26), "Term", true);
    dk_canvas.button(Rect::new(106, 8, 86, 26), "Files", false);
    dk_canvas.button(Rect::new(202, 8, 96, 26), "Settings", false);

    let dock_x = ((w - dock_w) / 2) as i32;
    let dock_y = (h.saturating_sub(dock_h + 12 * s)) as i32;
    let mut dock_win = WindowNode::new(4, "Dock", Rect::new(dock_x, dock_y, dock_w, dock_h), Material::Glass);
    dock_win.decorate = false;
    dock_win.corner_radius = cfg.theme.dock_radius;
    dock_win.client_bitmap = Some(ClientBitmap {
        ptr: dock_buf.as_ptr(),
        width: dock_w,
        height: dock_h,
        stride_px: dock_w,
    });
    comp.add_window(dock_win);

    // Frame 1: Full glass desktop render.
    let _ = comp.compose_and_present(backend, backbuffer)?;
    // Frame 2: Simulate dragging Terminal window and moving cursor -> incremental damage present.
    comp.move_window(2, 32 * s as i32, (mb_h + 22 * s) as i32);
    comp.set_cursor(dock_x + 40, dock_y + 16, true);
    let fence = comp.compose_and_present(backend, backbuffer)?;

    Ok((fence, comp.stats().frames_rendered as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightos_gpu_hal::{GpuFormat, LinearFbScanout};

    #[test]
    fn desktop_shell_renders_two_composited_frames() {
        let mut vram = vec![0u32; 640 * 480];
        let mut back = vec![0u32; 640 * 480];
        let mut mb = vec![0u32; 640 * 24];
        let mut dock = vec![0u32; 320 * 44];
        let mut term = vec![0u32; 360 * 160];
        let mut set = vec![0u32; 280 * 140];
        let mut scanout = LinearFbScanout::new(
            vram.as_mut_ptr() as usize,
            640,
            480,
            640 * 4,
            GpuFormat::Bgrx8888,
        );
        let cfg = DesktopConfig {
            arch: "test64",
            cpus: 4,
            usable_mib: 1024,
            iommu_enforced: true,
            theme: GlassTheme::dark(1),
        };
        let (fence, frames) = render_desktop_session(
            &mut scanout,
            &mut back,
            &mut mb,
            &mut dock,
            &mut term,
            &mut set,
            &cfg,
        )
        .unwrap();
        assert_eq!(frames, 2);
        assert_eq!(fence.timeline_seq, 2);
        // Verify menubar & dock rendered non-zero pixels.
        assert_ne!(vram[10 * 640 + 14], 0);
    }
}
