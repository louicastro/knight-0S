//! x86-64 4-level paging (`MmuArch` implementation).

use core::arch::asm;
use knightos_hal::{CachePolicy, MmuArch, PageFlags, PhysAddr, PhysFrameAllocator, VirtAddr};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const NO_EXEC: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

pub struct X86Mmu;

fn encode_flags(flags: PageFlags) -> Result<u64, &'static str> {
    if !flags.is_wx_valid() {
        return Err("W^X violation in x86_64 MmuArch::map_4k");
    }
    let mut bits = PRESENT;
    if flags.writable {
        bits |= WRITABLE;
    }
    if !flags.executable {
        bits |= NO_EXEC;
    }
    if flags.user {
        bits |= USER;
    }
    if flags.global {
        bits |= GLOBAL;
    }
    match flags.cache {
        CachePolicy::WriteBack => {}
        CachePolicy::Uncached => bits |= PWT,
        CachePolicy::DeviceMmio => bits |= PWT | PCD,
    }
    Ok(bits)
}

unsafe fn table_mut(phys: u64, hhdm_offset: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *((phys + hhdm_offset) as *mut [u64; 512]) }
}

fn ensure_next_table(
    table_phys: u64,
    index: usize,
    hhdm_offset: u64,
    user: bool,
    is_pd_level: bool,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<u64, &'static str> {
    let tbl = unsafe { table_mut(table_phys, hhdm_offset) };
    let entry = tbl[index];
    if (entry & PRESENT) != 0 {
        if (entry & HUGE) != 0 {
            if !is_pd_level {
                return Err("cannot descend through 1G huge page");
            }
            // Split 2 MiB huge page into 512 x 4 KiB entries preserving attributes.
            let pt_phys = alloc
                .alloc_frame_zeroed(hhdm_offset)
                .ok_or("OOM splitting 2M huge page")?;
            let base_phys = entry & 0x000F_FFFF_FFE0_0000;
            let leaf_attrs = entry & !ADDR_MASK & !HUGE;
            let pt_tbl = unsafe { table_mut(pt_phys.as_u64(), hhdm_offset) };
            for i in 0..512usize {
                pt_tbl[i] = (base_phys + (i as u64) * 4096) | leaf_attrs;
            }
            let mut dir_flags = PRESENT | WRITABLE;
            if user || (entry & USER) != 0 {
                dir_flags |= USER;
            }
            tbl[index] = pt_phys.as_u64() | dir_flags;
            return Ok(pt_phys.as_u64());
        }
        if user && (entry & USER) == 0 {
            tbl[index] = entry | USER;
        }
        return Ok(entry & ADDR_MASK);
    }
    let next = alloc
        .alloc_frame_zeroed(hhdm_offset)
        .ok_or("OOM allocating x86_64 page table")?;
    let mut flags = PRESENT | WRITABLE;
    if user {
        flags |= USER;
    }
    tbl[index] = next.as_u64() | flags;
    Ok(next.as_u64())
}

impl MmuArch for X86Mmu {
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
        let l4 = ((va >> 39) & 0x1FF) as usize;
        let l3 = ((va >> 30) & 0x1FF) as usize;
        let l2 = ((va >> 21) & 0x1FF) as usize;
        let l1 = ((va >> 12) & 0x1FF) as usize;

        let pdpt = ensure_next_table(root.as_u64(), l4, hhdm_offset, flags.user, false, alloc)?;
        let pd = ensure_next_table(pdpt, l3, hhdm_offset, flags.user, false, alloc)?;
        let pt = ensure_next_table(pd, l2, hhdm_offset, flags.user, true, alloc)?;

        let pt_tbl = unsafe { table_mut(pt, hhdm_offset) };
        pt_tbl[l1] = (phys.as_u64() & ADDR_MASK) | leaf_bits;
        Self::flush_tlb_page(virt);
        Ok(())
    }

    fn unmap_4k(root: PhysAddr, hhdm_offset: u64, virt: VirtAddr) -> Option<PhysAddr> {
        let va = virt.as_u64();
        let l4 = ((va >> 39) & 0x1FF) as usize;
        let l3 = ((va >> 30) & 0x1FF) as usize;
        let l2 = ((va >> 21) & 0x1FF) as usize;
        let l1 = ((va >> 12) & 0x1FF) as usize;

        let pml4 = unsafe { table_mut(root.as_u64(), hhdm_offset) };
        if (pml4[l4] & PRESENT) == 0 {
            return None;
        }
        let pdpt = unsafe { table_mut(pml4[l4] & ADDR_MASK, hhdm_offset) };
        if (pdpt[l3] & PRESENT) == 0 || (pdpt[l3] & HUGE) != 0 {
            return None;
        }
        let pd = unsafe { table_mut(pdpt[l3] & ADDR_MASK, hhdm_offset) };
        if (pd[l2] & PRESENT) == 0 || (pd[l2] & HUGE) != 0 {
            return None;
        }
        let pt = unsafe { table_mut(pd[l2] & ADDR_MASK, hhdm_offset) };
        let old = pt[l1];
        if (old & PRESENT) == 0 {
            return None;
        }
        pt[l1] = 0;
        Self::flush_tlb_page(virt);
        Some(PhysAddr(old & ADDR_MASK))
    }

    fn clear_lower_half(hhdm_offset: u64) {
        let root = Self::active_kernel_root();
        let pml4 = unsafe { table_mut(root.as_u64(), hhdm_offset) };
        for slot in &mut pml4[0..256] {
            *slot = 0;
        }
        Self::flush_tlb_all();
    }

    fn flush_tlb_page(virt: VirtAddr) {
        unsafe {
            asm!("invlpg [{}]", in(reg) virt.as_u64(), options(nostack, preserves_flags));
        }
    }

    fn flush_tlb_all() {
        unsafe {
            let cr3: u64;
            asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
            asm!("mov cr3, {}", in(reg) cr3, options(nostack));
        }
    }

    fn active_kernel_root() -> PhysAddr {
        let cr3: u64;
        unsafe {
            asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
        }
        PhysAddr(cr3 & ADDR_MASK)
    }

    fn new_user_root(
        hhdm_offset: u64,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<PhysAddr, &'static str> {
        let new_pml4 = alloc
            .alloc_frame_zeroed(hhdm_offset)
            .ok_or("OOM allocating user PML4")?;
        let kernel_root = Self::active_kernel_root();
        unsafe {
            let src = &*((kernel_root.as_u64() + hhdm_offset) as *const [u64; 512]);
            let dst = &mut *((new_pml4.as_u64() + hhdm_offset) as *mut [u64; 512]);
            // Share higher-half kernel & HHDM mappings; lower half [0..256] remains clean for user Vmar.
            dst[256..512].copy_from_slice(&src[256..512]);
        }
        Ok(new_pml4)
    }

    fn switch_user_root(root: PhysAddr) {
        let cur = Self::active_kernel_root();
        if cur != root {
            unsafe {
                asm!("mov cr3, {}", in(reg) root.as_u64(), options(nostack));
            }
        }
    }
}
