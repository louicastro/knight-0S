#![cfg_attr(not(test), no_std)]
//! User-space VirtIO Modern PCIe split-virtqueue drivers (`virtio-blk` & `virtio-gpu`).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

pub const VIRTIO_PCI_VENDOR: u16 = 0x1AF4;
pub const VIRTIO_PCI_DEV_BLK_MODERN: u16 = 0x1042;
pub const VIRTIO_PCI_DEV_BLK_TRANSITIONAL: u16 = 0x1001;
pub const VIRTIO_PCI_DEV_GPU_MODERN: u16 = 0x1050;
pub const VIRTIO_PCI_DEV_GPU_TRANSITIONAL: u16 = 0x1010;

pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

const MAX_QUEUE_DEPTH: usize = 16;
pub const DMA_REQUIRED_BYTES: usize = 16384; // 4 pages for splitq + request scratch
const OFF_DESC: usize = 0x0000;
const OFF_AVAIL: usize = 0x0400;
const OFF_USED: usize = 0x1000;
const OFF_REQ_HDR: usize = 0x2000;
const OFF_RESP_BUF: usize = 0x2200;
const OFF_STATUS: usize = 0x2040;
const OFF_DATA: usize = 0x3000;
const MAX_XFER_CHUNK: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    BadConfig,
    FeaturesRejected,
    NoQueue,
    IoError,
    Timeout,
    InvalidArg,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; MAX_QUEUE_DEPTH],
    pub used_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; MAX_QUEUE_DEPTH],
    pub avail_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioBlkReqHdr {
    pub req_type: u32,
    pub reserved: u32,
    pub sector: u64,
}

#[derive(Clone, Copy)]
pub struct VirtioBlkTransport {
    pub common_cfg_va: usize,
    pub notify_base_va: usize,
    pub notify_off_multiplier: u32,
    pub device_cfg_va: usize,
    pub dma_va: usize,
    pub dma_phys: u64,
    pub dma_len: usize,
}

pub type VirtioTransport = VirtioBlkTransport;

struct SplitQueue {
    common_cfg_va: usize,
    notify_ptr: *mut u16,
    queue_idx: u16,
    dma_va: usize,
    dma_phys: u64,
    queue_size: u16,
    avail_idx: u16,
    last_used_idx: u16,
    yield_fn: fn(),
}

#[inline(always)]
fn io_barrier() {
    fence(Ordering::SeqCst);
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("dsb sy", options(nostack));
    }
}

#[inline(always)]
unsafe fn mmio_r8(base: usize, off: usize) -> u8 {
    unsafe { read_volatile((base + off) as *const u8) }
}

#[inline(always)]
unsafe fn mmio_w8(base: usize, off: usize, v: u8) {
    unsafe { write_volatile((base + off) as *mut u8, v) }
}

#[inline(always)]
unsafe fn mmio_r16(base: usize, off: usize) -> u16 {
    unsafe { read_volatile((base + off) as *const u16) }
}

#[inline(always)]
unsafe fn mmio_w16(base: usize, off: usize, v: u16) {
    unsafe { write_volatile((base + off) as *mut u16, v) }
}

#[inline(always)]
unsafe fn mmio_r32(base: usize, off: usize) -> u32 {
    unsafe { read_volatile((base + off) as *const u32) }
}

#[inline(always)]
unsafe fn mmio_w32(base: usize, off: usize, v: u32) {
    unsafe { write_volatile((base + off) as *mut u32, v) }
}

#[inline(always)]
unsafe fn mmio_r64_split(base: usize, off: usize) -> u64 {
    let lo = unsafe { mmio_r32(base, off) } as u64;
    let hi = unsafe { mmio_r32(base, off + 4) } as u64;
    (hi << 32) | lo
}

#[inline(always)]
unsafe fn mmio_w64_split(base: usize, off: usize, v: u64) {
    unsafe {
        mmio_w32(base, off, v as u32);
        mmio_w32(base, off + 4, (v >> 32) as u32);
    }
}

impl SplitQueue {
    fn init(tr: VirtioTransport, queue_idx: u16, yield_fn: fn()) -> Result<Self, VirtioError> {
        if tr.common_cfg_va == 0
            || tr.notify_base_va == 0
            || tr.dma_va == 0
            || tr.dma_len < DMA_REQUIRED_BYTES
        {
            return Err(VirtioError::BadConfig);
        }

        let common = tr.common_cfg_va;
        unsafe {
            mmio_w8(common, 0x14, 0);
            for _ in 0..100_000 {
                if mmio_r8(common, 0x14) == 0 {
                    break;
                }
                core::hint::spin_loop();
            }

            mmio_w8(common, 0x14, STATUS_ACKNOWLEDGE);
            mmio_w8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

            // Negotiate VIRTIO_F_VERSION_1 (bit 32 -> word 1 bit 0).
            mmio_w32(common, 0x00, 1);
            let dev_feat_hi = mmio_r32(common, 0x04);
            mmio_w32(common, 0x08, 0);
            mmio_w32(common, 0x0C, 0);
            mmio_w32(common, 0x08, 1);
            mmio_w32(common, 0x0C, dev_feat_hi & 1);

            let status_feat = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
            mmio_w8(common, 0x14, status_feat);
            if (mmio_r8(common, 0x14) & STATUS_FEATURES_OK) == 0 {
                return Err(VirtioError::FeaturesRejected);
            }

            mmio_w16(common, 0x16, queue_idx);
            let max_q = mmio_r16(common, 0x18);
            if max_q < 4 {
                return Err(VirtioError::NoQueue);
            }
            let queue_size = max_q.min(MAX_QUEUE_DEPTH as u16);
            mmio_w16(common, 0x18, queue_size);

            core::ptr::write_bytes(tr.dma_va as *mut u8, 0, DMA_REQUIRED_BYTES);
            io_barrier();

            mmio_w64_split(common, 0x20, tr.dma_phys + OFF_DESC as u64);
            mmio_w64_split(common, 0x28, tr.dma_phys + OFF_AVAIL as u64);
            mmio_w64_split(common, 0x30, tr.dma_phys + OFF_USED as u64);

            let notify_off = mmio_r16(common, 0x1E) as usize;
            let notify_ptr =
                (tr.notify_base_va + notify_off * (tr.notify_off_multiplier as usize)) as *mut u16;

            mmio_w16(common, 0x1C, 1);
            mmio_w8(common, 0x14, status_feat | STATUS_DRIVER_OK);

            Ok(Self {
                common_cfg_va: common,
                notify_ptr,
                queue_idx,
                dma_va: tr.dma_va,
                dma_phys: tr.dma_phys,
                queue_size,
                avail_idx: 0,
                last_used_idx: 0,
                yield_fn,
            })
        }
    }

    unsafe fn submit_and_wait(&mut self, head_desc: u16) -> Result<u32, VirtioError> {
        let avail_ptr = (self.dma_va + OFF_AVAIL) as *mut VirtqAvail;
        let used_ptr = (self.dma_va + OFF_USED) as *const VirtqUsed;

        unsafe {
            let slot = (self.avail_idx % self.queue_size) as usize;
            write_volatile(&raw mut (*avail_ptr).ring[slot], head_desc);
            io_barrier();

            self.avail_idx = self.avail_idx.wrapping_add(1);
            write_volatile(&raw mut (*avail_ptr).idx, self.avail_idx);
            io_barrier();

            write_volatile(self.notify_ptr, self.queue_idx);

            let mut spins = 0u32;
            loop {
                io_barrier();
                let u_idx = read_volatile(&raw const (*used_ptr).idx);
                if u_idx != self.last_used_idx {
                    let used_slot = (self.last_used_idx % self.queue_size) as usize;
                    let used_len = read_volatile(&raw const (*used_ptr).ring[used_slot].len);
                    self.last_used_idx = self.last_used_idx.wrapping_add(1);
                    return Ok(used_len);
                }
                spins += 1;
                if spins > 20_000_000 {
                    return Err(VirtioError::Timeout);
                }
                if (spins & 0x3FF) == 0 {
                    (self.yield_fn)();
                } else {
                    core::hint::spin_loop();
                }
            }
        }
    }
}

pub struct VirtioBlk {
    vq: SplitQueue,
    capacity_sectors: u64,
}

impl VirtioBlk {
    pub fn init(tr: VirtioBlkTransport, yield_fn: fn()) -> Result<Self, VirtioError> {
        if tr.device_cfg_va == 0 {
            return Err(VirtioError::BadConfig);
        }
        let vq = SplitQueue::init(tr, 0, yield_fn)?;
        let capacity_sectors = unsafe { mmio_r64_split(tr.device_cfg_va, 0x00) };
        Ok(Self { vq, capacity_sectors })
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    pub fn status_byte(&self) -> u8 {
        unsafe { mmio_r8(self.vq.common_cfg_va, 0x14) }
    }

    pub fn read_sectors(&mut self, lba: u64, out: &mut [u8]) -> Result<(), VirtioError> {
        if !out.len().is_multiple_of(512) {
            return Err(VirtioError::InvalidArg);
        }
        let mut off = 0usize;
        let mut cur_lba = lba;
        while off < out.len() {
            let chunk = (out.len() - off).min(MAX_XFER_CHUNK);
            self.xfer_chunk(false, cur_lba, &mut out[off..off + chunk])?;
            off += chunk;
            cur_lba += (chunk / 512) as u64;
        }
        Ok(())
    }

    pub fn write_sectors(&mut self, lba: u64, data: &[u8]) -> Result<(), VirtioError> {
        if !data.len().is_multiple_of(512) {
            return Err(VirtioError::InvalidArg);
        }
        let mut off = 0usize;
        let mut cur_lba = lba;
        while off < data.len() {
            let chunk = (data.len() - off).min(MAX_XFER_CHUNK);
            let mut copy_buf = [0u8; MAX_XFER_CHUNK];
            copy_buf[..chunk].copy_from_slice(&data[off..off + chunk]);
            self.xfer_chunk(true, cur_lba, &mut copy_buf[..chunk])?;
            off += chunk;
            cur_lba += (chunk / 512) as u64;
        }
        Ok(())
    }

    fn xfer_chunk(&mut self, is_write: bool, lba: u64, buf: &mut [u8]) -> Result<(), VirtioError> {
        if buf.is_empty() || buf.len() > MAX_XFER_CHUNK || !buf.len().is_multiple_of(512) {
            return Err(VirtioError::InvalidArg);
        }
        let hdr_ptr = (self.vq.dma_va + OFF_REQ_HDR) as *mut VirtioBlkReqHdr;
        let status_ptr = (self.vq.dma_va + OFF_STATUS) as *mut u8;
        let data_ptr = (self.vq.dma_va + OFF_DATA) as *mut u8;
        let desc_ptr = (self.vq.dma_va + OFF_DESC) as *mut VirtqDesc;

        unsafe {
            write_volatile(
                hdr_ptr,
                VirtioBlkReqHdr {
                    req_type: if is_write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN },
                    reserved: 0,
                    sector: lba,
                },
            );
            write_volatile(status_ptr, 0xFF);
            if is_write {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), data_ptr, buf.len());
            }

            write_volatile(
                desc_ptr.add(0),
                VirtqDesc {
                    addr: self.vq.dma_phys + OFF_REQ_HDR as u64,
                    len: core::mem::size_of::<VirtioBlkReqHdr>() as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: 1,
                },
            );
            write_volatile(
                desc_ptr.add(1),
                VirtqDesc {
                    addr: self.vq.dma_phys + OFF_DATA as u64,
                    len: buf.len() as u32,
                    flags: VIRTQ_DESC_F_NEXT | if is_write { 0 } else { VIRTQ_DESC_F_WRITE },
                    next: 2,
                },
            );
            write_volatile(
                desc_ptr.add(2),
                VirtqDesc {
                    addr: self.vq.dma_phys + OFF_STATUS as u64,
                    len: 1,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );

            let _ = self.vq.submit_and_wait(0)?;
            if read_volatile(status_ptr) != 0 {
                return Err(VirtioError::IoError);
            }
            if !is_write {
                core::ptr::copy_nonoverlapping(data_ptr, buf.as_mut_ptr(), buf.len());
            }
        }
        Ok(())
    }
}

// =============================================================================
// VirtIO-GPU 2D Controlq Driver (VirtIO Spec 5.7)
// =============================================================================

const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VirtioGpuCtrlHdr {
    cmd_type: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    ring_idx: u8,
    padding: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VirtioGpuRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VirtioGpuDisplayOne {
    r: VirtioGpuRect,
    enabled: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuRespDisplayInfo {
    hdr: VirtioGpuCtrlHdr,
    pmodes: [VirtioGpuDisplayOne; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuResourceCreate2d {
    hdr: VirtioGpuCtrlHdr,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuAttachBacking {
    hdr: VirtioGpuCtrlHdr,
    resource_id: u32,
    nr_entries: u32,
    addr: u64,
    length: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuSetScanout {
    hdr: VirtioGpuCtrlHdr,
    r: VirtioGpuRect,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuTransferToHost2d {
    hdr: VirtioGpuCtrlHdr,
    r: VirtioGpuRect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuResourceFlush {
    hdr: VirtioGpuCtrlHdr,
    r: VirtioGpuRect,
    resource_id: u32,
    padding: u32,
}

pub struct VirtioGpu {
    vq: SplitQueue,
    fb_va: usize,
    width: u32,
    height: u32,
    resource_id: u32,
}

impl VirtioGpu {
    /// Initializes VirtIO-GPU 2D scanout bound to contiguous backing DMA memory `(fb_va, fb_phys, fb_bytes)`.
    pub fn init(
        tr: VirtioTransport,
        fb_va: usize,
        fb_phys: u64,
        fb_bytes: usize,
        yield_fn: fn(),
    ) -> Result<Self, VirtioError> {
        let mut vq = SplitQueue::init(tr, 0, yield_fn)?;

        // Query host display dimensions.
        let hdr_in = VirtioGpuCtrlHdr {
            cmd_type: VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
            ..Default::default()
        };
        let mut width = 1024u32;
        let mut height = 768u32;
        unsafe {
            let resp: VirtioGpuRespDisplayInfo =
                Self::ctrl_cmd(&mut vq, &hdr_in)?;
            if resp.hdr.cmd_type == VIRTIO_GPU_RESP_OK_DISPLAY_INFO && resp.pmodes[0].enabled != 0 {
                let w = resp.pmodes[0].r.width;
                let h = resp.pmodes[0].r.height;
                if w > 0 && h > 0 && (w as usize) * (h as usize) * 4 <= fb_bytes {
                    width = w;
                    height = h;
                }
            }
        }

        if (width as usize) * (height as usize) * 4 > fb_bytes {
            return Err(VirtioError::InvalidArg);
        }

        let resource_id = 1u32;
        let create = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                ..Default::default()
            },
            resource_id,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
            width,
            height,
        };
        let attach = VirtioGpuAttachBacking {
            hdr: VirtioGpuCtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                ..Default::default()
            },
            resource_id,
            nr_entries: 1,
            addr: fb_phys,
            length: width * height * 4,
            padding: 0,
        };
        let scanout = VirtioGpuSetScanout {
            hdr: VirtioGpuCtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_SET_SCANOUT,
                ..Default::default()
            },
            r: VirtioGpuRect { x: 0, y: 0, width, height },
            scanout_id: 0,
            resource_id,
        };

        unsafe {
            let r1: VirtioGpuCtrlHdr = Self::ctrl_cmd(&mut vq, &create)?;
            if r1.cmd_type != VIRTIO_GPU_RESP_OK_NODATA {
                return Err(VirtioError::IoError);
            }
            let r2: VirtioGpuCtrlHdr = Self::ctrl_cmd(&mut vq, &attach)?;
            if r2.cmd_type != VIRTIO_GPU_RESP_OK_NODATA {
                return Err(VirtioError::IoError);
            }
            let r3: VirtioGpuCtrlHdr = Self::ctrl_cmd(&mut vq, &scanout)?;
            if r3.cmd_type != VIRTIO_GPU_RESP_OK_NODATA {
                return Err(VirtioError::IoError);
            }
        }

        Ok(Self {
            vq,
            fb_va,
            width,
            height,
            resource_id,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn backbuffer_mut(&mut self) -> &mut [u32] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.fb_va as *mut u32,
                (self.width as usize) * (self.height as usize),
            )
        }
    }

    /// Presents damage rectangle `(x, y, w, h)` to the host scanout.
    pub fn flush_rect(&mut self, x: u32, y: u32, w: u32, h: u32) -> Result<(), VirtioError> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let rect = VirtioGpuRect { x, y, width: w, height: h };
        let offset = ((y as u64) * (self.width as u64) + (x as u64)) * 4;
        let xfer = VirtioGpuTransferToHost2d {
            hdr: VirtioGpuCtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
                ..Default::default()
            },
            r: rect,
            offset,
            resource_id: self.resource_id,
            padding: 0,
        };
        let flush = VirtioGpuResourceFlush {
            hdr: VirtioGpuCtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                ..Default::default()
            },
            r: rect,
            resource_id: self.resource_id,
            padding: 0,
        };
        unsafe {
            let r1: VirtioGpuCtrlHdr = Self::ctrl_cmd(&mut self.vq, &xfer)?;
            if r1.cmd_type != VIRTIO_GPU_RESP_OK_NODATA {
                return Err(VirtioError::IoError);
            }
            let r2: VirtioGpuCtrlHdr = Self::ctrl_cmd(&mut self.vq, &flush)?;
            if r2.cmd_type != VIRTIO_GPU_RESP_OK_NODATA {
                return Err(VirtioError::IoError);
            }
        }
        Ok(())
    }

    unsafe fn ctrl_cmd<Req: Copy, Resp: Copy>(
        vq: &mut SplitQueue,
        req: &Req,
    ) -> Result<Resp, VirtioError> {
        let req_len = core::mem::size_of::<Req>();
        let resp_len = core::mem::size_of::<Resp>();
        let req_ptr = (vq.dma_va + OFF_REQ_HDR) as *mut Req;
        let resp_ptr = (vq.dma_va + OFF_RESP_BUF) as *mut Resp;
        let desc_ptr = (vq.dma_va + OFF_DESC) as *mut VirtqDesc;

        unsafe {
            write_volatile(req_ptr, *req);
            core::ptr::write_bytes(resp_ptr as *mut u8, 0, resp_len);
            write_volatile(
                desc_ptr.add(0),
                VirtqDesc {
                    addr: vq.dma_phys + OFF_REQ_HDR as u64,
                    len: req_len as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: 1,
                },
            );
            write_volatile(
                desc_ptr.add(1),
                VirtqDesc {
                    addr: vq.dma_phys + OFF_RESP_BUF as u64,
                    len: resp_len as u32,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );
            let _ = vq.submit_and_wait(0)?;
            Ok(read_volatile(resp_ptr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_descriptor_abi_sizes() {
        assert_eq!(core::mem::size_of::<VirtqDesc>(), 16);
        assert_eq!(core::mem::size_of::<VirtqUsedElem>(), 8);
        assert_eq!(core::mem::size_of::<VirtioBlkReqHdr>(), 16);
        assert!(OFF_RESP_BUF + core::mem::size_of::<VirtioGpuRespDisplayInfo>() <= OFF_DATA);
        assert!(OFF_DATA + MAX_XFER_CHUNK <= DMA_REQUIRED_BYTES);
    }
}
