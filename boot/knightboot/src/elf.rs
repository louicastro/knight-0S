//! Pure `#![no_std]` ELF64 parser, validator, and W^X segment loader.

use core::mem::size_of;

use knightboot_protocol::KbipArch;

use crate::uefi::alloc::allocate_zeroed_pages;
use crate::uefi::types::{EFI_PAGE_SIZE, Status};

pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;
pub const PT_LOAD: u32 = 1;

pub const PF_X: u32 = 1 << 0;
pub const PF_W: u32 = 1 << 1;
#[allow(dead_code)]
pub const PF_R: u32 = 1 << 2;

pub const MAX_LOAD_SEGMENTS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Elf64Header {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    NotElf64LittleEndian,
    UnsupportedType(u16),
    WrongArchitecture(u16),
    BadProgramHeaderTable,
    TooManyLoadSegments,
    SegmentOutOfBounds,
    InvalidSegmentSize,
    WxViolation,
    LowerHalfKernelSegment,
    OverlappingSegments,
    NoLoadSegments,
    EntryOutsideLoadSegments,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelSegment {
    pub phys_base: u64,
    pub virt_base: u64,
    pub mem_size: u64,
    pub page_count: usize,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct LoadedKernel {
    pub phys_base: u64,
    pub virt_base: u64,
    pub total_size: u64,
    pub entry_virt: u64,
    pub kaslr_slide: u64,
    pub segments: [Option<KernelSegment>; MAX_LOAD_SEGMENTS],
    pub segment_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedSegment {
    pub file_offset: usize,
    pub file_size: usize,
    pub virt_addr: u64,
    pub mem_size: u64,
    pub flags: u32,
}

pub struct ValidatedElf<'a> {
    pub raw: &'a [u8],
    pub header: Elf64Header,
    pub segments: [Option<ValidatedSegment>; MAX_LOAD_SEGMENTS],
    pub segment_count: usize,
    pub is_pie: bool,
}

pub const fn expected_machine(arch: KbipArch) -> u16 {
    match arch {
        KbipArch::X86_64 => EM_X86_64,
        KbipArch::AArch64 => EM_AARCH64,
        KbipArch::Unknown => 0,
    }
}

/// Validates an ELF64 kernel image in memory without allocating physical pages.
pub fn validate_kernel_elf(bytes: &[u8], arch: KbipArch) -> Result<ValidatedElf<'_>, ElfError> {
    if bytes.len() < size_of::<Elf64Header>() {
        return Err(ElfError::TooSmall);
    }

    // SAFETY: Unaligned read avoids UB on arbitrary UEFI pool buffer alignments.
    let hdr = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const Elf64Header) };

    if hdr.e_ident[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    if hdr.e_ident[4] != ELFCLASS64 || hdr.e_ident[5] != ELFDATA2LSB {
        return Err(ElfError::NotElf64LittleEndian);
    }
    if hdr.e_type != ET_EXEC && hdr.e_type != ET_DYN {
        return Err(ElfError::UnsupportedType(hdr.e_type));
    }
    if hdr.e_machine != expected_machine(arch) {
        return Err(ElfError::WrongArchitecture(hdr.e_machine));
    }
    if (hdr.e_phentsize as usize) < size_of::<Elf64Phdr>() || hdr.e_phnum == 0 {
        return Err(ElfError::BadProgramHeaderTable);
    }

    let ph_table_size = (hdr.e_phentsize as u64)
        .checked_mul(hdr.e_phnum as u64)
        .ok_or(ElfError::BadProgramHeaderTable)?;
    let ph_end = hdr
        .e_phoff
        .checked_add(ph_table_size)
        .ok_or(ElfError::BadProgramHeaderTable)?;
    if ph_end as usize > bytes.len() {
        return Err(ElfError::BadProgramHeaderTable);
    }

    let is_pie = hdr.e_type == ET_DYN;
    let mut segments: [Option<ValidatedSegment>; MAX_LOAD_SEGMENTS] = [None; MAX_LOAD_SEGMENTS];
    let mut count = 0usize;
    let mut entry_covered = false;

    for i in 0..hdr.e_phnum as usize {
        let off = hdr.e_phoff as usize + i * (hdr.e_phentsize as usize);
        let ph = unsafe { core::ptr::read_unaligned(bytes[off..].as_ptr() as *const Elf64Phdr) };
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        if count >= MAX_LOAD_SEGMENTS {
            return Err(ElfError::TooManyLoadSegments);
        }
        if ph.p_filesz > ph.p_memsz {
            return Err(ElfError::InvalidSegmentSize);
        }
        // Strict W^X enforcement at ELF load time.
        if (ph.p_flags & (PF_W | PF_X)) == (PF_W | PF_X) {
            return Err(ElfError::WxViolation);
        }
        let file_end = ph
            .p_offset
            .checked_add(ph.p_filesz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if file_end as usize > bytes.len() {
            return Err(ElfError::SegmentOutOfBounds);
        }

        // For static higher-half ET_EXEC kernels, require upper canonical addresses.
        if !is_pie && ph.p_vaddr < 0xFFFF_8000_0000_0000 {
            return Err(ElfError::LowerHalfKernelSegment);
        }

        let seg_start = align_down(ph.p_vaddr, EFI_PAGE_SIZE as u64);
        let seg_end = align_up(
            ph.p_vaddr.checked_add(ph.p_memsz).ok_or(ElfError::InvalidSegmentSize)?,
            EFI_PAGE_SIZE as u64,
        );

        for existing in segments[..count].iter().flatten() {
            let ex_start = align_down(existing.virt_addr, EFI_PAGE_SIZE as u64);
            let ex_end = align_up(existing.virt_addr + existing.mem_size, EFI_PAGE_SIZE as u64);
            if seg_start < ex_end && ex_start < seg_end {
                return Err(ElfError::OverlappingSegments);
            }
        }

        if hdr.e_entry >= ph.p_vaddr && hdr.e_entry < ph.p_vaddr + ph.p_memsz {
            entry_covered = true;
        }

        segments[count] = Some(ValidatedSegment {
            file_offset: ph.p_offset as usize,
            file_size: ph.p_filesz as usize,
            virt_addr: ph.p_vaddr,
            mem_size: ph.p_memsz,
            flags: ph.p_flags,
        });
        count += 1;
    }

    if count == 0 {
        return Err(ElfError::NoLoadSegments);
    }
    if !entry_covered {
        return Err(ElfError::EntryOutsideLoadSegments);
    }

    Ok(ValidatedElf {
        raw: bytes,
        header: hdr,
        segments,
        segment_count: count,
        is_pie,
    })
}

/// Allocates UEFI `EfiLoaderData` physical pages, copies `PT_LOAD` segments, zeroes `.bss`,
/// and returns virtual/physical bounds for bootstrap page-table mapping.
pub fn load_kernel_elf(
    validated: &ValidatedElf<'_>,
    base_virt_override: u64,
    kaslr_slide: u64,
) -> Result<LoadedKernel, Status> {
    let slide = if validated.is_pie {
        let aligned_slide = align_down(kaslr_slide, 0x20_0000); // 2 MiB alignment
        base_virt_override.wrapping_add(aligned_slide)
    } else {
        0
    };

    let mut min_virt = u64::MAX;
    let mut max_virt = 0u64;
    let mut min_phys = u64::MAX;
    let mut loaded_segs = [None; MAX_LOAD_SEGMENTS];

    for (idx, seg_opt) in validated.segments[..validated.segment_count].iter().enumerate() {
        let seg = seg_opt.ok_or(Status::LOAD_ERROR)?;
        let target_vaddr = seg.virt_addr.wrapping_add(slide);
        let page_vstart = align_down(target_vaddr, EFI_PAGE_SIZE as u64);
        let page_offset = (target_vaddr - page_vstart) as usize;
        let total_span = page_offset + (seg.mem_size as usize);
        let page_count = total_span.div_ceil(EFI_PAGE_SIZE);

        let phys = allocate_zeroed_pages(page_count)?;
        // SAFETY: `phys` points to `page_count * 4096` zeroed bytes owned exclusively by us.
        unsafe {
            let dst = (phys as *mut u8).add(page_offset);
            let src = &validated.raw[seg.file_offset..seg.file_offset + seg.file_size];
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst, seg.file_size);
        }

        let mapped_end = page_vstart + (page_count as u64) * (EFI_PAGE_SIZE as u64);
        min_virt = min_virt.min(page_vstart);
        max_virt = max_virt.max(mapped_end);
        min_phys = min_phys.min(phys);

        loaded_segs[idx] = Some(KernelSegment {
            phys_base: phys,
            virt_base: page_vstart,
            mem_size: (page_count as u64) * (EFI_PAGE_SIZE as u64),
            page_count,
            flags: seg.flags,
        });
    }

    Ok(LoadedKernel {
        phys_base: min_phys,
        virt_base: min_virt,
        total_size: max_virt.saturating_sub(min_virt),
        entry_virt: validated.header.e_entry.wrapping_add(slide),
        kaslr_slide: slide,
        segments: loaded_segs,
        segment_count: validated.segment_count,
    })
}

pub const fn align_down(val: u64, align: u64) -> u64 {
    val & !(align - 1)
}

pub const fn align_up(val: u64, align: u64) -> u64 {
    (val + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_minimal_elf(machine: u16, seg_flags: u32, vaddr: u64) -> [u8; 256] {
        let mut buf = [0u8; 256];
        let hdr = Elf64Header {
            e_ident: [0x7F, b'E', b'L', b'F', ELFCLASS64, ELFDATA2LSB, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            e_type: ET_EXEC,
            e_machine: machine,
            e_version: 1,
            e_entry: vaddr,
            e_phoff: 64,
            e_shoff: 0,
            e_flags: 0,
            e_ehsize: 64,
            e_phentsize: 56,
            e_phnum: 1,
            e_shentsize: 0,
            e_shnum: 0,
            e_shstrndx: 0,
        };
        let ph = Elf64Phdr {
            p_type: PT_LOAD,
            p_flags: seg_flags,
            p_offset: 128,
            p_vaddr: vaddr,
            p_paddr: vaddr,
            p_filesz: 16,
            p_memsz: 32,
            p_align: 4096,
        };
        unsafe {
            core::ptr::write_unaligned(buf.as_mut_ptr() as *mut Elf64Header, hdr);
            core::ptr::write_unaligned(buf[64..].as_mut_ptr() as *mut Elf64Phdr, ph);
        }
        buf
    }

    #[test]
    fn elf_validates_valid_x86_64_and_aarch64() {
        let x86 = make_minimal_elf(EM_X86_64, PF_R | PF_X, 0xFFFF_FFFF_8000_0000);
        let res = validate_kernel_elf(&x86, KbipArch::X86_64).expect("valid x86_64 elf");
        assert_eq!(res.segment_count, 1);

        let arm = make_minimal_elf(EM_AARCH64, PF_R | PF_X, 0xFFFF_FFFF_8000_0000);
        let res = validate_kernel_elf(&arm, KbipArch::AArch64).expect("valid aarch64 elf");
        assert_eq!(res.segment_count, 1);
    }

    #[test]
    fn elf_rejects_wx_and_wrong_arch() {
        let wx = make_minimal_elf(EM_X86_64, PF_R | PF_W | PF_X, 0xFFFF_FFFF_8000_0000);
        assert_eq!(
            validate_kernel_elf(&wx, KbipArch::X86_64).err(),
            Some(ElfError::WxViolation)
        );

        let arm = make_minimal_elf(EM_AARCH64, PF_R | PF_X, 0xFFFF_FFFF_8000_0000);
        assert_eq!(
            validate_kernel_elf(&arm, KbipArch::X86_64).err(),
            Some(ElfError::WrongArchitecture(EM_AARCH64))
        );
    }
}
