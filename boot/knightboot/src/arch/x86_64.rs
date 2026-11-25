//! x86-64 CPU state preparation and higher-half kernel handoff.

#![cfg(all(not(test), target_arch = "x86_64"))]

use core::arch::asm;
use crate::paging::BootstrapMappingLayout;

#[repr(C, align(16))]
struct BootGdt([u64; 3]);

#[repr(C, packed)]
struct DescriptorTablePtr {
    limit: u16,
    base: u64,
}

static BOOT_GDT: BootGdt = BootGdt([
    0x0000_0000_0000_0000, // 0x00: Null descriptor
    0x00AF_9A00_0000_FFFF, // 0x08: 64-bit Kernel Code (L=1, D=0, P=1, DPL=0, RX)
    0x00CF_9200_0000_FFFF, // 0x10: 64-bit Kernel Data (P=1, DPL=0, RW)
]);

pub fn mix_hardware_entropy(seed: &mut [u8; 32]) {
    let cpuid_1 = core::arch::x86_64::__cpuid(1);
    let has_rdrand = (cpuid_1.ecx & (1 << 30)) != 0;

    for chunk in seed.as_chunks_mut::<8>().0 {
        let val: u64 = if has_rdrand {
            let mut rnd = 0u64;
            let mut ok = 0u8;
            for _ in 0..10 {
                unsafe {
                    asm!(
                        "rdrand {out}",
                        "setc {ok}",
                        out = out(reg) rnd,
                        ok = out(reg_byte) ok,
                        options(nomem, nostack)
                    );
                }
                if ok != 0 {
                    break;
                }
            }
            rnd ^ unsafe { core::arch::x86_64::_rdtsc() }
        } else {
            unsafe { core::arch::x86_64::_rdtsc() }
        };
        let bytes = val.to_le_bytes();
        for (dst, src) in chunk.iter_mut().zip(bytes.iter()) {
            *dst ^= *src;
        }
    }
}

pub unsafe fn handoff_to_kernel(
    layout: &BootstrapMappingLayout,
    kernel_entry_virt: u64,
    kbip_virt: u64,
) -> ! {
    let gdtr = DescriptorTablePtr {
        limit: (core::mem::size_of::<BootGdt>() - 1) as u16,
        base: &BOOT_GDT as *const BootGdt as u64,
    };
    let idtr = DescriptorTablePtr { limit: 0, base: 0 };

    unsafe {
        asm!(
            "cli",

            // Mask legacy 8259 PIC interrupts.
            "mov al, 0xff",
            "out 0x21, al",
            "out 0xa1, al",

            // Install clean 64-bit GDT and null IDT.
            "lgdt [r8]",
            "lidt [r9]",

            // Enable PAE (5), PGE (7), OSFXSR (9), OSXMMEXCPT (10) in CR4.
            "mov rax, cr4",
            "or rax, (1 << 5) | (1 << 7) | (1 << 9) | (1 << 10)",
            "mov cr4, rax",

            // Enable EFER.SCE (0) and EFER.NXE (11).
            "mov ecx, 0xC0000080",
            "rdmsr",
            "or eax, (1 << 0) | (1 << 11)",
            "wrmsr",

            // Enable CR0.MP (1), CR0.WP (16), CR0.PG (31).
            "mov rax, cr0",
            "mov edx, 0x80010002",
            "or rax, rdx",
            "mov cr0, rax",

            // Switch page tables to bootstrap PML4.
            "mov cr3, r10",

            // Reload CS = 0x08 via far return and zero/load data segments = 0x10.
            "push 0x08",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "xor ax, ax",
            "mov fs, ax",
            "mov gs, ax",

            // Switch to higher-half kernel boot stack and jump with RDI = kbip_virt.
            "mov rsp, r11",
            "xor rbp, rbp",
            "mov rdi, r12",
            "jmp r13",

            in("r8") &gdtr,
            in("r9") &idtr,
            in("r10") layout.kernel_root_phys,
            in("r11") layout.kernel_stack_top_virt,
            in("r12") kbip_virt,
            in("r13") kernel_entry_virt,
            options(noreturn)
        );
    }
}
