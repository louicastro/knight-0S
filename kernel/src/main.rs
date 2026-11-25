#![no_std]
#![no_main]

extern crate alloc;

mod arch_select;
mod console;
mod elf_user;
mod fb;
mod kmain;
mod kpkg;
mod syscall;
mod task;

use knightboot_protocol::KbipBootInfo;
use knightos_hal::CpuArch;
use knightos_mm::slab::KnightKernelHeap;

use crate::arch_select::ActiveCpu;

#[global_allocator]
static KERNEL_HEAP: KnightKernelHeap = KnightKernelHeap::new_uninit();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    kprintln!("\n[KERNEL PANIC] {}", info);
    ActiveCpu::halt_forever();
}

/// Higher-half kernel entry point jumped to by KnightBoot (`RDI` on x86-64, `X0` on AArch64).
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub unsafe extern "C" fn _start(_kbip: *const KbipBootInfo) -> ! {
    #[cfg(target_arch = "x86_64")]
    core::arch::naked_asm!(
        "and rsp, -16",
        "xor rbp, rbp",
        "call {entry}",
        "2: cli",
        "hlt",
        "jmp 2b",
        entry = sym kmain_trampoline,
    );

    #[cfg(target_arch = "aarch64")]
    core::arch::naked_asm!(
        "mov x29, xzr",
        "mov x30, xzr",
        "bl {entry}",
        "2: wfi",
        "b 2b",
        entry = sym kmain_trampoline,
    );
}

extern "C" fn kmain_trampoline(kbip: *const KbipBootInfo) -> ! {
    kmain::kmain(kbip)
}
