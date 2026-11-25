//! AArch64 4-level stage-1 paging (`MmuArch` implementation).

use core::arch::asm;
use knightos_hal::{CachePolicy, MmuArch, PageFlags, PhysAddr, PhysFrameAllocator, VirtAddr};

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
const AF: u64 = 1 << 10;
const SH_INNER: u64 = 0b11 << 8;
const AP_EL1_RW: u64 = 0b00 << 6;
const AP_EL1_RO: u64 = 0b10 << 6;
const AP_EL0_RW: u64 = 0b01 << 6;
const AP_EL0_RO: u64 = 0b11 << 6;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

pub struct ArmMmu;

fn encode_flags(flags: PageFlags) -> Result<u64, &'static str> {
    if !flags.is_wx_valid() {
        return Err("W^X violation in AArch64 MmuArch::map_4k");
    }
    let mut bits = VALID | TABLE_OR_PAGE | AF;
    bits |= match (flags.user, flags.writable) {
        (false, true) => AP_EL1_RW,
        (false, false) => AP_EL1_RO,
        (true, true) => AP_EL0_RW,
        (true, false) => AP_EL0_RO,
    };
    if !flags.executable || flags.user {
        bits |= PXN;
    }
    if !flags.executable || !flags.user {
        bits |= UXN;
    }
    let (attr_idx, sh) = match flags.cache {
        CachePolicy::WriteBack => (0u64, SH_INNER),
        CachePolicy::DeviceMmio => (1u64, 0u64),
        CachePolicy::Uncached => (2u64, SH_INNER),
    };
    Ok(bits | (attr_idx << 2) | sh)
}

unsafe fn table_mut(phys: u64, hhdm_offset: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *((phys + hhdm_offset) as *mut [u64; 512]) }
}

fn ensure_next_table(
    table_phys: u64,
    index: usize,
    hhdm_offset: u64,
    is_l2_level: bool,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<u64, &'static str> {
    let tbl = unsafe { table_mut(table_phys, hhdm_offset) };
    let entry = tbl[index];
    if (entry & VALID) != 0 {
        if (entry & TABLE_OR_PAGE) == 0 {
            if !is_l2_level {
                return Err("cannot descend through 1G block descriptor");
            }
            // Split 2 MiB L2 block descriptor into 512 x 4 KiB L3 page descriptors.
            let l3_phys = alloc
                .alloc_frame_zeroed(hhdm_offset)
                .ok_or("OOM splitting 2M AArch64 block")?;
            let base_phys = entry & 0x0000_FFFF_FFE0_0000;
            let leaf_attrs = (entry & !ADDR_MASK) | TABLE_OR_PAGE;
            let l3_tbl = unsafe { table_mut(l3_phys.as_u64(), hhdm_offset) };
            for i in 0..512usize {
                l3_tbl[i] = (base_phys + (i as u64) * 4096) | leaf_attrs;
            }
            tbl[index] = l3_phys.as_u64() | VALID | TABLE_OR_PAGE;
            return Ok(l3_phys.as_u64());
        }
        return Ok(entry & ADDR_MASK);
    }
    let next = alloc
        .alloc_frame_zeroed(hhdm_offset)
        .ok_or("OOM allocating AArch64 page table")?;
    tbl[index] = next.as_u64() | VALID | TABLE_OR_PAGE;
    Ok(next.as_u64())
}

impl MmuArch for ArmMmu {
    fn map_4k(
        root: PhysAddr,
        hhdm_offset: u64,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<(), &'static str> {
        let leaf_bits = encode_flags(flags)?;
        let va = virt.as_u64();
        let l0 = ((va >> 39) & 0x1FF) as usize;
        let l1 = ((va >> 30) & 0x1FF) as usize;
        let l2 = ((va >> 21) & 0x1FF) as usize;
        let l3 = ((va >> 12) & 0x1FF) as usize;

        let l1_tbl = ensure_next_table(root.as_u64(), l0, hhdm_offset, false, alloc)?;
        let l2_tbl = ensure_next_table(l1_tbl, l1, hhdm_offset, false, alloc)?;
        let l3_tbl = ensure_next_table(l2_tbl, l2, hhdm_offset, true, alloc)?;

        let l3_entries = unsafe { table_mut(l3_tbl, hhdm_offset) };
        l3_entries[l3] = (phys.as_u64() & ADDR_MASK) | leaf_bits;
        Self::flush_tlb_page(virt);
        Ok(())
    }

    fn unmap_4k(root: PhysAddr, hhdm_offset: u64, virt: VirtAddr) -> Option<PhysAddr> {
        let va = virt.as_u64();
        let l0 = ((va >> 39) & 0x1FF) as usize;
        let l1 = ((va >> 30) & 0x1FF) as usize;
        let l2 = ((va >> 21) & 0x1FF) as usize;
        let l3 = ((va >> 12) & 0x1FF) as usize;

        let l0_tbl = unsafe { table_mut(root.as_u64(), hhdm_offset) };
        if (l0_tbl[l0] & VALID) == 0 {
            return None;
        }
        let l1_tbl = unsafe { table_mut(l0_tbl[l0] & ADDR_MASK, hhdm_offset) };
        if (l1_tbl[l1] & VALID) == 0 || (l1_tbl[l1] & TABLE_OR_PAGE) == 0 {
            return None;
        }
        let l2_tbl = unsafe { table_mut(l1_tbl[l1] & ADDR_MASK, hhdm_offset) };
        if (l2_tbl[l2] & VALID) == 0 || (l2_tbl[l2] & TABLE_OR_PAGE) == 0 {
            return None;
        }
        let l3_tbl = unsafe { table_mut(l2_tbl[l2] & ADDR_MASK, hhdm_offset) };
        let old = l3_tbl[l3];
        if (old & VALID) == 0 {
            return None;
        }
        l3_tbl[l3] = 0;
        Self::flush_tlb_page(virt);
        Some(PhysAddr(old & ADDR_MASK))
    }

    fn clear_lower_half(_hhdm_offset: u64) {
        unsafe {
            asm!(
                "msr ttbr0_el1, xzr",
                "dsb ish",
                "isb",
                "tlbi vmalle1",
                "dsb ish",
                "isb",
                options(nostack)
            );
        }
    }

    fn flush_tlb_page(virt: VirtAddr) {
        let page = (virt.as_u64() >> 12) & 0x0000_0FFF_FFFF_FFFF;
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vaae1is, {p}",
                "dsb ish",
                "isb",
                p = in(reg) page,
                options(nostack)
            );
        }
    }

    fn flush_tlb_all() {
        unsafe {
            asm!(
                "dsb ish",
                "tlbi vmalle1is",
                "dsb ish",
                "isb",
                options(nostack)
            );
        }
    }

    fn active_kernel_root() -> PhysAddr {
        let ttbr1: u64;
        unsafe {
            asm!("mrs {}, ttbr1_el1", out(reg) ttbr1, options(nomem, nostack));
        }
        PhysAddr(ttbr1 & ADDR_MASK)
    }

    fn new_user_root(
        hhdm_offset: u64,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<PhysAddr, &'static str> {
        alloc
            .alloc_frame_zeroed(hhdm_offset)
            .ok_or("OOM allocating user TTBR0 L0 table")
    }

    fn switch_user_root(root: PhysAddr) {
        let cur: u64;
        unsafe {
            asm!("mrs {}, ttbr0_el1", out(reg) cur, options(nomem, nostack));
        }
        if (cur & ADDR_MASK) != root.as_u64() {
            unsafe {
                asm!(
                    "msr ttbr0_el1, {r}",
                    "dsb ish",
                    "isb",
                    "tlbi vmalle1",
                    "dsb ish",
                    "isb",
                    r = in(reg) root.as_u64(),
                    options(nostack)
                );
            }
        }
    }
}
