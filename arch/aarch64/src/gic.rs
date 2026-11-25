//! ARM64 GICv3 Distributor/Redistributor/CPU-Interface, Generic Timer, and PSCI SMP.

use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use knightos_hal::{MmuArch, PageFlags, PhysAddr, PhysFrameAllocator, PlatformTopology, VirtAddr};

use crate::context;
use crate::mmu::ArmMmu;
use crate::vectors;

/// Non-secure Physical Timer PPI (INTID 30).
pub const TIMER_PPI_INTID: u32 = 30;
const PSCI_CPU_ON_64: u64 = 0xC400_0003;

static GICD_VIRT: AtomicU64 = AtomicU64::new(0);
static GICR_VIRT: AtomicU64 = AtomicU64::new(0);
static TIMER_RELOAD_TICKS: AtomicU64 = AtomicU64::new(0);
static AP_ONLINE_COUNT: AtomicUsize = AtomicUsize::new(1);
static AP_SCHED_ENTRY: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub type ApSchedEntryFn = extern "C" fn(cpu_idx: usize) -> !;

pub fn register_ap_sched_entry(entry: ApSchedEntryFn) {
    AP_SCHED_ENTRY.store(entry as *mut (), Ordering::Release);
}

#[repr(C)]
struct ArmApMailbox {
    tcr_el1: u64,
    mair_el1: u64,
    ttbr1_el1: u64,
    ttbr0_temp_phys: u64,
    stack_top_virt: u64,
    entry_virt: u64,
    logical_cpu_id: u64,
}

unsafe extern "C" {
    static arm_ap_trampoline_start: u8;
    static arm_ap_trampoline_end: u8;
}

global_asm!(
    r#"
.section .rodata
.balign 16
.global arm_ap_trampoline_start
.global arm_ap_trampoline_end
arm_ap_trampoline_start:
    // Entry from PSCI CPU_ON with X0 = physical address of ArmApMailbox.
    msr daifset, #0xf
    mov x19, x0

    // If booted at EL2, drop cleanly to EL1h with EL1 MMU off.
    mrs x9, CurrentEL
    ubfx x9, x9, #2, #2
    cmp x9, #2
    b.ne 1f

    mov x9, #(1 << 31)
    msr hcr_el2, x9
    mov x9, #3
    msr cnthctl_el2, x9
    msr cntvoff_el2, xzr
    movz x9, #0x0800
    movk x9, #0x30D0, lsl #16
    msr sctlr_el1, x9
    mov x9, #0x3c5
    msr spsr_el2, x9
    adr x9, 1f
    msr elr_el2, x9
    isb
    eret

1:
    ldr x10, [x19, #0]   // tcr_el1
    ldr x11, [x19, #8]   // mair_el1
    ldr x12, [x19, #16]  // ttbr1_el1
    ldr x13, [x19, #24]  // ttbr0_temp_phys
    ldr x20, [x19, #32]  // stack_top_virt
    ldr x21, [x19, #40]  // entry_virt
    ldr x22, [x19, #48]  // logical_cpu_id

    msr tcr_el1, x10
    msr mair_el1, x11
    msr ttbr0_el1, x13
    msr ttbr1_el1, x12
    dsb ish
    isb
    tlbi vmalle1
    dsb ish
    isb

    mrs x9, sctlr_el1
    orr x9, x9, #(1 << 0)
    orr x9, x9, #(1 << 2)
    orr x9, x9, #(1 << 3)
    orr x9, x9, #(1 << 12)
    msr sctlr_el1, x9
    isb

    mov sp, x20
    mov x0, x22
    mov x29, xzr
    mov x30, xzr
    br x21
arm_ap_trampoline_end:
"#
);

/// Maps GICv3 Distributor + Redistributor pages, initializes GICv3 & Generic Timer on BSP.
pub fn init_bsp(
    topo: &PlatformTopology,
    hhdm_offset: u64,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<(), &'static str> {
    let root = ArmMmu::active_kernel_root();

    // Map GICD (64 KiB = 16 pages).
    let gicd_phys = topo.irq_controller_phys & !0xFFF;
    for i in 0..16u64 {
        let off = i * 4096;
        ArmMmu::map_4k(
            root,
            hhdm_offset,
            VirtAddr(hhdm_offset + gicd_phys + off),
            PhysAddr(gicd_phys + off),
            PageFlags::mmio_rw(),
            alloc,
        )?;
    }

    // Map GICR frames for up to cpu_count cores (128 KiB per core = 32 pages each).
    let gicr_phys = topo.irq_redistributor_phys & !0xFFF;
    let pages_per_cpu = 32u64;
    let total_gicr_pages = (topo.cpu_count.max(1) as u64) * pages_per_cpu;
    for i in 0..total_gicr_pages {
        let off = i * 4096;
        ArmMmu::map_4k(
            root,
            hhdm_offset,
            VirtAddr(hhdm_offset + gicr_phys + off),
            PhysAddr(gicr_phys + off),
            PageFlags::mmio_rw(),
            alloc,
        )?;
    }

    let gicd_v = hhdm_offset + gicd_phys;
    let gicr_v = hhdm_offset + gicr_phys;
    GICD_VIRT.store(gicd_v, Ordering::Release);
    GICR_VIRT.store(gicr_v, Ordering::Release);

    unsafe {
        // Enable Distributor (ARE_NS | EnableGrp1NS).
        write_volatile(gicd_v as *mut u32, (1 << 4) | (1 << 1));

        init_redistributor_and_cpu_iface(0);

        // Program ARM Generic Timer at 100 Hz.
        let freq: u64;
        asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack));
        let reload = (freq / 100).max(1000);
        TIMER_RELOAD_TICKS.store(reload, Ordering::Release);

        asm!("msr cntp_tval_el0, {}", in(reg) reload, options(nostack));
        asm!("msr cntp_ctl_el0, {}", in(reg) 1u64, options(nostack));
    }

    Ok(())
}

unsafe fn init_redistributor_and_cpu_iface(cpu_idx: usize) {
    let gicr_base = GICR_VIRT.load(Ordering::Acquire) + (cpu_idx as u64) * 0x20000;
    let sgi_base = gicr_base + 0x10000;

    unsafe {
        // Wake Redistributor (GICR_WAKER bit 1 = ProcessorSleep -> clear, wait bit 2 = ChildrenAsleep == 0).
        let waker_ptr = (gicr_base + 0x0014) as *mut u32;
        let mut waker = read_volatile(waker_ptr);
        waker &= !(1 << 1);
        write_volatile(waker_ptr, waker);
        for _ in 0..100_000 {
            if (read_volatile(waker_ptr) & (1 << 2)) == 0 {
                break;
            }
        }

        // Put PPIs (0..31) in Group 1 NS and enable PPI 30 (timer).
        write_volatile((sgi_base + 0x0080) as *mut u32, 0xFFFF_FFFF);
        write_volatile((sgi_base + 0x0100) as *mut u32, 1 << TIMER_PPI_INTID);

        // Enable GICv3 System Register Interface (ICC_SRE_EL1.SRE = 1).
        let mut sre: u64;
        asm!("mrs {}, icc_sre_el1", out(reg) sre, options(nomem, nostack));
        sre |= 1;
        asm!("msr icc_sre_el1, {}", "isb", in(reg) sre, options(nostack));

        // Unmask all priorities (ICC_PMR_EL1 = 0xFF) and enable Group 1 interrupts.
        asm!("msr icc_pmr_el1, {}", in(reg) 0xFFu64, options(nostack));
        asm!("msr icc_igrpen1_el1, {}", "isb", in(reg) 1u64, options(nostack));
    }
}

pub fn ack_irq() -> u32 {
    let iar: u64;
    unsafe {
        asm!("mrs {}, icc_iar1_el1", out(reg) iar, options(nomem, nostack));
    }
    iar as u32
}

pub fn eoi_irq(iar: u32) {
    unsafe {
        asm!("msr icc_eoir1_el1, {}", in(reg) iar as u64, options(nostack));
    }
}

pub fn rearm_timer() {
    let reload = TIMER_RELOAD_TICKS.load(Ordering::Acquire);
    if reload > 0 {
        unsafe {
            asm!("msr cntp_tval_el0, {}", in(reg) reload, options(nostack));
        }
    }
}

/// Wakes secondary ARM64 cores via `PSCI CPU_ON` and returns total online cores.
pub fn wake_aps(
    topo: &PlatformTopology,
    hhdm_offset: u64,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<usize, &'static str> {
    if topo.cpu_count <= 1 {
        return Ok(1);
    }

    let stub_start = &raw const arm_ap_trampoline_start as *const u8 as usize;
    let stub_end = &raw const arm_ap_trampoline_end as *const u8 as usize;
    let stub_len = stub_end - stub_start;
    if stub_len > 4096 {
        return Err("ARM64 AP trampoline exceeds 1 page");
    }

    let stub_phys = alloc
        .alloc_frame_zeroed(hhdm_offset)
        .ok_or("OOM allocating ARM64 AP trampoline")?;
    let mailbox_phys = alloc
        .alloc_frame_zeroed(hhdm_offset)
        .ok_or("OOM allocating ARM64 AP mailbox")?;

    unsafe {
        let dst = (hhdm_offset + stub_phys.as_u64()) as *mut u8;
        core::ptr::copy_nonoverlapping(stub_start as *const u8, dst, stub_len);
        clean_dcache_range(dst as u64, stub_len);
    }

    // Build temporary identity-mapped TTBR0 for the single AP trampoline page.
    let ttbr0_temp_phys = alloc
        .alloc_frame_zeroed(hhdm_offset)
        .ok_or("OOM allocating ARM64 AP TTBR0")?;
    ArmMmu::map_4k(
        ttbr0_temp_phys,
        hhdm_offset,
        VirtAddr(stub_phys.as_u64()),
        stub_phys,
        knightos_hal::PageFlags::kernel_rx(),
        alloc,
    )?;

    let tcr_el1: u64;
    let mair_el1: u64;
    let ttbr1_el1: u64;
    unsafe {
        asm!("mrs {}, tcr_el1", out(reg) tcr_el1, options(nomem, nostack));
        asm!("mrs {}, mair_el1", out(reg) mair_el1, options(nomem, nostack));
        asm!("mrs {}, ttbr1_el1", out(reg) ttbr1_el1, options(nomem, nostack));
    }

    // Flush all 4 page-table levels (L0..L3) of ttbr0_temp_phys to PoC so AP with caches off sees valid tables.
    flush_4level_tables_to_poc(ttbr0_temp_phys.as_u64(), stub_phys.as_u64(), hhdm_offset);

    for cpu in topo.cpus[..topo.cpu_count].iter() {
        if !cpu.enabled || cpu.is_bsp {
            continue;
        }

        let stack_phys = alloc
            .alloc_contiguous_zeroed(4, hhdm_offset)
            .ok_or("OOM allocating ARM64 AP stack")?;
        let stack_top_virt = hhdm_offset + stack_phys.as_u64() + (4 * 4096);

        let mailbox_virt = (hhdm_offset + mailbox_phys.as_u64()) as *mut ArmApMailbox;
        unsafe {
            write_volatile(
                mailbox_virt,
                ArmApMailbox {
                    tcr_el1,
                    mair_el1,
                    ttbr1_el1,
                    ttbr0_temp_phys: ttbr0_temp_phys.as_u64(),
                    stack_top_virt,
                    entry_virt: ap_rust_entry as *const () as usize as u64,
                    logical_cpu_id: cpu.logical_id as u64,
                },
            );
            clean_dcache_range(mailbox_virt as u64, core::mem::size_of::<ArmApMailbox>());
        }

        let prev = AP_ONLINE_COUNT.load(Ordering::Acquire);
        let ret = psci_call(
            topo.psci_use_hvc,
            PSCI_CPU_ON_64,
            cpu.hw_id,
            stub_phys.as_u64(),
            mailbox_phys.as_u64(),
        );
        if ret == 0 {
            for _ in 0..5_000_000 {
                if AP_ONLINE_COUNT.load(Ordering::Acquire) > prev {
                    break;
                }
                core::hint::spin_loop();
            }
        }
    }

    Ok(AP_ONLINE_COUNT.load(Ordering::Acquire))
}

unsafe fn clean_dcache_range(start_virt: u64, len: usize) {
    let mut addr = start_virt & !63;
    let end = start_virt + len as u64;
    while addr < end {
        unsafe {
            asm!("dc cvac, {}", in(reg) addr, options(nostack));
        }
        addr += 64;
    }
    unsafe {
        asm!("dsb sy", "isb", options(nostack));
    }
}

fn flush_4level_tables_to_poc(l0_phys: u64, va: u64, hhdm_offset: u64) {
    const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
    let l0_virt = l0_phys + hhdm_offset;
    flush_dcache_po_c_page(l0_virt);
    let l0 = unsafe { &*(l0_virt as *const [u64; 512]) };
    let e0 = l0[((va >> 39) & 0x1FF) as usize];
    if (e0 & 1) == 0 {
        return;
    }
    let l1_virt = (e0 & ADDR_MASK) + hhdm_offset;
    flush_dcache_po_c_page(l1_virt);
    let l1 = unsafe { &*(l1_virt as *const [u64; 512]) };
    let e1 = l1[((va >> 30) & 0x1FF) as usize];
    if (e1 & 1) == 0 {
        return;
    }
    let l2_virt = (e1 & ADDR_MASK) + hhdm_offset;
    flush_dcache_po_c_page(l2_virt);
    let l2 = unsafe { &*(l2_virt as *const [u64; 512]) };
    let e2 = l2[((va >> 21) & 0x1FF) as usize];
    if (e2 & 1) == 0 {
        return;
    }
    let l3_virt = (e2 & ADDR_MASK) + hhdm_offset;
    flush_dcache_po_c_page(l3_virt);
}

fn flush_dcache_po_c_page(virt: u64) {
    unsafe {
        clean_dcache_range(virt, 4096);
    }
}

fn psci_call(use_hvc: bool, fid: u64, arg1: u64, arg2: u64, arg3: u64) -> i64 {
    let ret: i64;
    unsafe {
        if use_hvc {
            asm!(
                "hvc #0",
                inout("x0") fid => ret,
                in("x1") arg1,
                in("x2") arg2,
                in("x3") arg3,
                options(nostack)
            );
        } else {
            asm!(
                "smc #0",
                inout("x0") fid => ret,
                in("x1") arg1,
                in("x2") arg2,
                in("x3") arg3,
                options(nostack)
            );
        }
    }
    ret
}

extern "C" fn ap_rust_entry(logical_cpu_id: u64) -> ! {
    let idx = logical_cpu_id as usize;
    ArmMmu::clear_lower_half(0);
    // Enable FP/NEON access at EL1 and EL0 on secondary cores (CPACR_EL1.FPEN = 0b11).
    unsafe {
        asm!("mov x9, #(3 << 20)", "msr cpacr_el1, x9", "isb", out("x9") _, options(nomem, nostack));
    }
    vectors::init_vbar();
    context::init_per_cpu(idx, 0);
    unsafe {
        init_redistributor_and_cpu_iface(idx);
        let reload = TIMER_RELOAD_TICKS.load(Ordering::Acquire);
        if reload > 0 {
            asm!("msr cntp_tval_el0, {}", in(reg) reload, options(nostack));
            asm!("msr cntp_ctl_el0, {}", in(reg) 1u64, options(nostack));
        }
    }
    AP_ONLINE_COUNT.fetch_add(1, Ordering::Release);
    let hook = AP_SCHED_ENTRY.load(Ordering::Acquire);
    if !hook.is_null() {
        let f: ApSchedEntryFn = unsafe { core::mem::transmute(hook) };
        f(idx);
    }
    loop {
        unsafe {
            asm!("msr daifclr, #2", "wfi", options(nomem, nostack));
        }
    }
}

pub fn online_cpu_count() -> usize {
    AP_ONLINE_COUNT.load(Ordering::Acquire)
}
