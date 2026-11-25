#![cfg_attr(not(test), no_std)]
//! Explicit GPU Hardware Abstraction Layer (`gpu-hal`).
//!
//! Provides Vulkan-aligned command buffers, explicit fences, damage-tracked scanout
//! presentation, direct fullscreen scanout bypass, and linear framebuffer / VirtIO-GPU
//! backend integration without kernel-mode bloat.

use core::ptr::write_volatile;
use core::sync::atomic::{Ordering, fence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuFormat {
    Bgrx8888,
    Rgbx8888,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuError {
    InvalidGeometry,
    QueueFull,
    Unsupported,
    IoError,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const EMPTY: Self = Self { x: 0, y: 0, w: 0, h: 0 };

    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w as i32).max(other.x + other.w as i32);
        let y1 = (self.y + self.h as i32).max(other.y + other.h as i32);
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0) as u32,
            h: (y1 - y0).max(0) as u32,
        }
    }

    pub fn clamp(self, max_w: u32, max_h: u32) -> Self {
        if self.is_empty() || max_w == 0 || max_h == 0 {
            return Self::EMPTY;
        }
        let x0 = self.x.clamp(0, max_w as i32);
        let y0 = self.y.clamp(0, max_h as i32);
        let x1 = (self.x + self.w as i32).clamp(0, max_w as i32);
        let y1 = (self.y + self.h as i32).clamp(0, max_h as i32);
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0) as u32,
            h: (y1 - y0).max(0) as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanoutInfo {
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub format: GpuFormat,
    pub direct_scanout_supported: bool,
    pub vrr_supported: bool,
    pub refresh_mhz: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuFence {
    pub timeline_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuCommand {
    Clear { color: u32 },
    FillRect { rect: Rect, color: u32 },
    PresentDamage { rect: Rect },
    SetDirectScanout { enabled: bool },
}

/// Fixed-capacity explicit command buffer avoiding heap allocations on the render hot path.
pub struct CommandBuffer<const CAP: usize = 128> {
    cmds: [GpuCommand; CAP],
    len: usize,
}

impl<const CAP: usize> Default for CommandBuffer<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAP: usize> CommandBuffer<CAP> {
    pub const fn new() -> Self {
        Self {
            cmds: [GpuCommand::Clear { color: 0 }; CAP],
            len: 0,
        }
    }

    pub fn reset(&mut self) {
        self.len = 0;
    }

    pub fn push(&mut self, cmd: GpuCommand) -> Result<(), GpuError> {
        if self.len >= CAP {
            return Err(GpuError::QueueFull);
        }
        self.cmds[self.len] = cmd;
        self.len += 1;
        Ok(())
    }

    pub fn commands(&self) -> &[GpuCommand] {
        &self.cmds[..self.len]
    }
}

pub trait ScanoutBackend {
    fn info(&self) -> ScanoutInfo;
    fn present(
        &mut self,
        argb_backbuffer: &[u32],
        stride_px: u32,
        damage: Rect,
    ) -> Result<GpuFence, GpuError>;
    fn set_direct_scanout(&mut self, _enabled: bool) -> bool {
        false
    }
}

/// UEFI GOP / linear MMIO framebuffer scanout backend.
pub struct LinearFbScanout {
    mmio_va: usize,
    info: ScanoutInfo,
    fence_seq: u64,
    direct_scanout: bool,
}

impl LinearFbScanout {
    pub fn new(mmio_va: usize, width: u32, height: u32, pitch_bytes: u32, format: GpuFormat) -> Self {
        Self {
            mmio_va,
            info: ScanoutInfo {
                width,
                height,
                pitch_bytes,
                format,
                direct_scanout_supported: true,
                vrr_supported: false,
                refresh_mhz: 60_000,
            },
            fence_seq: 0,
            direct_scanout: false,
        }
    }

    #[inline(always)]
    fn pack_pixel(fmt: GpuFormat, argb: u32) -> u32 {
        match fmt {
            GpuFormat::Bgrx8888 => argb & 0x00FF_FFFF,
            GpuFormat::Rgbx8888 => {
                let r = (argb >> 16) & 0xFF;
                let g = (argb >> 8) & 0xFF;
                let b = argb & 0xFF;
                r | (g << 8) | (b << 16)
            }
        }
    }

    pub fn is_direct_scanout(&self) -> bool {
        self.direct_scanout
    }
}

impl ScanoutBackend for LinearFbScanout {
    fn info(&self) -> ScanoutInfo {
        self.info
    }

    fn set_direct_scanout(&mut self, enabled: bool) -> bool {
        self.direct_scanout = enabled;
        true
    }

    fn present(
        &mut self,
        argb_backbuffer: &[u32],
        stride_px: u32,
        damage: Rect,
    ) -> Result<GpuFence, GpuError> {
        let dmg = damage.clamp(self.info.width, self.info.height);
        if dmg.is_empty() {
            self.fence_seq += 1;
            return Ok(GpuFence {
                timeline_seq: self.fence_seq,
            });
        }
        let fb_stride = (self.info.pitch_bytes / 4) as usize;
        let ptr = self.mmio_va as *mut u32;
        for row in 0..dmg.h as usize {
            let y = dmg.y as usize + row;
            let src_row = y * (stride_px as usize);
            let dst_row = y * fb_stride;
            for col in 0..dmg.w as usize {
                let x = dmg.x as usize + col;
                let px = Self::pack_pixel(self.info.format, argb_backbuffer[src_row + x]);
                unsafe {
                    write_volatile(ptr.add(dst_row + x), px);
                }
            }
        }
        fence(Ordering::Release);
        self.fence_seq += 1;
        Ok(GpuFence {
            timeline_seq: self.fence_seq,
        })
    }
}

/// Executes a GPU command buffer over a CPU-accessible shadow surface and presents damage.
pub fn submit_commands<B: ScanoutBackend, const CAP: usize>(
    backend: &mut B,
    backbuffer: &mut [u32],
    stride_px: u32,
    height_px: u32,
    cmd_buf: &CommandBuffer<CAP>,
) -> Result<GpuFence, GpuError> {
    let mut dirty = Rect::EMPTY;
    for &cmd in cmd_buf.commands() {
        match cmd {
            GpuCommand::Clear { color } => {
                backbuffer.fill(color);
                dirty = Rect::new(0, 0, stride_px, height_px);
            }
            GpuCommand::FillRect { rect, color } => {
                let r = rect.clamp(stride_px, height_px);
                for row in 0..r.h as usize {
                    let off = (r.y as usize + row) * (stride_px as usize) + r.x as usize;
                    backbuffer[off..off + r.w as usize].fill(color);
                }
                dirty = dirty.union(r);
            }
            GpuCommand::PresentDamage { rect } => {
                dirty = dirty.union(rect);
            }
            GpuCommand::SetDirectScanout { enabled } => {
                let _ = backend.set_direct_scanout(enabled);
            }
        }
    }
    backend.present(backbuffer, stride_px, dirty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_buffer_and_linear_scanout_present() {
        let mut vram = vec![0u32; 64 * 64];
        let mut backbuffer = vec![0u32; 64 * 64];
        let mut backend = LinearFbScanout::new(
            vram.as_mut_ptr() as usize,
            64,
            64,
            64 * 4,
            GpuFormat::Bgrx8888,
        );
        let mut cb: CommandBuffer<16> = CommandBuffer::new();
        cb.push(GpuCommand::Clear { color: 0xFF10_2030 }).unwrap();
        cb.push(GpuCommand::FillRect {
            rect: Rect::new(8, 8, 16, 16),
            color: 0xFFAA_BBCC,
        })
        .unwrap();
        cb.push(GpuCommand::SetDirectScanout { enabled: true }).unwrap();

        let fence = submit_commands(&mut backend, &mut backbuffer, 64, 64, &cb).unwrap();
        assert_eq!(fence.timeline_seq, 1);
        assert!(backend.is_direct_scanout());
        assert_eq!(vram[0], 0x0010_2030);
        assert_eq!(vram[8 * 64 + 8], 0x00AA_BBCC);
    }
}

/// VirtIO-GPU 2D [`ScanoutBackend`] adapter flushing dirty damage rects over DMA controlq.
pub struct VirtioGpuScanout {
    gpu: knightos_virtio::VirtioGpu,
    fence_seq: u64,
}

impl VirtioGpuScanout {
    pub fn new(gpu: knightos_virtio::VirtioGpu) -> Self {
        Self { gpu, fence_seq: 0 }
    }
}

impl ScanoutBackend for VirtioGpuScanout {
    fn info(&self) -> ScanoutInfo {
        ScanoutInfo {
            width: self.gpu.width(),
            height: self.gpu.height(),
            pitch_bytes: self.gpu.width() * 4,
            format: GpuFormat::Bgrx8888,
            direct_scanout_supported: true,
            vrr_supported: false,
            refresh_mhz: 60_000,
        }
    }

    fn present(
        &mut self,
        argb_backbuffer: &[u32],
        stride_px: u32,
        damage: Rect,
    ) -> Result<GpuFence, GpuError> {
        let w = self.gpu.width();
        let h = self.gpu.height();
        let dmg = damage.clamp(w, h);
        if dmg.is_empty() {
            self.fence_seq += 1;
            return Ok(GpuFence {
                timeline_seq: self.fence_seq,
            });
        }
        let dst = self.gpu.backbuffer_mut();
        for row in 0..dmg.h as usize {
            let y = dmg.y as usize + row;
            let src_off = y * (stride_px as usize) + dmg.x as usize;
            let dst_off = y * (w as usize) + dmg.x as usize;
            dst[dst_off..dst_off + dmg.w as usize]
                .copy_from_slice(&argb_backbuffer[src_off..src_off + dmg.w as usize]);
        }
        self.gpu
            .flush_rect(dmg.x as u32, dmg.y as u32, dmg.w, dmg.h)
            .map_err(|_| GpuError::IoError)?;
        self.fence_seq += 1;
        Ok(GpuFence {
            timeline_seq: self.fence_seq,
        })
    }
}
