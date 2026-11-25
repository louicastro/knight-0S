#![cfg_attr(not(test), no_std)]
//! KnightOS User-Space Device Manager (`devmgr`): PCIe ECAM enumeration and VirtIO binding.

use core::ptr::{read_volatile, write_volatile};

use knightfs::{BlockDevice, FsError};
use knightos_nvme::{NVME_DMA_BYTES, NvmeController, NvmeError};
use knightos_virtio::{
    DMA_REQUIRED_BYTES, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG,
    VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_PCI_DEV_BLK_MODERN, VIRTIO_PCI_DEV_BLK_TRANSITIONAL,
    VIRTIO_PCI_DEV_GPU_MODERN, VIRTIO_PCI_DEV_GPU_TRANSITIONAL, VIRTIO_PCI_VENDOR, VirtioBlk,
    VirtioError, VirtioGpu, VirtioTransport,
};
use libknight::{
    Handle, KnightStatus, mmio_map, pci_ecam_info, thread_yield, vmar_flags, vmar_map,
    vmo_create_dma,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevmgrError {
    NoEcam,
    NoVirtioBlk,
    NoVirtioGpu,
    NoNvme,
    UnassignedBar,
    Syscall(KnightStatus),
    Virtio(VirtioError),
    Nvme(NvmeError),
}

#[derive(Clone, Copy, Default)]
struct PciCapSlice {
    found: bool,
    bar: u8,
    offset: u32,
    length: u32,
}

pub struct VirtioBlockDev {
    pub bdf: (u8, u8, u8),
    pub vendor_id: u16,
    pub device_id: u16,
    blk: VirtioBlk,
}

impl VirtioBlockDev {
    pub fn capacity_sectors(&self) -> u64 {
        self.blk.capacity_sectors()
    }
}

impl BlockDevice for VirtioBlockDev {
    fn sector_size(&self) -> usize {
        512
    }

    fn sector_count(&self) -> u64 {
        self.blk.capacity_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        self.blk.read_sectors(lba, buf).map_err(|_| FsError::IoError)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        self.blk.write_sectors(lba, buf).map_err(|_| FsError::IoError)
    }
}

pub struct VirtioGpuDev {
    pub bdf: (u8, u8, u8),
    pub gpu: VirtioGpu,
}

#[inline(always)]
unsafe fn cfg_r8(base: usize, off: usize) -> u8 {
    unsafe { read_volatile((base + off) as *const u8) }
}

#[inline(always)]
unsafe fn cfg_r16(base: usize, off: usize) -> u16 {
    unsafe { read_volatile((base + off) as *const u16) }
}

#[inline(always)]
unsafe fn cfg_w16(base: usize, off: usize, v: u16) {
    unsafe { write_volatile((base + off) as *mut u16, v) }
}

#[inline(always)]
unsafe fn cfg_r32(base: usize, off: usize) -> u32 {
    unsafe { read_volatile((base + off) as *const u32) }
}

fn yield_hook() {
    let _ = thread_yield();
}

unsafe fn read_pci_bar_phys(cfg_va: usize, bar_idx: u8) -> u64 {
    if bar_idx >= 6 {
        return 0;
    }
    let off = 0x10 + (bar_idx as usize) * 4;
    let lo = unsafe { cfg_r32(cfg_va, off) };
    if (lo & 1) != 0 {
        return 0;
    }
    let is_64 = (lo & 0x6) == 0x4;
    let addr_lo = (lo & !0xF) as u64;
    if is_64 && bar_idx + 1 < 6 {
        let hi = unsafe { cfg_r32(cfg_va, off + 4) } as u64;
        (hi << 32) | addr_lo
    } else {
        addr_lo
    }
}

fn map_cap_slice(hw_root: Handle, cfg_va: usize, cap: PciCapSlice) -> Result<usize, DevmgrError> {
    let bar_phys = unsafe { read_pci_bar_phys(cfg_va, cap.bar) };
    if bar_phys == 0 {
        return Err(DevmgrError::UnassignedBar);
    }
    let abs_phys = bar_phys + cap.offset as u64;
    let page_base = abs_phys & !0xFFF;
    let page_off = (abs_phys & 0xFFF) as usize;
    let map_len = (page_off + (cap.length as usize).max(4096) + 0xFFF) & !0xFFF;
    let mapped_va = mmio_map(hw_root, page_base, map_len).map_err(DevmgrError::Syscall)? as usize;
    Ok(mapped_va + page_off)
}

struct ProbedVirtioPci {
    bdf: (u8, u8, u8),
    vendor_id: u16,
    device_id: u16,
    transport: VirtioTransport,
}

fn scan_virtio_pci<F: Fn(u16, u16) -> bool>(
    hw_root: Handle,
    matcher: F,
    require_device_cfg: bool,
) -> Result<Option<ProbedVirtioPci>, DevmgrError> {
    let (ecam_phys, _) = pci_ecam_info(hw_root).map_err(DevmgrError::Syscall)?;
    if ecam_phys == 0 {
        return Err(DevmgrError::NoEcam);
    }

    let bus0_va =
        mmio_map(hw_root, ecam_phys, 32 * 8 * 4096).map_err(DevmgrError::Syscall)? as usize;

    for dev in 0..32u8 {
        for func in 0..8u8 {
            let cfg_va = bus0_va + (((dev as usize) << 15) | ((func as usize) << 12));
            let vendor_id = unsafe { cfg_r16(cfg_va, 0x00) };
            if vendor_id == 0xFFFF {
                if func == 0 {
                    break;
                }
                continue;
            }
            let device_id = unsafe { cfg_r16(cfg_va, 0x02) };
            let subsys_id = unsafe { cfg_r16(cfg_va, 0x2E) };
            if vendor_id == VIRTIO_PCI_VENDOR && matcher(device_id, subsys_id) {
                let pci_status = unsafe { cfg_r16(cfg_va, 0x06) };
                if (pci_status & (1 << 4)) != 0 {
                    let mut common = PciCapSlice::default();
                    let mut notify = PciCapSlice::default();
                    let mut device = PciCapSlice::default();
                    let mut notify_off_multiplier = 0u32;

                    let mut cap_ptr = (unsafe { cfg_r8(cfg_va, 0x34) } & 0xFC) as usize;
                    let mut guard = 0usize;
                    while cap_ptr >= 0x40 && cap_ptr < 0x100 && guard < 48 {
                        guard += 1;
                        let cap_id = unsafe { cfg_r8(cfg_va, cap_ptr) };
                        let next_ptr = (unsafe { cfg_r8(cfg_va, cap_ptr + 1) } & 0xFC) as usize;
                        if cap_id == 0x09 {
                            let cfg_type = unsafe { cfg_r8(cfg_va, cap_ptr + 3) };
                            let bar = unsafe { cfg_r8(cfg_va, cap_ptr + 4) };
                            let offset = unsafe { cfg_r32(cfg_va, cap_ptr + 8) };
                            let length = unsafe { cfg_r32(cfg_va, cap_ptr + 12) };
                            match cfg_type {
                                VIRTIO_PCI_CAP_COMMON_CFG => {
                                    common = PciCapSlice { found: true, bar, offset, length };
                                }
                                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                                    notify = PciCapSlice { found: true, bar, offset, length };
                                    notify_off_multiplier = unsafe { cfg_r32(cfg_va, cap_ptr + 16) };
                                }
                                VIRTIO_PCI_CAP_DEVICE_CFG => {
                                    device = PciCapSlice { found: true, bar, offset, length };
                                }
                                _ => {}
                            }
                        }
                        cap_ptr = next_ptr;
                    }

                    if common.found && notify.found && (!require_device_cfg || device.found) {
                        let cmd = unsafe { cfg_r16(cfg_va, 0x04) };
                        unsafe { cfg_w16(cfg_va, 0x04, cmd | 0x0406) };

                        let common_va = map_cap_slice(hw_root, cfg_va, common)?;
                        let notify_va = map_cap_slice(hw_root, cfg_va, notify)?;
                        let device_va = if device.found {
                            map_cap_slice(hw_root, cfg_va, device)?
                        } else {
                            0
                        };

                        let (dma_vmo, dma_phys) = vmo_create_dma(hw_root, DMA_REQUIRED_BYTES)
                            .map_err(DevmgrError::Syscall)?;
                        let dma_va = vmar_map(
                            dma_vmo,
                            0,
                            DMA_REQUIRED_BYTES,
                            vmar_flags::READ | vmar_flags::WRITE,
                        )
                        .map_err(DevmgrError::Syscall)?
                            as usize;

                        return Ok(Some(ProbedVirtioPci {
                            bdf: (0, dev, func),
                            vendor_id,
                            device_id,
                            transport: VirtioTransport {
                                common_cfg_va: common_va,
                                notify_base_va: notify_va,
                                notify_off_multiplier,
                                device_cfg_va: device_va,
                                dma_va,
                                dma_phys,
                                dma_len: DMA_REQUIRED_BYTES,
                            },
                        }));
                    }
                }
            }

            if func == 0 {
                let hdr_type = unsafe { cfg_r8(cfg_va, 0x0E) };
                if (hdr_type & 0x80) == 0 {
                    break;
                }
            }
        }
    }
    Ok(None)
}

/// Discovers the first `virtio-blk-pci` device on PCIe Bus 0 using `hw_root` capabilities.
pub fn probe_virtio_blk(hw_root: Handle) -> Result<VirtioBlockDev, DevmgrError> {
    let found = scan_virtio_pci(
        hw_root,
        |dev_id, sub_id| {
            dev_id == VIRTIO_PCI_DEV_BLK_MODERN
                || (dev_id == VIRTIO_PCI_DEV_BLK_TRANSITIONAL && (sub_id == 2 || sub_id == 0))
        },
        true,
    )?
    .ok_or(DevmgrError::NoVirtioBlk)?;

    let blk = VirtioBlk::init(found.transport, yield_hook).map_err(DevmgrError::Virtio)?;
    Ok(VirtioBlockDev {
        bdf: found.bdf,
        vendor_id: found.vendor_id,
        device_id: found.device_id,
        blk,
    })
}

/// Discovers and binds the first `virtio-gpu-pci` 2D device on PCIe Bus 0.
pub fn probe_virtio_gpu(hw_root: Handle) -> Result<VirtioGpuDev, DevmgrError> {
    let found = scan_virtio_pci(
        hw_root,
        |dev_id, sub_id| {
            dev_id == VIRTIO_PCI_DEV_GPU_MODERN
                || (dev_id == VIRTIO_PCI_DEV_GPU_TRANSITIONAL && (sub_id == 16 || sub_id == 0))
        },
        false,
    )?
    .ok_or(DevmgrError::NoVirtioGpu)?;

    // Allocate up to 1280x720x4 contiguous DMA backing framebuffer (3.69 MiB < 4 MiB DMA cap).
    let fb_bytes = 1280 * 720 * 4;
    let (fb_vmo, fb_phys) = vmo_create_dma(hw_root, fb_bytes).map_err(DevmgrError::Syscall)?;
    let fb_va = vmar_map(fb_vmo, 0, fb_bytes, vmar_flags::READ | vmar_flags::WRITE)
        .map_err(DevmgrError::Syscall)? as usize;

    let gpu = VirtioGpu::init(found.transport, fb_va, fb_phys, fb_bytes, yield_hook)
        .map_err(DevmgrError::Virtio)?;
    Ok(VirtioGpuDev {
        bdf: found.bdf,
        gpu,
    })
}

pub struct NvmeDev {
    pub bdf: (u8, u8, u8),
    pub ctrl: NvmeController,
}

impl BlockDevice for NvmeDev {
    fn sector_size(&self) -> usize {
        self.ctrl.sector_size()
    }
    fn sector_count(&self) -> u64 {
        self.ctrl.sector_count()
    }
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        self.ctrl.read_sectors(lba, buf)
    }
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        self.ctrl.write_sectors(lba, buf)
    }
}

/// Discovers the first PCIe NVMe controller (class 0x01, subclass 0x08, prog-if 0x02).
pub fn probe_nvme(hw_root: Handle) -> Result<NvmeDev, DevmgrError> {
    let (ecam_phys, _) = pci_ecam_info(hw_root).map_err(DevmgrError::Syscall)?;
    if ecam_phys == 0 {
        return Err(DevmgrError::NoEcam);
    }
    let bus0_va =
        mmio_map(hw_root, ecam_phys, 32 * 8 * 4096).map_err(DevmgrError::Syscall)? as usize;

    for dev in 0..32u8 {
        for func in 0..8u8 {
            let cfg_va = bus0_va + (((dev as usize) << 15) | ((func as usize) << 12));
            let vendor_id = unsafe { cfg_r16(cfg_va, 0x00) };
            if vendor_id == 0xFFFF {
                if func == 0 {
                    break;
                }
                continue;
            }
            let class_rev = unsafe { cfg_r32(cfg_va, 0x08) };
            let class_code = (class_rev >> 8) & 0x00FF_FFFF;
            if class_code == 0x010802 {
                let cmd = unsafe { cfg_r16(cfg_va, 0x04) };
                unsafe { cfg_w16(cfg_va, 0x04, cmd | 0x0406) };

                let bar0_phys = unsafe { read_pci_bar_phys(cfg_va, 0) };
                if bar0_phys == 0 {
                    return Err(DevmgrError::UnassignedBar);
                }
                let bar0_va = mmio_map(hw_root, bar0_phys & !0xFFF, 16384)
                    .map_err(DevmgrError::Syscall)? as usize;
                let (dma_vmo, dma_phys) =
                    vmo_create_dma(hw_root, NVME_DMA_BYTES).map_err(DevmgrError::Syscall)?;
                let dma_va = vmar_map(
                    dma_vmo,
                    0,
                    NVME_DMA_BYTES,
                    vmar_flags::READ | vmar_flags::WRITE,
                )
                .map_err(DevmgrError::Syscall)? as usize;

                let ctrl =
                    NvmeController::init(bar0_va, dma_va, dma_phys, NVME_DMA_BYTES, yield_hook)
                        .map_err(DevmgrError::Nvme)?;
                return Ok(NvmeDev {
                    bdf: (0, dev, func),
                    ctrl,
                });
            }
            if func == 0 && (unsafe { cfg_r8(cfg_va, 0x0E) } & 0x80) == 0 {
                break;
            }
        }
    }
    Err(DevmgrError::NoNvme)
}
