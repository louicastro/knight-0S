#![cfg_attr(not(test), no_std)]
//! KnightOS x86-64 HAL implementation.

#[cfg(target_arch = "x86_64")]
pub mod acpi;
#[cfg(target_arch = "x86_64")]
pub mod apic;
#[cfg(target_arch = "x86_64")]
pub mod context;
#[cfg(target_arch = "x86_64")]
pub mod gdt_idt;
#[cfg(target_arch = "x86_64")]
pub mod mmu;
#[cfg(target_arch = "x86_64")]
pub mod serial;

use knightos_hal::CpuArch;

pub struct X86Cpu;

impl CpuArch for X86Cpu {
    fn arch_name() -> &'static str {
        "x86_64"
    }

    fn current_hw_id() -> u32 {
        #[cfg(target_arch = "x86_64")]
        {
            let res = core::arch::x86_64::__cpuid(1);
            (res.ebx >> 24) & 0xFF
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            0
        }
    }

    fn enable_interrupts() {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }

    fn disable_interrupts() {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("cli", options(nomem, nostack));
        }
    }

    fn halt_once() {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
        #[cfg(not(target_arch = "x86_64"))]
        core::hint::spin_loop();
    }
}
