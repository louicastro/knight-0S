#![cfg_attr(not(test), no_std)]
//! KnightOS AArch64 (ARM64) HAL implementation.

#[cfg(target_arch = "aarch64")]
pub mod context;
#[cfg(target_arch = "aarch64")]
pub mod dtb;
#[cfg(target_arch = "aarch64")]
pub mod gic;
#[cfg(target_arch = "aarch64")]
pub mod mmu;
#[cfg(target_arch = "aarch64")]
pub mod serial;
#[cfg(target_arch = "aarch64")]
pub mod vectors;

use knightos_hal::CpuArch;

pub struct ArmCpu;

impl CpuArch for ArmCpu {
    fn arch_name() -> &'static str {
        "aarch64"
    }

    fn current_hw_id() -> u32 {
        #[cfg(target_arch = "aarch64")]
        {
            let mpidr: u64;
            unsafe {
                core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack));
            }
            (mpidr & 0xFF_FFFF) as u32
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            0
        }
    }

    fn enable_interrupts() {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        }
    }

    fn disable_interrupts() {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("msr daifset, #2", options(nomem, nostack));
        }
    }

    fn halt_once() {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
        #[cfg(not(target_arch = "aarch64"))]
        core::hint::spin_loop();
    }
}
