//! AArch64 CPU state preparation (EL2 -> EL1 drop, MAIR/TCR/TTBR setup) and kernel handoff.

#![cfg(all(not(test), target_arch = "aarch64"))]

use core::arch::asm;
use crate::paging::BootstrapMappingLayout;

pub fn is_el2_present() -> bool {
    let current_el: u64;
    unsafe {
        asm!("mrs {el}, CurrentEL", el = out(reg) current_el, options(nomem, nostack));
    }
    ((current_el >> 2) & 0b11) == 2
}

pub fn mix_hardware_entropy(seed: &mut [u8; 32]) {
    let isar0: u64;
    unsafe {
        asm!("mrs {v}, ID_AA64ISAR0_EL1", v = out(reg) isar0, options(nomem, nostack));
    }
    let has_rndr = ((isar0 >> 60) & 0xF) != 0;

    for chunk in seed.as_chunks_mut::<8>().0 {
        let mut val: u64;
        unsafe {
            asm!("mrs {v}, CNTVCT_EL0", v = out(reg) val, options(nomem, nostack));
        }
        if has_rndr {
            let mut rnd = 0u64;
            unsafe {
                // RNDR encoding: s3_3_c2_c4_0
                asm!("mrs {v}, s3_3_c2_c4_0", v = out(reg) rnd, options(nomem, nostack));
            }
            val ^= rnd;
        }
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
    // MAIR_EL1: Attr0 = 0xFF (Normal WBWA), Attr1 = 0x00 (Device-nGnRnE), Attr2 = 0x44 (Normal NC)
    let mair: u64 = 0x0000_0000_0044_00FF;

    // Preserve inputs in fixed registers across the EL2 -> EL1 transition.
    unsafe {
        asm!(
            "msr daifset, #0xf",

            // Check if running at EL2; if so, drop cleanly to EL1h with EL1 MMU off.
            "mrs x9, CurrentEL",
            "ubfx x9, x9, #2, #2",
            "cmp x9, #2",
            "b.ne 1f",

            // HCR_EL2.RW (bit 31) = 1 -> EL1 executes in AArch64.
            "mov x9, #(1 << 31)",
            "msr hcr_el2, x9",
            // Grant EL1 access to physical counter & timer.
            "mov x9, #3",
            "msr cnthctl_el2, x9",
            "msr cntvoff_el2, xzr",
            // Clean EL1 control state before enabling MMU in EL1.
            "movz x9, #0x0800",
            "movk x9, #0x30D0, lsl #16",
            "msr sctlr_el1, x9",
            // SPSR_EL2 = 0x3C5 (DAIF masked, EL1h).
            "mov x9, #0x3c5",
            "msr spsr_el2, x9",
            "adr x9, 1f",
            "msr elr_el2, x9",
            "isb",
            "eret",

            "1:",
            // Read PARange from ID_AA64MMFR0_EL1[3:0] and clamp to 5 (48-bit PA).
            "mrs x9, id_aa64mmfr0_el1",
            "and x9, x9, #0xf",
            "cmp x9, #5",
            "b.le 2f",
            "mov x9, #5",
            "2:",

            // Build TCR_EL1: T0SZ=16, T1SZ=16, IRGN0/1=WBWA, ORGN0/1=WBWA, SH0/1=Inner, TG0=4K, TG1=4K.
            "movz x10, #0x3510",
            "movk x10, #0xb510, lsl #16",
            "bfi x10, x9, #32, #3",
            "msr tcr_el1, x10",
            "msr mair_el1, x11",
            "isb",

            // Install TTBR0 (trampoline identity map) and TTBR1 (higher-half kernel + HHDM).
            "msr ttbr0_el1, x12",
            "msr ttbr1_el1, x13",
            "dsb ish",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",

            // Enable MMU (0), C (2), SA (3), I (12) plus RES1 bits in SCTLR_EL1.
            "mrs x9, sctlr_el1",
            "orr x9, x9, #(1 << 0)",
            "orr x9, x9, #(1 << 2)",
            "orr x9, x9, #(1 << 3)",
            "orr x9, x9, #(1 << 12)",
            "msr sctlr_el1, x9",
            "isb",

            // Switch to higher-half kernel stack and jump with X0 = kbip_virt.
            "mov sp, x14",
            "mov x0, x15",
            "mov x29, xzr",
            "mov x30, xzr",
            "br x16",

            in("x11") mair,
            in("x12") layout.trampoline_root_phys,
            in("x13") layout.kernel_root_phys,
            in("x14") layout.kernel_stack_top_virt,
            in("x15") kbip_virt,
            in("x16") kernel_entry_virt,
            options(noreturn)
        );
    }
}
