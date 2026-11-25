//! x86-64 4-level paging builder (`PML4 -> PDPT -> PD -> PT`).

use crate::uefi::alloc::PageAllocator;
use super::{BootstrapMapper, CacheMode, MapFlags, PAGE_SIZE_2M, PAGE_SIZE_4K};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const HUGE_PAGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const NO_EXEC: u64 = 1 << 63;

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

pub struct X86PageMapper {
    pml4_phys: u64,
}

impl X86PageMapper {
    pub fn new(alloc: &mut dyn PageAllocator) -> Result<Self, &'static str> {
        let pml4_phys = alloc.alloc_zeroed_page().ok_or("OOM allocating PML4")?;
        Ok(Self { pml4_phys })
    }

    pub const fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    fn encode_leaf_flags(flags: MapFlags, _allow_wx_trampoline: bool) -> u64 {
        let mut bits = PRESENT | GLOBAL;
        if flags.writable {
            bits |= WRITABLE;
        }
        if !flags.executable {
            bits |= NO_EXEC;
        }
        match flags.cache {
            CacheMode::NormalWriteBack => {}
            CacheMode::NormalUncached => bits |= PWT,
            CacheMode::DeviceMmio => bits |= PWT | PCD,
        }
        bits
    }

    fn ensure_next_table(
        table_phys: u64,
        index: usize,
        alloc: &mut dyn PageAllocator,
    ) -> Result<u64, &'static str> {
        let table = unsafe { &mut *(table_phys as *mut [u64; 512]) };
        let entry = table[index];
        if (entry & PRESENT) != 0 {
            if (entry & HUGE_PAGE) != 0 {
                return Err("overlapping 2M huge page in x86_64 page table");
            }
            return Ok(entry & ADDR_MASK);
        }
        let next = alloc.alloc_zeroed_page().ok_or("OOM allocating x86_64 page table")?;
        table[index] = next | PRESENT | WRITABLE;
        Ok(next)
    }

    fn map_4k_inner(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
        allow_wx_trampoline: bool,
    ) -> Result<(), &'static str> {
        let l4 = ((virt >> 39) & 0x1FF) as usize;
        let l3 = ((virt >> 30) & 0x1FF) as usize;
        let l2 = ((virt >> 21) & 0x1FF) as usize;
        let l1 = ((virt >> 12) & 0x1FF) as usize;

        let pdpt = Self::ensure_next_table(self.pml4_phys, l4, alloc)?;
        let pd = Self::ensure_next_table(pdpt, l3, alloc)?;
        let pt = Self::ensure_next_table(pd, l2, alloc)?;

        let pt_table = unsafe { &mut *(pt as *mut [u64; 512]) };
        pt_table[l1] = (phys & ADDR_MASK) | Self::encode_leaf_flags(flags, allow_wx_trampoline);
        Ok(())
    }

    fn map_2m_inner(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
        allow_wx_trampoline: bool,
    ) -> Result<(), &'static str> {
        let l4 = ((virt >> 39) & 0x1FF) as usize;
        let l3 = ((virt >> 30) & 0x1FF) as usize;
        let l2 = ((virt >> 21) & 0x1FF) as usize;

        let pdpt = Self::ensure_next_table(self.pml4_phys, l4, alloc)?;
        let pd = Self::ensure_next_table(pdpt, l3, alloc)?;

        let pd_table = unsafe { &mut *(pd as *mut [u64; 512]) };
        pd_table[l2] =
            (phys & ADDR_MASK) | HUGE_PAGE | Self::encode_leaf_flags(flags, allow_wx_trampoline);
        Ok(())
    }

    /// Maps lower-half trampoline pages (`knightboot` image) during `CR3` transition.
    pub fn map_range_trampoline(
        &mut self,
        alloc: &mut dyn PageAllocator,
        mut virt: u64,
        mut phys: u64,
        mut size: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str> {
        while size > 0 {
            if size >= PAGE_SIZE_2M
                && (virt & (PAGE_SIZE_2M - 1)) == 0
                && (phys & (PAGE_SIZE_2M - 1)) == 0
            {
                self.map_2m_inner(alloc, virt, phys, flags, true)?;
                virt += PAGE_SIZE_2M;
                phys += PAGE_SIZE_2M;
                size -= PAGE_SIZE_2M;
            } else {
                self.map_4k_inner(alloc, virt, phys, flags, true)?;
                virt += PAGE_SIZE_4K;
                phys += PAGE_SIZE_4K;
                size = size.saturating_sub(PAGE_SIZE_4K);
            }
        }
        Ok(())
    }
}

impl BootstrapMapper for X86PageMapper {
    fn map_4k(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str> {
        debug_assert!(!(flags.writable && flags.executable));
        self.map_4k_inner(alloc, virt, phys, flags, false)
    }

    fn map_2m(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str> {
        debug_assert!(!(flags.writable && flags.executable));
        self.map_2m_inner(alloc, virt, phys, flags, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(4096))]
    struct MockAlloc {
        pool: [[u64; 512]; 16],
        next: usize,
    }

    impl MockAlloc {
        fn new() -> Self {
            Self {
                pool: [[0; 512]; 16],
                next: 0,
            }
        }
    }

    impl PageAllocator for MockAlloc {
        fn alloc_zeroed_page(&mut self) -> Option<u64> {
            if self.next >= self.pool.len() {
                return None;
            }
            let ptr = self.pool[self.next].as_mut_ptr() as u64;
            self.next += 1;
            Some(ptr)
        }
    }

    #[test]
    fn x86_page_flags_enforce_wx() {
        let mut alloc = MockAlloc::new();
        let mut mapper = X86PageMapper::new(&mut alloc).unwrap();
        let va = 0xFFFF_FFFF_8000_0000u64;
        mapper
            .map_4k(&mut alloc, va, 0x20_0000, MapFlags::kernel_code())
            .unwrap();

        let pml4 = unsafe { &*(mapper.pml4_phys() as *const [u64; 512]) };
        let pdpt_phys = pml4[511] & ADDR_MASK;
        let pdpt = unsafe { &*(pdpt_phys as *const [u64; 512]) };
        let pd_phys = pdpt[510] & ADDR_MASK;
        let pd = unsafe { &*(pd_phys as *const [u64; 512]) };
        let pt_phys = pd[0] & ADDR_MASK;
        let pt = unsafe { &*(pt_phys as *const [u64; 512]) };
        let pte = pt[0];

        assert_ne!(pte & PRESENT, 0);
        assert_eq!(pte & WRITABLE, 0); // Read-only code
        assert_eq!(pte & NO_EXEC, 0);  // Executable
    }
}
