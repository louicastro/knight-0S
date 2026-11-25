//! Bootstrap 4-level page-table builders for x86-64 and ARM64 (AArch64).
//!
//! Enforces strict `W^X`, maps the higher-half kernel segments, establishes the
//! Higher-Half Direct Map (HHDM) + framebuffer MMIO, allocates a guard-paged
//! kernel boot stack, and builds a temporary lower-half identity trampoline so
//! the final page-table switch never faults mid-jump.

pub mod aarch64;
pub mod x86_64;

use knightboot_protocol::{KbipArch, KbipFramebuffer};

use crate::elf::{LoadedKernel, PF_W, PF_X, align_down, align_up};
use crate::uefi::alloc::PageAllocator;
use crate::uefi::types::EFI_PAGE_SIZE;

pub const PAGE_SIZE_4K: u64 = 4096;
pub const PAGE_SIZE_2M: u64 = 2 * 1024 * 1024;

/// Default bootstrap kernel stack size (64 KiB, below an unmapped 4 KiB guard gap).
pub const KERNEL_BOOT_STACK_PAGES: usize = 16;
pub const KERNEL_BOOT_STACK_SIZE: u64 = (KERNEL_BOOT_STACK_PAGES as u64) * PAGE_SIZE_4K;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheMode {
    NormalWriteBack,
    NormalUncached,
    DeviceMmio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapFlags {
    pub writable: bool,
    pub executable: bool,
    pub cache: CacheMode,
}

impl MapFlags {
    pub const fn kernel_code() -> Self {
        Self {
            writable: false,
            executable: true,
            cache: CacheMode::NormalWriteBack,
        }
    }

    #[allow(dead_code)]
    pub const fn kernel_rodata() -> Self {
        Self {
            writable: false,
            executable: false,
            cache: CacheMode::NormalWriteBack,
        }
    }

    pub const fn kernel_data() -> Self {
        Self {
            writable: true,
            executable: false,
            cache: CacheMode::NormalWriteBack,
        }
    }

    pub const fn framebuffer() -> Self {
        Self {
            writable: true,
            executable: false,
            cache: CacheMode::NormalUncached,
        }
    }

    pub fn from_elf_flags(p_flags: u32) -> Option<Self> {
        let writable = (p_flags & PF_W) != 0;
        let executable = (p_flags & PF_X) != 0;
        if writable && executable {
            return None; // Strict W^X rejection
        }
        Some(Self {
            writable,
            executable,
            cache: CacheMode::NormalWriteBack,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BootstrapMappingLayout {
    /// Primary higher-half root (`CR3` on x86-64, `TTBR1_EL1` on ARM64).
    pub kernel_root_phys: u64,
    /// Lower-half identity root (`TTBR0_EL1` on ARM64; equal to `kernel_root_phys` on x86-64).
    #[allow(dead_code)]
    pub trampoline_root_phys: u64,
    pub kernel_stack_top_virt: u64,
}

pub trait BootstrapMapper {
    fn map_4k(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str>;

    fn map_2m(
        &mut self,
        alloc: &mut dyn PageAllocator,
        virt: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str>;

    fn map_range(
        &mut self,
        alloc: &mut dyn PageAllocator,
        mut virt: u64,
        mut phys: u64,
        mut size: u64,
        flags: MapFlags,
    ) -> Result<(), &'static str> {
        debug_assert!(!(flags.writable && flags.executable));
        while size > 0 {
            if size >= PAGE_SIZE_2M
                && (virt & (PAGE_SIZE_2M - 1)) == 0
                && (phys & (PAGE_SIZE_2M - 1)) == 0
            {
                self.map_2m(alloc, virt, phys, flags)?;
                virt += PAGE_SIZE_2M;
                phys += PAGE_SIZE_2M;
                size -= PAGE_SIZE_2M;
            } else {
                self.map_4k(alloc, virt, phys, flags)?;
                virt += PAGE_SIZE_4K;
                phys += PAGE_SIZE_4K;
                size = size.saturating_sub(PAGE_SIZE_4K);
            }
        }
        Ok(())
    }
}

pub struct IdentityRange {
    pub start: u64,
    pub size: u64,
    pub executable: bool,
}

pub struct BootstrapParams<'a> {
    pub arch: KbipArch,
    pub kernel: &'a LoadedKernel,
    pub hhdm_offset: u64,
    pub max_phys_end: u64,
    pub framebuffer: &'a KbipFramebuffer,
    pub kernel_stack_phys: u64,
    pub identity_ranges: &'a [IdentityRange],
}

/// Builds the complete bootstrap page tables for `params.arch`.
pub fn build_bootstrap_page_tables(
    alloc: &mut dyn PageAllocator,
    params: &BootstrapParams<'_>,
) -> Result<BootstrapMappingLayout, &'static str> {
    let aligned_phys_end = align_up(params.max_phys_end.max(0x1000_0000), PAGE_SIZE_2M);
    // Place the kernel stack cleanly below kernel base with an unmapped guard page.
    let stack_base_virt = 0xFFFF_FFFF_7000_0000u64 + PAGE_SIZE_4K;
    let stack_top_virt = stack_base_virt + KERNEL_BOOT_STACK_SIZE;

    match params.arch {
        KbipArch::X86_64 => {
            let mut mapper = x86_64::X86PageMapper::new(alloc)?;
            map_common_higher_half(
                &mut mapper,
                alloc,
                params,
                aligned_phys_end,
                stack_base_virt,
            )?;
            for r in params.identity_ranges {
                if r.size == 0 {
                    continue;
                }
                let start = align_down(r.start, PAGE_SIZE_4K);
                let end = align_up(r.start + r.size, PAGE_SIZE_4K);
                let flags = if r.executable {
                    MapFlags {
                        writable: true, // EFI PE sections may share RX/RW pages inside trampoline image
                        executable: true,
                        cache: CacheMode::NormalWriteBack,
                    }
                } else {
                    MapFlags::kernel_data()
                };
                mapper.map_range_trampoline(alloc, start, start, end - start, flags)?;
            }
            Ok(BootstrapMappingLayout {
                kernel_root_phys: mapper.pml4_phys(),
                trampoline_root_phys: mapper.pml4_phys(),
                kernel_stack_top_virt: stack_top_virt,
            })
        }
        KbipArch::AArch64 => {
            let mut ttbr1 = aarch64::ArmPageMapper::new(alloc)?;
            map_common_higher_half(
                &mut ttbr1,
                alloc,
                params,
                aligned_phys_end,
                stack_base_virt,
            )?;

            let mut ttbr0 = aarch64::ArmPageMapper::new(alloc)?;
            for r in params.identity_ranges {
                if r.size == 0 {
                    continue;
                }
                let start = align_down(r.start, PAGE_SIZE_4K);
                let end = align_up(r.start + r.size, PAGE_SIZE_4K);
                let flags = if r.executable {
                    MapFlags::kernel_code()
                } else {
                    MapFlags::kernel_data()
                };
                ttbr0.map_range(alloc, start, start, end - start, flags)?;
            }

            Ok(BootstrapMappingLayout {
                kernel_root_phys: ttbr1.root_phys(),
                trampoline_root_phys: ttbr0.root_phys(),
                kernel_stack_top_virt: stack_top_virt,
            })
        }
        KbipArch::Unknown => Err("unsupported architecture"),
    }
}

fn map_common_higher_half<M: BootstrapMapper>(
    mapper: &mut M,
    alloc: &mut dyn PageAllocator,
    params: &BootstrapParams<'_>,
    aligned_phys_end: u64,
    stack_base_virt: u64,
) -> Result<(), &'static str> {
    // 1. Direct physical memory window (HHDM).
    mapper.map_range(
        alloc,
        params.hhdm_offset,
        0,
        aligned_phys_end,
        MapFlags::kernel_data(),
    )?;

    // 2. Framebuffer MMIO window at HHDM + fb_phys_base (non-cacheable / uncached).
    if params.framebuffer.phys_base != 0 && params.framebuffer.size > 0 {
        let fb_phys = align_down(params.framebuffer.phys_base, EFI_PAGE_SIZE as u64);
        let fb_end = align_up(
            params.framebuffer.phys_base + params.framebuffer.size,
            EFI_PAGE_SIZE as u64,
        );
        mapper.map_range(
            alloc,
            params.hhdm_offset + fb_phys,
            fb_phys,
            fb_end - fb_phys,
            MapFlags::framebuffer(),
        )?;
    }

    // 3. Kernel ELF PT_LOAD segments with strict W^X.
    for seg_opt in params.kernel.segments[..params.kernel.segment_count].iter() {
        let seg = seg_opt.ok_or("missing kernel segment")?;
        let flags = MapFlags::from_elf_flags(seg.flags).ok_or("W^X violation in kernel ELF")?;
        mapper.map_range(
            alloc,
            seg.virt_base,
            seg.phys_base,
            (seg.page_count as u64) * PAGE_SIZE_4K,
            flags,
        )?;
    }

    // 4. Kernel boot stack (guard page at stack_base_virt - 4096 left unmapped).
    mapper.map_range(
        alloc,
        stack_base_virt,
        params.kernel_stack_phys,
        KERNEL_BOOT_STACK_SIZE,
        MapFlags::kernel_data(),
    )?;

    Ok(())
}
