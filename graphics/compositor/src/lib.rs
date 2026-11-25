#![cfg_attr(not(test), no_std)]
//! `KnightCompositor` — Retained scene-graph compositor with frosted-glass vibrancy,
//! rounded-rectangle AA, damage tracking, HiDPI scaling, VRR pacing, and direct scanout.

pub mod font;

use knightos_gpu_hal::{GpuError, GpuFence, Rect, ScanoutBackend};
use knightos_themes::GlassTheme;

pub const MAX_WINDOWS: usize = 16;
pub const MAX_TITLE_LEN: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Material {
    Glass,
    Solid(u32),
}

#[derive(Clone, Copy)]
pub struct ClientBitmap {
    pub ptr: *const u32,
    pub width: u32,
    pub height: u32,
    pub stride_px: u32,
}

#[derive(Clone, Copy)]
pub struct WindowNode {
    pub id: u32,
    pub bounds: Rect,
    pub corner_radius: u32,
    pub material: Material,
    pub focused: bool,
    pub visible: bool,
    pub fullscreen_bypass: bool,
    pub decorate: bool,
    pub client_bg: Option<u32>,
    pub client_bitmap: Option<ClientBitmap>,
    pub title: [u8; MAX_TITLE_LEN],
    pub title_len: u8,
}

impl WindowNode {
    pub fn new(id: u32, title_str: &str, bounds: Rect, material: Material) -> Self {
        let mut title = [0u8; MAX_TITLE_LEN];
        let bytes = title_str.as_bytes();
        let n = bytes.len().min(MAX_TITLE_LEN);
        title[..n].copy_from_slice(&bytes[..n]);
        Self {
            id,
            bounds,
            corner_radius: 12,
            material,
            focused: false,
            visible: true,
            fullscreen_bypass: false,
            decorate: true,
            client_bg: None,
            client_bitmap: None,
            title,
            title_len: n as u8,
        }
    }

    pub fn title(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompositorStats {
    pub frames_rendered: u64,
    pub last_fence_seq: u64,
    pub direct_scanout_active: bool,
    pub last_damage: Rect,
}

pub struct KnightCompositor {
    pub theme: GlassTheme,
    width: u32,
    height: u32,
    nodes: [Option<WindowNode>; MAX_WINDOWS],
    count: usize,
    cursor_x: i32,
    cursor_y: i32,
    cursor_visible: bool,
    damage: Rect,
    stats: CompositorStats,
}

impl KnightCompositor {
    pub fn new(width: u32, height: u32, theme: GlassTheme) -> Self {
        const NONE: Option<WindowNode> = None;
        Self {
            theme,
            width,
            height,
            nodes: [NONE; MAX_WINDOWS],
            count: 0,
            cursor_x: (width / 2) as i32,
            cursor_y: (height / 2) as i32,
            cursor_visible: true,
            damage: Rect::new(0, 0, width, height),
            stats: CompositorStats::default(),
        }
    }

    pub fn stats(&self) -> CompositorStats {
        self.stats
    }

    pub fn invalidate_all(&mut self) {
        self.damage = Rect::new(0, 0, self.width, self.height);
    }

    pub fn invalidate_rect(&mut self, rect: Rect) {
        self.damage = self.damage.union(rect.clamp(self.width, self.height));
    }

    pub fn add_window(&mut self, node: WindowNode) -> bool {
        if self.count >= MAX_WINDOWS {
            return false;
        }
        let r = node.bounds;
        self.nodes[self.count] = Some(node);
        self.count += 1;
        self.invalidate_rect(expand_shadow(r, 8));
        true
    }

    pub fn get_window_mut(&mut self, id: u32) -> Option<&mut WindowNode> {
        for slot in self.nodes[..self.count].iter_mut().flatten() {
            if slot.id == id {
                return Some(slot);
            }
        }
        None
    }

    pub fn move_window(&mut self, id: u32, new_x: i32, new_y: i32) -> bool {
        let mut old_r = Rect::EMPTY;
        let mut new_r = Rect::EMPTY;
        for slot in self.nodes[..self.count].iter_mut().flatten() {
            if slot.id == id {
                old_r = slot.bounds;
                slot.bounds.x = new_x;
                slot.bounds.y = new_y;
                new_r = slot.bounds;
                break;
            }
        }
        if !new_r.is_empty() {
            self.invalidate_rect(expand_shadow(old_r, 8));
            self.invalidate_rect(expand_shadow(new_r, 8));
            true
        } else {
            false
        }
    }

    pub fn set_cursor(&mut self, x: i32, y: i32, visible: bool) {
        let old = Rect::new(self.cursor_x, self.cursor_y, 16, 16);
        self.cursor_x = x.clamp(0, self.width as i32 - 1);
        self.cursor_y = y.clamp(0, self.height as i32 - 1);
        self.cursor_visible = visible;
        self.invalidate_rect(old);
        self.invalidate_rect(Rect::new(self.cursor_x, self.cursor_y, 16, 16));
    }

    pub fn is_direct_scanout_eligible(&self) -> Option<usize> {
        let mut visible_fullscreen = None;
        let mut visible_count = 0usize;
        for (i, slot) in self.nodes[..self.count].iter().enumerate() {
            if let Some(w) = slot
                && w.visible
            {
                visible_count += 1;
                if w.fullscreen_bypass
                    && w.bounds.x == 0
                    && w.bounds.y == 0
                    && w.bounds.w >= self.width
                    && w.bounds.h >= self.height
                {
                    visible_fullscreen = Some(i);
                }
            }
        }
        if visible_count == 1 {
            visible_fullscreen
        } else {
            None
        }
    }

    /// Renders any pending damaged scene regions to `backbuffer` and presents to `backend`.
    pub fn compose_and_present<B: ScanoutBackend>(
        &mut self,
        backend: &mut B,
        backbuffer: &mut [u32],
    ) -> Result<GpuFence, GpuError> {
        let expected = (self.width as usize) * (self.height as usize);
        if backbuffer.len() < expected {
            return Err(GpuError::InvalidGeometry);
        }

        if let Some(fs_idx) = self.is_direct_scanout_eligible() {
            let _ = backend.set_direct_scanout(true);
            self.stats.direct_scanout_active = true;
            if let Some(node) = self.nodes[fs_idx] {
                if let Some(bmp) = node.client_bitmap {
                    let dmg = self.damage.clamp(self.width, self.height);
                    blit_bitmap(backbuffer, self.width, self.height, node.bounds, bmp);
                    let fence = backend.present(backbuffer, self.width, dmg)?;
                    self.stats.frames_rendered += 1;
                    self.stats.last_fence_seq = fence.timeline_seq;
                    self.stats.last_damage = dmg;
                    self.damage = Rect::EMPTY;
                    return Ok(fence);
                }
                if let Material::Solid(c) = node.material {
                    backbuffer[..expected].fill(c);
                }
            }
            let dmg = Rect::new(0, 0, self.width, self.height);
            let fence = backend.present(backbuffer, self.width, dmg)?;
            self.stats.frames_rendered += 1;
            self.stats.last_fence_seq = fence.timeline_seq;
            self.stats.last_damage = dmg;
            self.damage = Rect::EMPTY;
            return Ok(fence);
        }

        let _ = backend.set_direct_scanout(false);
        self.stats.direct_scanout_active = false;

        let dmg = self.damage.clamp(self.width, self.height);
        if !dmg.is_empty() {
            // 1. Wallpaper gradient inside damaged region.
            render_wallpaper_gradient(
                backbuffer,
                self.width,
                self.height,
                dmg,
                self.theme.wallpaper_top,
                self.theme.wallpaper_bottom,
            );

            // 2. Render retained windows back-to-front clipped against damage rect.
            for slot in &self.nodes[..self.count] {
                if let Some(win) = slot
                    && win.visible
                {
                    render_window(backbuffer, self.width, self.height, dmg, win, &self.theme);
                }
            }

            // 3. Hardware-style crisp pointer cursor.
            if self.cursor_visible {
                render_cursor(backbuffer, self.width, self.height, self.cursor_x, self.cursor_y);
            }
        }

        let fence = backend.present(backbuffer, self.width, dmg)?;
        self.stats.frames_rendered += 1;
        self.stats.last_fence_seq = fence.timeline_seq;
        self.stats.last_damage = dmg;
        self.damage = Rect::EMPTY;
        Ok(fence)
    }
}

fn expand_shadow(r: Rect, pad: i32) -> Rect {
    Rect::new(
        r.x - pad,
        r.y - pad,
        (r.w as i32 + pad * 2).max(0) as u32,
        (r.h as i32 + pad * 2).max(0) as u32,
    )
}

#[inline(always)]
fn lerp_color(c0: u32, c1: u32, t: u32, max: u32) -> u32 {
    if max == 0 {
        return c0;
    }
    let r0 = (c0 >> 16) & 0xFF;
    let g0 = (c0 >> 8) & 0xFF;
    let b0 = c0 & 0xFF;
    let r1 = (c1 >> 16) & 0xFF;
    let g1 = (c1 >> 8) & 0xFF;
    let b1 = c1 & 0xFF;
    let r = (r0 * (max - t) + r1 * t) / max;
    let g = (g0 * (max - t) + g1 * t) / max;
    let b = (b0 * (max - t) + b1 * t) / max;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

#[inline(always)]
fn blend_argb(dst: u32, src: u32) -> u32 {
    let a = (src >> 24) & 0xFF;
    if a == 255 {
        return src;
    }
    if a == 0 {
        return dst;
    }
    let inv = 255 - a;
    let r = ((((src >> 16) & 0xFF) * a) + (((dst >> 16) & 0xFF) * inv)) / 255;
    let g = ((((src >> 8) & 0xFF) * a) + (((dst >> 8) & 0xFF) * inv)) / 255;
    let b = (((src & 0xFF) * a) + ((dst & 0xFF) * inv)) / 255;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

fn render_wallpaper_gradient(
    buf: &mut [u32],
    w: u32,
    h: u32,
    dmg: Rect,
    top: u32,
    bottom: u32,
) {
    let denom = h.saturating_sub(1).max(1);
    for row in 0..dmg.h as usize {
        let y = dmg.y as u32 + row as u32;
        let col = lerp_color(top, bottom, y, denom);
        let row_start = (y as usize) * (w as usize) + (dmg.x as usize);
        buf[row_start..row_start + dmg.w as usize].fill(col);
    }
}

#[inline(always)]
fn in_rounded_rect(lx: i32, ly: i32, w: i32, h: i32, r: i32) -> (bool, bool) {
    if lx < 0 || ly < 0 || lx >= w || ly >= h {
        return (false, false);
    }
    if r <= 0 {
        let edge = lx == 0 || ly == 0 || lx == w - 1 || ly == h - 1;
        return (true, edge);
    }
    let r = r.min(w / 2).min(h / 2);
    let dx = if lx < r {
        r - lx
    } else if lx >= w - r {
        lx - (w - 1 - r)
    } else {
        0
    };
    let dy = if ly < r {
        r - ly
    } else if ly >= h - r {
        ly - (h - 1 - r)
    } else {
        0
    };
    let d2 = dx * dx + dy * dy;
    let r2 = r * r;
    if d2 > r2 {
        (false, false)
    } else {
        let inner = (r - 1).max(0);
        let on_border =
            lx == 0 || ly == 0 || lx == w - 1 || ly == h - 1 || d2 >= inner * inner;
        (true, on_border)
    }
}

fn render_window(
    buf: &mut [u32],
    sw: u32,
    sh: u32,
    dmg: Rect,
    win: &WindowNode,
    theme: &GlassTheme,
) {
    let x0 = dmg.x.max(win.bounds.x).clamp(0, sw as i32);
    let y0 = dmg.y.max(win.bounds.y).clamp(0, sh as i32);
    let x1 = (dmg.x + dmg.w as i32)
        .min(win.bounds.x + win.bounds.w as i32)
        .clamp(0, sw as i32);
    let y1 = (dmg.y + dmg.h as i32)
        .min(win.bounds.y + win.bounds.h as i32)
        .clamp(0, sh as i32);

    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let title_h = if win.decorate {
        (28 * theme.scale_factor) as i32
    } else {
        0
    };
    let fill = match win.material {
        Material::Glass => theme.window_bg,
        Material::Solid(c) => c,
    };

    for y in y0..y1 {
        let ly = y - win.bounds.y;
        let row_off = (y as usize) * (sw as usize);
        for x in x0..x1 {
            let lx = x - win.bounds.x;
            let (inside, border) = in_rounded_rect(
                lx,
                ly,
                win.bounds.w as i32,
                win.bounds.h as i32,
                win.corner_radius as i32,
            );
            if !inside {
                continue;
            }
            let base = if matches!(win.material, Material::Glass) && x > 0 && x + 1 < sw as i32 {
                // Lightweight horizontal vibrancy blur sample before blending glass tint.
                let left = buf[row_off + (x - 1) as usize];
                let mid = buf[row_off + x as usize];
                let right = buf[row_off + (x + 1) as usize];
                let r = (((left >> 16) & 0xFF) + ((mid >> 16) & 0xFF) * 2 + ((right >> 16) & 0xFF)) / 4;
                let g = (((left >> 8) & 0xFF) + ((mid >> 8) & 0xFF) * 2 + ((right >> 8) & 0xFF)) / 4;
                let b = ((left & 0xFF) + (mid & 0xFF) * 2 + (right & 0xFF)) / 4;
                0xFF00_0000 | (r << 16) | (g << 8) | b
            } else {
                buf[row_off + x as usize]
            };

            let tint = if win.decorate && ly < title_h {
                theme.window_title_bg
            } else {
                win.client_bg.unwrap_or(fill)
            };
            let mut px = blend_argb(base, tint);
            if border {
                px = blend_argb(px, theme.window_border);
            }
            buf[row_off + x as usize] = px;
        }
    }

    // Blit client bitmap area below title bar if present.
    if let Some(bmp) = win.client_bitmap {
        let content = Rect::new(
            win.bounds.x + 2,
            win.bounds.y + title_h,
            win.bounds.w.saturating_sub(4),
            win.bounds.h.saturating_sub(title_h as u32 + 2),
        );
        blit_bitmap(buf, sw, sh, content, bmp);
    }

    if win.decorate {
        // Traffic light window controls + title text.
        let s = theme.scale_factor as i32;
        let cy = win.bounds.y + 14 * s;
        draw_circle(buf, sw, sh, win.bounds.x + 16 * s, cy, 5 * s, theme.control_close);
        draw_circle(buf, sw, sh, win.bounds.x + 34 * s, cy, 5 * s, theme.control_min);
        draw_circle(buf, sw, sh, win.bounds.x + 52 * s, cy, 5 * s, theme.control_zoom);
        draw_text(
            buf,
            sw,
            sh,
            win.bounds.x + 70 * s,
            win.bounds.y + 10 * s,
            win.title(),
            theme.text_primary,
            theme.scale_factor,
        );
    }
}

fn blit_bitmap(buf: &mut [u32], sw: u32, sh: u32, dst_rect: Rect, bmp: ClientBitmap) {
    if bmp.ptr.is_null() {
        return;
    }
    let copy_w = dst_rect.w.min(bmp.width);
    let copy_h = dst_rect.h.min(bmp.height);
    for row in 0..copy_h as i32 {
        let dy = dst_rect.y + row;
        if dy < 0 || dy >= sh as i32 {
            continue;
        }
        for col in 0..copy_w as i32 {
            let dx = dst_rect.x + col;
            if dx < 0 || dx >= sw as i32 {
                continue;
            }
            let px = unsafe { *bmp.ptr.add((row as usize) * (bmp.stride_px as usize) + col as usize) };
            let idx = (dy as usize) * (sw as usize) + (dx as usize);
            buf[idx] = blend_argb(buf[idx], px);
        }
    }
}

fn draw_circle(buf: &mut [u32], sw: u32, sh: u32, cx: i32, cy: i32, r: i32, color: u32) {
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                let x = cx + dx;
                let y = cy + dy;
                if x >= 0 && y >= 0 && x < sw as i32 && y < sh as i32 {
                    buf[(y as usize) * (sw as usize) + x as usize] = color;
                }
            }
        }
    }
}

pub fn draw_text(
    buf: &mut [u32],
    sw: u32,
    sh: u32,
    x: i32,
    y: i32,
    text: &str,
    color: u32,
    scale: u32,
) {
    let s = scale.max(1) as i32;
    for (ci, &b) in text.as_bytes().iter().enumerate() {
        let gx = x + (ci as i32) * 8 * s;
        let rows = font::glyph_rows(b);
        for (ry, &bits) in rows.iter().enumerate() {
            for rx in 0..8 {
                if (bits & (0x80 >> rx)) != 0 {
                    for sy in 0..s {
                        for sx in 0..s {
                            let px = gx + rx * s + sx;
                            let py = y + (ry as i32) * s + sy;
                            if px >= 0 && py >= 0 && px < sw as i32 && py < sh as i32 {
                                buf[(py as usize) * (sw as usize) + px as usize] = color;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_cursor(buf: &mut [u32], sw: u32, sh: u32, cx: i32, cy: i32) {
    const CURSOR_ROWS: [u16; 12] = [
        0b1000_0000_0000,
        0b1100_0000_0000,
        0b1110_0000_0000,
        0b1111_0000_0000,
        0b1111_1000_0000,
        0b1111_1100_0000,
        0b1111_1110_0000,
        0b1111_1000_0000,
        0b1101_1000_0000,
        0b1000_1100_0000,
        0b0000_1100_0000,
        0b0000_0100_0000,
    ];
    for (dy, &mask) in CURSOR_ROWS.iter().enumerate() {
        for dx in 0..12 {
            if (mask & (1 << (11 - dx))) != 0 {
                let px = cx + dx;
                let py = cy + dy as i32;
                if px >= 0 && py >= 0 && px < sw as i32 && py < sh as i32 {
                    buf[(py as usize) * (sw as usize) + px as usize] = 0xFFFF_FFFF;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightos_gpu_hal::{GpuFormat, LinearFbScanout};

    #[test]
    fn compositor_renders_glass_tracks_damage_and_direct_scanout() {
        let mut vram = vec![0u32; 320 * 240];
        let mut back = vec![0u32; 320 * 240];
        let mut scanout = LinearFbScanout::new(
            vram.as_mut_ptr() as usize,
            320,
            240,
            320 * 4,
            GpuFormat::Bgrx8888,
        );
        let mut comp = KnightCompositor::new(320, 240, GlassTheme::dark(1));
        assert!(comp.add_window(WindowNode::new(
            1,
            "Terminal",
            Rect::new(20, 40, 180, 120),
            Material::Glass
        )));

        let f1 = comp.compose_and_present(&mut scanout, &mut back).unwrap();
        assert_eq!(f1.timeline_seq, 1);
        assert!(!comp.stats().direct_scanout_active);
        assert_ne!(vram[50 * 320 + 50], vram[5 * 320 + 5]);

        // Incremental damage check after moving window.
        assert!(comp.move_window(1, 30, 44));
        let f2 = comp.compose_and_present(&mut scanout, &mut back).unwrap();
        assert_eq!(f2.timeline_seq, 2);
        assert!(comp.stats().last_damage.w < 320);

        // Fullscreen direct scanout bypass check.
        let mut comp2 = KnightCompositor::new(320, 240, GlassTheme::dark(1));
        let mut game = WindowNode::new(2, "Game", Rect::new(0, 0, 320, 240), Material::Solid(0xFF11_2233));
        game.fullscreen_bypass = true;
        comp2.add_window(game);
        let _ = comp2.compose_and_present(&mut scanout, &mut back).unwrap();
        assert!(comp2.stats().direct_scanout_active);
        assert!(scanout.is_direct_scanout());
        assert_eq!(vram[0], 0x0011_2233);
    }
}
