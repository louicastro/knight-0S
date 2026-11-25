//! Architecture-independent Virtual Memory Manager (`KernelVmm`) over [`MmuArch`].

use core::marker::PhantomData;

use knightos_hal::{MmuArch, PAGE_SIZE_4K, PageFlags, PhysAddr, PhysFrameAllocator, VirtAddr};

pub struct KernelVmm<M: MmuArch> {
    root: PhysAddr,
    hhdm_offset: u64,
    _marker: PhantomData<M>,
}

impl<M: MmuArch> KernelVmm<M> {
    pub fn from_active(hhdm_offset: u64) -> Self {
        Self {
            root: M::active_kernel_root(),
            hhdm_offset,
            _marker: PhantomData,
        }
    }

    pub const fn root(&self) -> PhysAddr {
        self.root
    }

    pub const fn hhdm_offset(&self) -> u64 {
        self.hhdm_offset
    }

    pub fn clear_trampoline(&self) {
        M::clear_lower_half(self.hhdm_offset);
    }

    pub fn map_range(
        &self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        byte_len: usize,
        flags: PageFlags,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<(), &'static str> {
        if !flags.is_wx_valid() {
            return Err("KernelVmm rejected W^X mapping");
        }
        let pages = byte_len.div_ceil(PAGE_SIZE_4K);
        for i in 0..pages {
            let off = (i * PAGE_SIZE_4K) as u64;
            M::map_4k(
                self.root,
                self.hhdm_offset,
                VirtAddr(virt_start.as_u64() + off),
                PhysAddr(phys_start.as_u64() + off),
                flags,
                alloc,
            )?;
        }
        Ok(())
    }

    pub fn unmap_range(&self, virt_start: VirtAddr, byte_len: usize) {
        let pages = byte_len.div_ceil(PAGE_SIZE_4K);
        for i in 0..pages {
            let off = (i * PAGE_SIZE_4K) as u64;
            let _ = M::unmap_4k(self.root, self.hhdm_offset, VirtAddr(virt_start.as_u64() + off));
        }
    }
}
