//! User-space ELF64 loader mapping `PT_LOAD` segments and stack into a task's [`Vmar`].

use core::mem::size_of;

use knightos_hal::{MmuArch, PAGE_SIZE_4K, PhysFrameAllocator};
use knightos_mm::pmm::BitmapPmm;
use knightos_mm::vmar::Vmar;
use knightos_mm::vmo::Vmo;

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1 << 0;
const PF_W: u32 = 1 << 1;

#[cfg(target_arch = "x86_64")]
const EXPECTED_MACHINE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const EXPECTED_MACHINE: u16 = 183;

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Header {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

pub struct LoadedUserElf {
    pub entry_va: u64,
    pub stack_top_va: u64,
}

pub fn load_user_elf<M: MmuArch>(
    elf_bytes: &[u8],
    vmar: &mut Vmar,
    pmm: &'static BitmapPmm,
    hhdm_offset: u64,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<LoadedUserElf, &'static str> {
    if elf_bytes.len() < size_of::<Elf64Header>() {
        return Err("user ELF too small");
    }
    let hdr = unsafe { core::ptr::read_unaligned(elf_bytes.as_ptr() as *const Elf64Header) };
    if hdr.e_ident[0..4] != ELF_MAGIC || hdr.e_ident[4] != ELFCLASS64 || hdr.e_ident[5] != ELFDATA2LSB {
        return Err("invalid user ELF64 header");
    }
    if hdr.e_machine != EXPECTED_MACHINE {
        return Err("wrong user ELF machine");
    }
    if (hdr.e_phentsize as usize) < size_of::<Elf64Phdr>() || hdr.e_phnum == 0 {
        return Err("invalid user ELF program headers");
    }

    for i in 0..hdr.e_phnum as usize {
        let off = hdr.e_phoff as usize + i * (hdr.e_phentsize as usize);
        if off + size_of::<Elf64Phdr>() > elf_bytes.len() {
            return Err("user ELF phdr out of bounds");
        }
        let ph = unsafe { core::ptr::read_unaligned(elf_bytes[off..].as_ptr() as *const Elf64Phdr) };
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        if ph.p_filesz > ph.p_memsz {
            return Err("user ELF filesz > memsz");
        }
        let file_end = ph.p_offset.checked_add(ph.p_filesz).ok_or("overflow")? as usize;
        if file_end > elf_bytes.len() {
            return Err("user ELF segment out of file bounds");
        }

        let writable = (ph.p_flags & PF_W) != 0;
        let executable = (ph.p_flags & PF_X) != 0;
        let flags = Vmar::user_page_flags(writable, executable)?;

        let page_vstart = ph.p_vaddr & !(PAGE_SIZE_4K as u64 - 1);
        let page_off = (ph.p_vaddr - page_vstart) as usize;
        let total_span = page_off + (ph.p_memsz as usize);
        let aligned_len = (total_span + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);

        let vmo = Vmo::create_anonymous(aligned_len, pmm, hhdm_offset)
            .ok_or("OOM creating user ELF segment VMO")?;
        if ph.p_filesz > 0 {
            vmo.write(page_off, &elf_bytes[ph.p_offset as usize..file_end])
                .map_err(|_| "VMO write failed")?;
        }
        vmar.map_vmo_at::<M>(page_vstart, vmo, 0, aligned_len, flags, alloc)?;
    }

    // Allocate 64 KiB user stack with guard page via `map_vmo_auto`.
    let stack_size = 64 * 1024;
    let stack_vmo = Vmo::create_anonymous(stack_size, pmm, hhdm_offset)
        .ok_or("OOM creating user stack VMO")?;
    let stack_flags = Vmar::user_page_flags(true, false)?;
    let stack_base = vmar.map_vmo_auto::<M>(stack_vmo, 0, stack_size, stack_flags, alloc)?;

    Ok(LoadedUserElf {
        entry_va: hdr.e_entry,
        stack_top_va: stack_base + stack_size as u64,
    })
}
