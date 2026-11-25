//! Architecture-independent KnightOS kernel initialization (`kmain`).

use alloc::vec::Vec;

use knight_abi::Rights;
use knightboot_protocol::KbipBootInfo;
use knightos_hal::CpuArch;
use knightos_mm::pmm::{BitmapPmm, PmmRef};
use knightos_mm::vmm::KernelVmm;
use knightos_sched::SchedClass;

use crate::arch_select::{
    ActiveCpu, ActiveMmu, discover_topology, init_early_serial, init_mmio_serial,
    init_traps_and_irq_timer, timer_ticks, wake_smp_cores,
};
use crate::{KERNEL_HEAP, elf_user, fb, kpkg, kprintln, syscall, task};

static KERNEL_PMM: BitmapPmm = BitmapPmm::new_uninit();

pub fn kmain(kbip_ptr: *const KbipBootInfo) -> ! {
    if kbip_ptr.is_null() {
        ActiveCpu::halt_forever();
    }
    let kbip = unsafe { &*kbip_ptr };

    // Stage 6.1: Remove KnightBoot's temporary lower-half identity trampoline.
    let vmm = KernelVmm::<ActiveMmu>::from_active(kbip.hhdm_offset);
    vmm.clear_trampoline();

    init_early_serial(kbip.hhdm_offset);

    if let Err(err) = kbip.validate() {
        kprintln!("[kernel] FATAL: Invalid KBIP boot info: {:?}", err);
        ActiveCpu::halt_forever();
    }

    fb::init(&kbip.framebuffer);

    let regions = unsafe { kbip.memory_regions() };
    let pmm_stats = match unsafe { KERNEL_PMM.init(regions, kbip.hhdm_offset) } {
        Ok(s) => s,
        Err(msg) => {
            kprintln!("[kernel] FATAL: PMM init failed: {}", msg);
            ActiveCpu::halt_forever();
        }
    };
    unsafe {
        KERNEL_HEAP.init(&KERNEL_PMM, kbip.hhdm_offset);
    }

    let topo = discover_topology(kbip);
    let mut pmm_alloc = PmmRef(&KERNEL_PMM);

    if let Err(msg) = init_mmio_serial(&topo, kbip.hhdm_offset, &mut pmm_alloc) {
        kprintln!("[kernel] WARNING: MMIO serial init failed: {}", msg);
    }

    kprintln!("====================================================");
    kprintln!(" KnightOS Microkernel - Phase 2/3 Bring-Up");
    kprintln!(
        " Arch: {} | KBIP v{} | HHDM={:#x}",
        ActiveCpu::arch_name(),
        kbip.version,
        kbip.hhdm_offset
    );
    kprintln!(
        " PMM: usable={} MiB | free={} MiB | regions={}",
        pmm_stats.total_usable_bytes / (1024 * 1024),
        pmm_stats.free_bytes / (1024 * 1024),
        regions.len()
    );

    let mut heap_check = Vec::with_capacity(16);
    for i in 0..16u64 {
        heap_check.push(i * 3 + 1);
    }
    let heap_sum: u64 = heap_check.iter().sum();
    kprintln!("[kernel] Heap test (Vec<u64>, sum={}): OK", heap_sum);

    // Initialize kernel task/scheduler runtime before enabling interrupts & APs.
    task::init_kernel_runtime(
        &KERNEL_PMM,
        kbip.hhdm_offset,
        kbip.arch as u64,
        topo.cpu_count,
        topo.pcie_ecam_phys,
        topo.irq_controller_phys,
    );
    task::set_fb_info(knight_abi::FbInfo {
        phys_base: kbip.framebuffer.phys_base,
        size: kbip.framebuffer.size,
        width: kbip.framebuffer.width,
        height: kbip.framebuffer.height,
        pitch_bytes: kbip.framebuffer.pitch_bytes,
        pixel_format: kbip.framebuffer.pixel_format as u32,
    });

    if let Err(msg) = init_traps_and_irq_timer(
        &topo,
        kbip.hhdm_offset,
        vmm.root(),
        &mut pmm_alloc,
        syscall::dispatch_syscall,
        task::on_timer_tick,
        task::ap_sched_loop,
        task::on_user_fault,
    ) {
        kprintln!("[kernel] FATAL: IRQ/Timer init failed: {}", msg);
        ActiveCpu::halt_forever();
    }

    ActiveCpu::enable_interrupts();
    for _ in 0..20_000_000 {
        if timer_ticks() >= 2 {
            break;
        }
        core::hint::spin_loop();
    }
    kprintln!(
        "[kernel] Interrupts & Timer active (ticks={}, irq_ctrl={:#x}, iommu={:?}@{:#x})",
        timer_ticks(),
        topo.irq_controller_phys,
        topo.iommu_kind,
        topo.iommu_phys
    );

    let online = match wake_smp_cores(&topo, kbip.hhdm_offset, &mut pmm_alloc) {
        Ok(n) => n,
        Err(msg) => {
            kprintln!("[kernel] WARNING: SMP wakeup error: {}", msg);
            1
        }
    };

    kprintln!(
        "[KNIGHTOS PHASE2 PASS] arch={} cpus={}/{} timer_ticks={} usable_mib={} heap=ok",
        ActiveCpu::arch_name(),
        online,
        topo.cpu_count,
        timer_ticks(),
        pmm_stats.total_usable_bytes / (1024 * 1024)
    );

    // Phase 3: Load `initd.elf` from `initrd.kpkg` and launch Ring 3 / EL0 user space.
    if kbip.initrd_phys != 0 && kbip.initrd_size > 0 {
        let initrd = unsafe {
            core::slice::from_raw_parts(
                (kbip.hhdm_offset + kbip.initrd_phys) as *const u8,
                kbip.initrd_size as usize,
            )
        };
        if let Some(initd_elf) = kpkg::find_entry(initrd, "initd.elf") {
            kprintln!(
                "[kernel] Unpacked initd.elf ({} bytes) from initrd.kpkg",
                initd_elf.len()
            );
            let init_task = match task::Task::create_user() {
                Ok(t) => t,
                Err(msg) => {
                    kprintln!("[kernel] FATAL: Task::create_user failed: {}", msg);
                    ActiveCpu::halt_forever();
                }
            };
            let loaded = {
                let mut vmar = init_task.vmar.lock();
                match elf_user::load_user_elf::<ActiveMmu>(
                    initd_elf,
                    &mut vmar,
                    &KERNEL_PMM,
                    kbip.hhdm_offset,
                    &mut pmm_alloc,
                ) {
                    Ok(l) => l,
                    Err(msg) => {
                        kprintln!("[kernel] FATAL: load_user_elf failed: {}", msg);
                        ActiveCpu::halt_forever();
                    }
                }
            };
            let hw_handle = init_task
                .insert_handle(task::KernelObject::HardwareRoot, Rights::ALL)
                .expect("insert HardwareRoot handle");
            let initrd_vmo = knightos_mm::vmo::Vmo::create_anonymous(
                initrd.len(),
                &KERNEL_PMM,
                kbip.hhdm_offset,
            )
            .expect("create initrd VMO");
            initrd_vmo.write(0, initrd).expect("populate initrd VMO");
            let initrd_handle = init_task
                .insert_handle(
                    task::KernelObject::Vmo(initrd_vmo),
                    Rights::READ | Rights::MAP | Rights::DUPLICATE | Rights::TRANSFER,
                )
                .expect("insert initrd handle");
            let init_arg = (hw_handle as u64) | ((initrd_handle as u64) << 32);
            kprintln!(
                "[kernel] Spawning initd in user mode (entry={:#x}, sp={:#x}, hw_cap={}, initrd_cap={})...",
                loaded.entry_va,
                loaded.stack_top_va,
                hw_handle,
                initrd_handle
            );
            if let Err(msg) = task::spawn_user_thread(
                init_task,
                loaded.entry_va,
                loaded.stack_top_va,
                init_arg,
                SchedClass::Interactive,
            ) {
                kprintln!("[kernel] FATAL: spawn_user_thread failed: {}", msg);
                ActiveCpu::halt_forever();
            }
        } else {
            kprintln!("[kernel] WARNING: initd.elf not found inside initrd.kpkg");
        }
    } else {
        kprintln!("[kernel] No initrd present; staying in kernel idle loop.");
    }

    task::bsp_sched_loop();
}
