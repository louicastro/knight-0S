#![cfg_attr(not(test), no_std)]
//! User-space PCIe NVMe 1.4 storage controller driver (`knightos-nvme`).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use knightfs::{BlockDevice, FsError};

pub const NVME_DMA_BYTES: usize = 7 * 4096; // Admin SQ/CQ, IO SQ/CQ, Identify, 8K Data
const Q_DEPTH: u16 = 16;

const OFF_ASQ: usize = 0x0000;
const OFF_ACQ: usize = 0x1000;
const OFF_ISQ: usize = 0x2000;
const OFF_ICQ: usize = 0x3000;
const OFF_IDENT: usize = 0x4000;
const OFF_DATA: usize = 0x5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeError {
    BadConfig,
    ControllerTimeout,
    CommandFailed(u16),
    NoNamespace,
    InvalidArg,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NvmeCmd {
    pub cdw0: u32,
    pub nsid: u32,
    pub rsvd2: [u32; 2],
    pub mptr: u64,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NvmeCqe {
    pub result: u32,
    pub rsvd: u32,
    pub sq_head_id: u32,
    pub status_cid: u32,
}

struct QueueState {
    sq_va: usize,
    cq_va: usize,
    sq_db: *mut u32,
    cq_db: *mut u32,
    sq_tail: u16,
    cq_head: u16,
    cq_phase: u16,
    next_cid: u16,
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
unsafe fn r32(base: usize, off: usize) -> u32 {
    unsafe { read_volatile((base + off) as *const u32) }
}

#[inline(always)]
unsafe fn w32(base: usize, off: usize, v: u32) {
    unsafe { write_volatile((base + off) as *mut u32, v) }
}

#[inline(always)]
unsafe fn r64(base: usize, off: usize) -> u64 {
    let lo = unsafe { r32(base, off) } as u64;
    let hi = unsafe { r32(base, off + 4) } as u64;
    (hi << 32) | lo
}

#[inline(always)]
unsafe fn w64(base: usize, off: usize, v: u64) {
    unsafe {
        w32(base, off, v as u32);
        w32(base, off + 4, (v >> 32) as u32);
    }
}

impl QueueState {
    unsafe fn submit(
        &mut self,
        mut cmd: NvmeCmd,
        yield_fn: fn(),
    ) -> Result<u32, NvmeError> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        cmd.cdw0 = (cmd.cdw0 & 0xFFFF) | ((cid as u32) << 16);

        let sq_entry = unsafe { (self.sq_va as *mut NvmeCmd).add(self.sq_tail as usize) };
        unsafe {
            write_volatile(sq_entry, cmd);
        }
        io_barrier();

        self.sq_tail = (self.sq_tail + 1) % Q_DEPTH;
        unsafe {
            write_volatile(self.sq_db, self.sq_tail as u32);
        }

        let cq_entry = unsafe { (self.cq_va as *const NvmeCqe).add(self.cq_head as usize) };
        for spins in 0..20_000_000u32 {
            io_barrier();
            let cqe = unsafe { read_volatile(cq_entry) };
            let phase = ((cqe.status_cid >> 16) & 1) as u16;
            if phase == self.cq_phase {
                let status = ((cqe.status_cid >> 17) & 0x7FFF) as u16;
                self.cq_head += 1;
                if self.cq_head >= Q_DEPTH {
                    self.cq_head = 0;
                    self.cq_phase ^= 1;
                }
                unsafe {
                    write_volatile(self.cq_db, self.cq_head as u32);
                }
                if status != 0 {
                    return Err(NvmeError::CommandFailed(status));
                }
                return Ok(cqe.result);
            }
            if (spins & 0x3FF) == 0 {
                yield_fn();
            } else {
                core::hint::spin_loop();
            }
        }
        Err(NvmeError::ControllerTimeout)
    }
}

pub struct NvmeController {
    admin: QueueState,
    io: QueueState,
    dma_va: usize,
    dma_phys: u64,
    sector_size: usize,
    sector_count: u64,
    nsid: u32,
    yield_fn: fn(),
}

impl NvmeController {
    pub fn init(
        bar0_va: usize,
        dma_va: usize,
        dma_phys: u64,
        dma_len: usize,
        yield_fn: fn(),
    ) -> Result<Self, NvmeError> {
        if bar0_va == 0 || dma_va == 0 || dma_len < NVME_DMA_BYTES {
            return Err(NvmeError::BadConfig);
        }

        unsafe {
            let cap = r64(bar0_va, 0x00);
            let dstrd = ((cap >> 32) & 0xF) as usize;
            let db_stride = 4usize << dstrd;

            // Disable controller before reconfiguring AQA/ASQ/ACQ.
            let mut cc = r32(bar0_va, 0x14);
            if (cc & 1) != 0 {
                cc &= !1;
                w32(bar0_va, 0x14, cc);
                for _ in 0..5_000_000 {
                    if (r32(bar0_va, 0x1C) & 1) == 0 {
                        break;
                    }
                    core::hint::spin_loop();
                }
            }

            core::ptr::write_bytes(dma_va as *mut u8, 0, NVME_DMA_BYTES);
            io_barrier();

            let q_max = (Q_DEPTH as u32) - 1;
            w32(bar0_va, 0x24, (q_max << 16) | q_max); // AQA
            w64(bar0_va, 0x28, dma_phys + OFF_ASQ as u64);
            w64(bar0_va, 0x30, dma_phys + OFF_ACQ as u64);

            // CC: IOCQES=4 (16B), IOSQES=6 (64B), MPS=0 (4K), EN=1.
            w32(bar0_va, 0x14, (4 << 20) | (6 << 16) | 1);
            let mut ready = false;
            for spins in 0..10_000_000u32 {
                if (r32(bar0_va, 0x1C) & 1) != 0 {
                    ready = true;
                    break;
                }
                if (spins & 0x3FF) == 0 {
                    yield_fn();
                }
            }
            if !ready {
                return Err(NvmeError::ControllerTimeout);
            }

            let mut admin = QueueState {
                sq_va: dma_va + OFF_ASQ,
                cq_va: dma_va + OFF_ACQ,
                sq_db: (bar0_va + 0x1000) as *mut u32,
                cq_db: (bar0_va + 0x1000 + db_stride) as *mut u32,
                sq_tail: 0,
                cq_head: 0,
                cq_phase: 1,
                next_cid: 1,
            };

            // Identify Controller (CNS = 1).
            let ident_ctrl = NvmeCmd {
                cdw0: 0x06,
                prp1: dma_phys + OFF_IDENT as u64,
                cdw10: 1,
                ..Default::default()
            };
            admin.submit(ident_ctrl, yield_fn)?;

            // Identify Namespace 1 (CNS = 0).
            let ident_ns = NvmeCmd {
                cdw0: 0x06,
                nsid: 1,
                prp1: dma_phys + OFF_IDENT as u64,
                cdw10: 0,
                ..Default::default()
            };
            admin.submit(ident_ns, yield_fn)?;

            let ident_ptr = (dma_va + OFF_IDENT) as *const u8;
            let nsze = core::ptr::read_unaligned(ident_ptr as *const u64);
            if nsze == 0 {
                return Err(NvmeError::NoNamespace);
            }
            let flbas = *ident_ptr.add(26) & 0xF;
            let lbaf = core::ptr::read_unaligned(ident_ptr.add(128 + (flbas as usize) * 4) as *const u32);
            let lbads = ((lbaf >> 16) & 0xFF) as u32;
            let sector_size = 1usize << lbads.clamp(9, 12);

            // Create I/O CQ (qid=1).
            let create_cq = NvmeCmd {
                cdw0: 0x05,
                prp1: dma_phys + OFF_ICQ as u64,
                cdw10: (q_max << 16) | 1,
                cdw11: 1, // Physically contiguous (PC=1), polling (IEN=0)
                ..Default::default()
            };
            admin.submit(create_cq, yield_fn)?;

            // Create I/O SQ (qid=1 -> cqid=1).
            let create_sq = NvmeCmd {
                cdw0: 0x01,
                prp1: dma_phys + OFF_ISQ as u64,
                cdw10: (q_max << 16) | 1,
                cdw11: (1 << 16) | 1, // CQID=1 | PC=1
                ..Default::default()
            };
            admin.submit(create_sq, yield_fn)?;

            let io = QueueState {
                sq_va: dma_va + OFF_ISQ,
                cq_va: dma_va + OFF_ICQ,
                sq_db: (bar0_va + 0x1000 + 2 * db_stride) as *mut u32,
                cq_db: (bar0_va + 0x1000 + 3 * db_stride) as *mut u32,
                sq_tail: 0,
                cq_head: 0,
                cq_phase: 1,
                next_cid: 100,
            };

            Ok(Self {
                admin,
                io,
                dma_va,
                dma_phys,
                sector_size,
                sector_count: nsze,
                nsid: 1,
                yield_fn,
            })
        }
    }

    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    pub fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn xfer_chunk(&mut self, is_write: bool, lba: u64, buf: &mut [u8]) -> Result<(), NvmeError> {
        let ssz = self.sector_size;
        if buf.is_empty() || buf.len() > 4096 || !buf.len().is_multiple_of(ssz) {
            return Err(NvmeError::InvalidArg);
        }
        let nlb = (buf.len() / ssz - 1) as u32;
        let data_ptr = (self.dma_va + OFF_DATA) as *mut u8;
        if is_write {
            unsafe {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), data_ptr, buf.len());
            }
        }
        let cmd = NvmeCmd {
            cdw0: if is_write { 0x01 } else { 0x02 },
            nsid: self.nsid,
            prp1: self.dma_phys + OFF_DATA as u64,
            cdw10: lba as u32,
            cdw11: (lba >> 32) as u32,
            cdw12: nlb,
            ..Default::default()
        };
        unsafe {
            self.io.submit(cmd, self.yield_fn)?;
            if !is_write {
                core::ptr::copy_nonoverlapping(data_ptr, buf.as_mut_ptr(), buf.len());
            }
        }
        let _ = &self.admin;
        Ok(())
    }
}

impl BlockDevice for NvmeController {
    fn sector_size(&self) -> usize {
        self.sector_size
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        if !buf.len().is_multiple_of(self.sector_size) {
            return Err(FsError::InvalidGeometry);
        }
        let mut off = 0usize;
        let mut cur_lba = lba;
        while off < buf.len() {
            let chunk = (buf.len() - off).min(4096);
            self.xfer_chunk(false, cur_lba, &mut buf[off..off + chunk])
                .map_err(|_| FsError::IoError)?;
            off += chunk;
            cur_lba += (chunk / self.sector_size) as u64;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        if !buf.len().is_multiple_of(self.sector_size) {
            return Err(FsError::IoError);
        }
        let mut off = 0usize;
        let mut cur_lba = lba;
        while off < buf.len() {
            let chunk = (buf.len() - off).min(4096);
            let mut copy = [0u8; 4096];
            copy[..chunk].copy_from_slice(&buf[off..off + chunk]);
            self.xfer_chunk(true, cur_lba, &mut copy[..chunk])
                .map_err(|_| FsError::IoError)?;
            off += chunk;
            cur_lba += (chunk / self.sector_size) as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvme_abi_struct_sizes() {
        assert_eq!(core::mem::size_of::<NvmeCmd>(), 64);
        assert_eq!(core::mem::size_of::<NvmeCqe>(), 16);
        assert!(OFF_DATA + 8192 <= NVME_DMA_BYTES);
    }
}
