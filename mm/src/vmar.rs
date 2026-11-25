//! Per-task Virtual Memory Address Region (`Vmar`) manager for user space.

use alloc::sync::Arc;
use alloc::vec::Vec;

use knightos_hal::{
    CachePolicy, MmuArch, PAGE_SIZE_4K, PageFlags, PhysAddr, PhysFrameAllocator, VirtAddr,
};

use crate::vmo::Vmo;

pub const USER_VA_MIN: u64 = 0x0000_0000_0040_0000;
pub const USER_VA_MAX: u64 = 0x0000_7FFF_FFFF_0000;
const AUTO_ALLOC_BASE: u64 = 0x0000_1000_0000_0000;

#[derive(Clone)]
pub struct VmarMapping {
    pub vaddr: u64,
    pub len: usize,
    pub flags: PageFlags,
    pub vmo: Option<Arc<Vmo>>,
}

/// Tracks all user-space virtual memory mappings inside one task's lower half.
pub struct Vmar {
    root: PhysAddr,
    hhdm_offset: u64,
    next_auto_va: u64,
    mappings: Vec<VmarMapping>,
}

impl Vmar {
    pub fn new(root: PhysAddr, hhdm_offset: u64) -> Self {
        Self {
            root,
            hhdm_offset,
            next_auto_va: AUTO_ALLOC_BASE,
            mappings: Vec::new(),
        }
    }

    pub const fn root(&self) -> PhysAddr {
        self.root
    }

    pub fn user_page_flags(writable: bool, executable: bool) -> Result<PageFlags, &'static str> {
        let flags = PageFlags {
            readable: true,
            writable,
            executable,
            user: true,
            global: false,
            cache: CachePolicy::WriteBack,
        };
        if !flags.is_wx_valid() {
            return Err("W^X violation in Vmar");
        }
        Ok(flags)
    }

    fn overlaps(&self, start: u64, len: usize) -> bool {
        let end = start.saturating_add(len as u64);
        self.mappings.iter().any(|m| {
            let m_end = m.vaddr.saturating_add(m.len as u64);
            start < m_end && m.vaddr < end
        })
    }

    pub fn map_vmo_at<M: MmuArch>(
        &mut self,
        vaddr: u64,
        vmo: Arc<Vmo>,
        vmo_offset: usize,
        len: usize,
        flags: PageFlags,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<u64, &'static str> {
        if !flags.is_wx_valid() || !flags.user {
            return Err("invalid user Vmar flags");
        }
        if (vaddr & (PAGE_SIZE_4K as u64 - 1)) != 0
            || (vmo_offset & (PAGE_SIZE_4K - 1)) != 0
            || len == 0
        {
            return Err("unaligned or empty Vmar mapping");
        }
        let aligned_len = len
            .checked_add(PAGE_SIZE_4K - 1)
            .map(|v| v & !(PAGE_SIZE_4K - 1))
            .ok_or("Vmar length overflow")?;
        let end = vaddr.checked_add(aligned_len as u64).ok_or("VA overflow")?;
        if vaddr < USER_VA_MIN || end > USER_VA_MAX {
            return Err("VA outside lower-half user bounds");
        }
        if vmo_offset.saturating_add(aligned_len) > vmo.size_bytes() {
            return Err("Vmar mapping exceeds VMO size");
        }
        if self.overlaps(vaddr, aligned_len) {
            return Err("overlapping Vmar mapping");
        }

        let start_page = vmo_offset / PAGE_SIZE_4K;
        let pages = aligned_len / PAGE_SIZE_4K;
        for i in 0..pages {
            let phys = vmo
                .frame_at(start_page + i)
                .ok_or("missing VMO backing frame")?;
            let page_va = VirtAddr(vaddr + (i * PAGE_SIZE_4K) as u64);
            if let Err(err) = M::map_4k(self.root, self.hhdm_offset, page_va, phys, flags, alloc) {
                for j in 0..i {
                    let _ = M::unmap_4k(
                        self.root,
                        self.hhdm_offset,
                        VirtAddr(vaddr + (j * PAGE_SIZE_4K) as u64),
                    );
                }
                return Err(err);
            }
        }

        self.mappings.push(VmarMapping {
            vaddr,
            len: aligned_len,
            flags,
            vmo: Some(vmo),
        });
        Ok(vaddr)
    }

    /// Maps an arbitrary physical MMIO aperture into user space as `DeviceMmio` (non-cacheable, non-executable).
    pub fn map_mmio_auto<M: MmuArch>(
        &mut self,
        phys_base: u64,
        len: usize,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<u64, &'static str> {
        if (phys_base & (PAGE_SIZE_4K as u64 - 1)) != 0 || len == 0 {
            return Err("unaligned or empty MMIO range");
        }
        let aligned_len = len
            .checked_add(PAGE_SIZE_4K - 1)
            .map(|v| v & !(PAGE_SIZE_4K - 1))
            .ok_or("MMIO length overflow")?;
        if aligned_len > 256 * 1024 * 1024 {
            return Err("MMIO range too large");
        }
        let flags = PageFlags {
            readable: true,
            writable: true,
            executable: false,
            user: true,
            global: false,
            cache: CachePolicy::DeviceMmio,
        };
        while self.overlaps(self.next_auto_va, aligned_len) {
            self.next_auto_va = self
                .next_auto_va
                .saturating_add(aligned_len as u64 + PAGE_SIZE_4K as u64);
        }
        let chosen = self.next_auto_va;
        let pages = aligned_len / PAGE_SIZE_4K;
        for i in 0..pages {
            let page_va = VirtAddr(chosen + (i * PAGE_SIZE_4K) as u64);
            let page_pa = PhysAddr(phys_base + (i * PAGE_SIZE_4K) as u64);
            if let Err(err) = M::map_4k(self.root, self.hhdm_offset, page_va, page_pa, flags, alloc) {
                for j in 0..i {
                    let _ = M::unmap_4k(
                        self.root,
                        self.hhdm_offset,
                        VirtAddr(chosen + (j * PAGE_SIZE_4K) as u64),
                    );
                }
                return Err(err);
            }
        }
        self.mappings.push(VmarMapping {
            vaddr: chosen,
            len: aligned_len,
            flags,
            vmo: None,
        });
        self.next_auto_va = chosen.saturating_add(aligned_len as u64 + PAGE_SIZE_4K as u64);
        Ok(chosen)
    }

    pub fn map_vmo_auto<M: MmuArch>(
        &mut self,
        vmo: Arc<Vmo>,
        vmo_offset: usize,
        len: usize,
        flags: PageFlags,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<u64, &'static str> {
        let aligned_len = len
            .checked_add(PAGE_SIZE_4K - 1)
            .map(|v| v & !(PAGE_SIZE_4K - 1))
            .ok_or("Vmar length overflow")?;
        // Leave one 4 KiB guard page between adjacent auto-placed regions.
        while self.overlaps(self.next_auto_va, aligned_len) {
            self.next_auto_va = self
                .next_auto_va
                .saturating_add(aligned_len as u64 + PAGE_SIZE_4K as u64);
        }
        let chosen = self.next_auto_va;
        let mapped = self.map_vmo_at::<M>(chosen, vmo, vmo_offset, aligned_len, flags, alloc)?;
        self.next_auto_va = chosen.saturating_add(aligned_len as u64 + PAGE_SIZE_4K as u64);
        Ok(mapped)
    }

    pub fn unmap<M: MmuArch>(&mut self, vaddr: u64, len: usize) -> Result<(), &'static str> {
        let aligned_len = len
            .checked_add(PAGE_SIZE_4K - 1)
            .map(|v| v & !(PAGE_SIZE_4K - 1))
            .ok_or("Vmar length overflow")?;
        let pos = self
            .mappings
            .iter()
            .position(|m| m.vaddr == vaddr && m.len == aligned_len)
            .ok_or("mapping not found")?;
        let pages = aligned_len / PAGE_SIZE_4K;
        for i in 0..pages {
            let _ = M::unmap_4k(
                self.root,
                self.hhdm_offset,
                VirtAddr(vaddr + (i * PAGE_SIZE_4K) as u64),
            );
        }
        self.mappings.swap_remove(pos);
        Ok(())
    }

    /// Validates that `[ptr, ptr + len)` lies completely within mapped user memory.
    pub fn validate_user_range(&self, ptr: u64, len: usize, need_write: bool) -> bool {
        if len == 0 {
            return true;
        }
        let Some(end) = ptr.checked_add(len as u64) else {
            return false;
        };
        if ptr < USER_VA_MIN || end > USER_VA_MAX {
            return false;
        }
        self.mappings.iter().any(|m| {
            let m_end = m.vaddr.saturating_add(m.len as u64);
            ptr >= m.vaddr && end <= m_end && (!need_write || m.flags.writable)
        })
    }
}
