//! Isolated architecture-selection adapter (`x86_64` vs `aarch64`).
//!
//! This is the single module inside `kernel/` that dispatches via `#[cfg(target_arch)]`
//! to the concrete HAL crate under `arch/`.

use core::sync::atomic::Ordering;
use knightboot_protocol::KbipBootInfo;
use knightos_hal::{
    PhysAddr, PhysFrameAllocator, PlatformTopology, SyscallDispatchFn, TimerTickFn,
};
#[cfg(target_arch = "x86_64")]
use knightos_hal::SerialArch;

#[cfg(target_arch = "x86_64")]
pub use knightos_arch_x86_64::{
    X86Cpu as ActiveCpu, context as arch_ctx, mmu::X86Mmu as ActiveMmu,
    serial::X86Serial as ActiveSerial,
};

#[cfg(target_arch = "aarch64")]
pub use knightos_arch_aarch64::{
    ArmCpu as ActiveCpu, context as arch_ctx, mmu::ArmMmu as ActiveMmu,
    serial::ArmSerial as ActiveSerial,
};

pub fn init_early_serial(hhdm_offset: u64) {
    #[cfg(target_arch = "x86_64")]
    ActiveSerial::init(hhdm_offset, 0);
    #[cfg(target_arch = "aarch64")]
    let _ = hhdm_offset;
}

pub fn discover_topology(kbip: &KbipBootInfo) -> PlatformTopology {
    #[cfg(target_arch = "x86_64")]
    {
        use knightos_hal::CpuArch;
        knightos_arch_x86_64::acpi::discover_topology(
            kbip.acpi_rsdp_phys,
            kbip.hhdm_offset,
            ActiveCpu::current_hw_id(),
        )
    }

    #[cfg(target_arch = "aarch64")]
    {
        use knightos_hal::CpuArch;
        knightos_arch_aarch64::dtb::discover_topology(
            kbip.acpi_rsdp_phys,
            kbip.dtb_phys,
            kbip.hhdm_offset,
            ActiveCpu::current_hw_id() as u64,
        )
    }
}

pub fn init_mmio_serial(
    topo: &PlatformTopology,
    hhdm_offset: u64,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<(), &'static str> {
    #[cfg(target_arch = "aarch64")]
    {
        ActiveSerial::map_and_init(hhdm_offset, topo.uart_phys, alloc)?;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let _ = (topo, hhdm_offset, alloc);
    }
    Ok(())
}

pub fn init_traps_and_irq_timer(
    topo: &PlatformTopology,
    hhdm_offset: u64,
    root: PhysAddr,
    alloc: &mut dyn PhysFrameAllocator,
    syscall_fn: SyscallDispatchFn,
    timer_fn: TimerTickFn,
    ap_entry_fn: extern "C" fn(usize) -> !,
    user_fault_fn: extern "C" fn(u64, u64, u64) -> !,
) -> Result<(), &'static str> {
    arch_ctx::register_syscall_handler(syscall_fn);

    #[cfg(target_arch = "x86_64")]
    {
        let rsp: u64;
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
        }
        knightos_arch_x86_64::gdt_idt::init_bsp();
        arch_ctx::init_per_cpu(0, rsp);
        arch_ctx::init_syscall_msrs();
        knightos_arch_x86_64::gdt_idt::register_timer_hook(timer_fn);
        knightos_arch_x86_64::gdt_idt::register_user_fault_hook(user_fault_fn);
        knightos_arch_x86_64::apic::register_ap_sched_entry(ap_entry_fn);
        let _ = knightos_arch_x86_64::apic::init_bsp(
            topo.irq_controller_phys,
            hhdm_offset,
            root,
            alloc,
        )?;
        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    {
        let _ = root;
        knightos_arch_aarch64::vectors::init_vbar();
        arch_ctx::init_per_cpu(0, 0);
        arch_ctx::init_syscall_msrs();
        knightos_arch_aarch64::vectors::register_timer_hook(timer_fn);
        knightos_arch_aarch64::vectors::register_user_fault_hook(user_fault_fn);
        knightos_arch_aarch64::gic::register_ap_sched_entry(ap_entry_fn);
        knightos_arch_aarch64::gic::init_bsp(topo, hhdm_offset, alloc)?;
        Ok(())
    }
}

pub fn wake_smp_cores(
    topo: &PlatformTopology,
    hhdm_offset: u64,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<usize, &'static str> {
    #[cfg(target_arch = "x86_64")]
    {
        use knightos_hal::MmuArch;
        let root = ActiveMmu::active_kernel_root();
        knightos_arch_x86_64::apic::wake_aps(topo, hhdm_offset, root, alloc)
    }

    #[cfg(target_arch = "aarch64")]
    {
        knightos_arch_aarch64::gic::wake_aps(topo, hhdm_offset, alloc)
    }
}

pub fn timer_ticks() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        knightos_arch_x86_64::gdt_idt::TIMER_TICKS.load(Ordering::Relaxed)
    }
    #[cfg(target_arch = "aarch64")]
    {
        knightos_arch_aarch64::vectors::TIMER_TICKS.load(Ordering::Relaxed)
    }
}

pub fn online_cpus() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        knightos_arch_x86_64::apic::online_cpu_count()
    }
    #[cfg(target_arch = "aarch64")]
    {
        knightos_arch_aarch64::gic::online_cpu_count()
    }
}
