//! Architecture-specific CPU feature probing, entropy fallback, and kernel handoff.

use knightboot_protocol::KbipArch;
use crate::paging::BootstrapMappingLayout;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "aarch64")]
mod aarch64;

pub const fn current_arch() -> KbipArch {
    #[cfg(target_arch = "x86_64")]
    {
        KbipArch::X86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        KbipArch::AArch64
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        KbipArch::Unknown
    }
}

pub fn is_el2_present() -> bool {
    #[cfg(all(not(test), target_arch = "aarch64"))]
    {
        aarch64::is_el2_present()
    }
    #[cfg(not(all(not(test), target_arch = "aarch64")))]
    {
        false
    }
}

pub fn mix_hardware_entropy(seed: &mut [u8; 32]) {
    #[cfg(all(not(test), target_arch = "x86_64"))]
    x86_64::mix_hardware_entropy(seed);

    #[cfg(all(not(test), target_arch = "aarch64"))]
    aarch64::mix_hardware_entropy(seed);

    #[cfg(not(any(
        all(not(test), target_arch = "x86_64"),
        all(not(test), target_arch = "aarch64")
    )))]
    let _ = seed;
}

/// Transfers control permanently to the KnightOS kernel entry point (`_start(kbip_virt)`).
///
/// # Safety
/// Must only be called after `ExitBootServices` has succeeded, `layout` contains valid
/// bootstrap page tables mapping the kernel, HHDM, stack, and lower-half trampoline, and
/// `kbip_virt` points to a finalized `KbipBootInfo` structure.
#[allow(unused_variables)]
pub unsafe fn handoff_to_kernel(
    layout: &BootstrapMappingLayout,
    kernel_entry_virt: u64,
    kbip_virt: u64,
) -> ! {
    #[cfg(all(not(test), target_arch = "x86_64"))]
    unsafe {
        x86_64::handoff_to_kernel(layout, kernel_entry_virt, kbip_virt)
    }

    #[cfg(all(not(test), target_arch = "aarch64"))]
    unsafe {
        aarch64::handoff_to_kernel(layout, kernel_entry_virt, kbip_virt)
    }

    #[cfg(not(any(
        all(not(test), target_arch = "x86_64"),
        all(not(test), target_arch = "aarch64")
    )))]
    loop {
        core::hint::spin_loop();
    }
}
