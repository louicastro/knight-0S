//! AArch64 4-level stage-1 paging builder (4 KiB granule, 48-bit VA).
//!
//! `MAIR_EL1` indices assumed by [`ArmPageMapper`]:
//! - Index 0 (`0xFF`): Normal Write-Back Read/Write-Allocate RAM
//! - Index 1 (`0x00`): Device-nGnRnE MMIO
//! - Index 2 (`0x44`): Normal Non-Cacheable (framebuffer aperture)

use crate::uefi::alloc::PageAllocator;
use super::{BootstrapMapper, CacheMode, MapFlags};

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1; // L0/L1/L2 table = 0b11, L3 page = 0b11, L2 block = 0b01
const AF: u64 = 1 << 10;           // Access Flag
const SH_INNER: u64 = 0b11 << 8;   // Inner Shareable
const AP_EL1_RW: u64 = 0b00 << 6;
const AP_EL1_RO: u64 = 0b10 << 6;
const PXN: u64 = 1 << 53;          // Privileged Execute Never
const UXN: u64 = 1 << 54;          // Unprivileged Execute Never

const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

pub struct ArmPageMapper {
    root_phys: u64,
}

impl ArmPageMapper {
    pub fn new(alloc: &mut dyn PageAllocator) -> Result<Self, &'static str> {
        let root_phys = alloc.alloc_zeroed_page().ok_or("OOM allocating AArch64 L0 table")?;
        Ok(Self { root_phys })
    }

    pub const fn root_phys(&self) -> u64 {
        self.root_phys
    }

    fn encode_attrs(flags: MapFlags) -> u64 {
        let mut bits = VALID | AF | UXN;
        if flags.writable {
            bits |= AP_EL1_RW;
        } else {
            bits |= AP_EL1_RO;
        }
        if !flags.executable {
            bits |= PXN;
        }
        let (attr_idx, sh) = match flags.cache {
            CacheMode::NormalWriteBack => (0u64, SH_INNER),
            CacheMode::DeviceMmio => (1u64, 0u64),
            CacheMode::NormalUncached => (2u64, SH_INNER),
        };
        bits | (attr_idx << 2) | sh
    }

    fn ensure_next_table(
        table_phys: u64,
        index: usize,
        alloc: &mut dyn PageAllocator,
    ) -> Result<u64, &'static str> {
        let table = unsafe { &mut *(table_phys as *mut [u64; 512]) };
        let entry = table[index];
        if (entry & VALID) != 0 {
            if (entry & TABLE_OR_PAGE) == 0 {
                return Err("overlapping 2M block in AArch64 page table");
            }
            return Ok(entry & ADDR_MASK);
        }
        let next = alloc.alloc_zeroed_page().ok_or("OOM allocating AArch64 page table")?;
        table[index] = next | VALID | TABLE_OR_PAGE;
        Ok(next)
    }
}

impl BootstrapMapper for ArmPageMapper {
    fn map_4k(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str> {
        debug_assert!(!(flags.writable && flags.executable));
        let l0 = ((virt >> 39) & 0x1FF) as usize;
        let l1 = ((virt >> 30) & 0x1FF) as usize;
        let l2 = ((virt >> 21) & 0x1FF) as usize;
        let l3 = ((virt >> 12) & 0x1FF) as usize;

        let l1_tbl = Self::ensure_next_table(self.root_phys, l0, alloc)?;
        let l2_tbl = Self::ensure_next_table(l1_tbl, l1, alloc)?;
        let l3_tbl = Self::ensure_next_table(l2_tbl, l2, alloc)?;

        let l3_entries = unsafe { &mut *(l3_tbl as *mut [u64; 512]) };
        // L3 page descriptor requires bits[1:0] = 0b11 (`VALID | TABLE_OR_PAGE`).
        l3_entries[l3] = (phys & ADDR_MASK) | TABLE_OR_PAGE | Self::encode_attrs(flags);
        Ok(())
    }

    fn map_2m(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str> {
        debug_assert!(!(flags.writable && flags.executable));
        let l0 = ((virt >> 39) & 0x1FF) as usize;
        let l1 = ((virt >> 30) & 0x1FF) as usize;
        let l2 = ((virt >> 21) & 0x1FF) as usize;

        let l1_tbl = Self::ensure_next_table(self.root_phys, l0, alloc)?;
        let l2_tbl = Self::ensure_next_table(l1_tbl, l1, alloc)?;

        let l2_entries = unsafe { &mut *(l2_tbl as *mut [u64; 512]) };
        // L2 block descriptor uses bits[1:0] = 0b01 (`VALID` without `TABLE_OR_PAGE`).
        l2_entries[l2] = (phys & ADDR_MASK) | Self::encode_attrs(flags);
        Ok(())
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
    fn arm64_l3_descriptor_and_wx() {
        let mut alloc = MockAlloc::new();
        let mut mapper = ArmPageMapper::new(&mut alloc).unwrap();
        let va = 0xFFFF_FFFF_8000_0000u64;
        mapper
            .map_4k(&mut alloc, va, 0x4000_0000, MapFlags::kernel_data())
            .unwrap();

        let l0 = unsafe { &*(mapper.root_phys() as *const [u64; 512]) };
        let l1 = unsafe { &*((l0[511] & ADDR_MASK) as *const [u64; 512]) };
        let l2 = unsafe { &*((l1[510] & ADDR_MASK) as *const [u64; 512]) };
        let l3 = unsafe { &*((l2[0] & ADDR_MASK) as *const [u64; 512]) };
        let desc = l3[0];

        assert_eq!(desc & 0b11, 0b11); // Valid L3 page descriptor
        assert_ne!(desc & PXN, 0);     // Non-executable data
        assert_ne!(desc & UXN, 0);
        assert_eq!(desc & AP_EL1_RO, 0); // Writable at EL1
    }
}
