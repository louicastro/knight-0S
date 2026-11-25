//! x86-64 Local APIC, timer calibration, and INIT-SIPI-SIPI SMP wakeup.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use knightos_hal::{MmuArch, PageFlags, PhysAddr, PhysFrameAllocator, PlatformTopology, VirtAddr};

use crate::context;
use crate::gdt_idt;
use crate::mmu::X86Mmu;
use crate::serial::{inb, outb};

const AP_TRAMPOLINE_PHYS: u64 = 0x8000;
const AP_MAILBOX_PHYS: u64 = 0x9000;

static AP_BOOT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ap_boot.bin"));
static LAPIC_VIRT_BASE: AtomicU64 = AtomicU64::new(0);
static AP_ONLINE_COUNT: AtomicUsize = AtomicUsize::new(1);
static CALIBRATED_RELOAD: AtomicU32 = AtomicU32::new(62_500);
static AP_SCHED_ENTRY: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub type ApSchedEntryFn = extern "C" fn(cpu_idx: usize) -> !;

pub fn register_ap_sched_entry(entry: ApSchedEntryFn) {
    AP_SCHED_ENTRY.store(entry as *mut (), Ordering::Release);
}

const LAPIC_ID: usize = 0x020;
const LAPIC_EOI: usize = 0x0B0;
const LAPIC_SVR: usize = 0x0F0;
const LAPIC_ICR_LOW: usize = 0x300;
const LAPIC_ICR_HIGH: usize = 0x310;
const LAPIC_LVT_TIMER: usize = 0x320;
const LAPIC_TIMER_INIT: usize = 0x380;
const LAPIC_TIMER_CUR: usize = 0x390;
const LAPIC_TIMER_DIV: usize = 0x3E0;

#[repr(C, packed)]
struct ApMailbox {
    cr3_phys: u64,
    stack_top_virt: u64,
    entry_virt: u64,
    logical_cpu_id: u64,
    gdt: [u64; 5],
    gdtr_limit: u16,
    gdtr_base: u32,
}

#[inline]
unsafe fn lapic_read(offset: usize) -> u32 {
    let base = LAPIC_VIRT_BASE.load(Ordering::Acquire);
    unsafe { read_volatile((base + offset as u64) as *const u32) }
}

#[inline]
unsafe fn lapic_write(offset: usize, val: u32) {
    let base = LAPIC_VIRT_BASE.load(Ordering::Acquire);
    unsafe { write_volatile((base + offset as u64) as *mut u32, val) };
}

pub fn eoi() {
    if LAPIC_VIRT_BASE.load(Ordering::Acquire) != 0 {
        unsafe {
            lapic_write(LAPIC_EOI, 0);
        }
    }
}

/// Maps the Local APIC MMIO page, enables the BSP Local APIC, and starts a 100 Hz timer.
pub fn init_bsp(
    lapic_phys: u64,
    hhdm_offset: u64,
    root: PhysAddr,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<u32, &'static str> {
    let lapic_virt = hhdm_offset + lapic_phys;
    X86Mmu::map_4k(
        root,
        hhdm_offset,
        VirtAddr(lapic_virt),
        PhysAddr(lapic_phys),
        PageFlags::mmio_rw(),
        alloc,
    )?;
    LAPIC_VIRT_BASE.store(lapic_virt, Ordering::Release);

    unsafe {
        // Enable Local APIC via Spurious Interrupt Vector Register (vector 0xFF, enable bit 8).
        lapic_write(LAPIC_SVR, 0x100 | 0xFF);
        // Calibrate APIC timer against PIT channel 2 (~10 ms gate).
        calibrate_and_start_timer(100);
    }

    let ticks_per_sec = 100u32;
    let _ = ticks_per_sec;
    Ok(unsafe { (lapic_read(LAPIC_ID) >> 24) & 0xFF })
}

unsafe fn pit_sleep_10ms() {
    unsafe {
        // Program PIT Channel 2 one-shot for ~10 ms (11932 ticks at 1.193182 MHz).
        let gate = inb(0x61);
        outb(0x61, (gate & 0xFD) | 0x01);
        outb(0x43, 0xB0); // Ch2, lobyte/hibyte, mode 0
        let count: u16 = 11932;
        outb(0x42, (count & 0xFF) as u8);
        outb(0x42, (count >> 8) as u8);

        // Pulse gate low->high to start counting.
        let g = inb(0x61) & 0xFE;
        outb(0x61, g);
        outb(0x61, g | 0x01);

        for _ in 0..5_000_000 {
            if (inb(0x61) & 0x20) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }
}

unsafe fn calibrate_and_start_timer(hz: u32) {
    unsafe {
        lapic_write(LAPIC_TIMER_DIV, 0x03); // Divide by 16
        lapic_write(LAPIC_LVT_TIMER, 1 << 16); // Masked during calibration
        lapic_write(LAPIC_TIMER_INIT, 0xFFFF_FFFF);

        pit_sleep_10ms();

        let elapsed_10ms = 0xFFFF_FFFFu32.wrapping_sub(lapic_read(LAPIC_TIMER_CUR));
        let ticks_per_sec = (elapsed_10ms as u64).saturating_mul(100);
        let reload = (ticks_per_sec / hz.max(1) as u64).max(1000) as u32;
        CALIBRATED_RELOAD.store(reload, Ordering::Release);

        // Periodic mode (bit 17), vector 32, unmasked.
        lapic_write(LAPIC_LVT_TIMER, (1 << 17) | 32);
        lapic_write(LAPIC_TIMER_DIV, 0x03);
        lapic_write(LAPIC_TIMER_INIT, reload);
    }
}

/// Wakes all enabled Application Processors via `INIT-SIPI-SIPI` and returns the total
/// number of online CPUs (BSP + APs).
pub fn wake_aps(
    topo: &PlatformTopology,
    hhdm_offset: u64,
    root: PhysAddr,
    alloc: &mut dyn PhysFrameAllocator,
) -> Result<usize, &'static str> {
    if topo.cpu_count <= 1 {
        return Ok(1);
    }
    if root.as_u64() >= 0x1_0000_0000 {
        return Err("CR3 above 4 GiB unsupported by 32-bit AP trampoline");
    }

    // Temporarily identity-map 0x8000 (RX trampoline) and 0x9000 (RW mailbox) with strict W^X.
    X86Mmu::map_4k(
        root,
        hhdm_offset,
        VirtAddr(AP_TRAMPOLINE_PHYS),
        PhysAddr(AP_TRAMPOLINE_PHYS),
        PageFlags::kernel_rx(),
        alloc,
    )?;
    X86Mmu::map_4k(
        root,
        hhdm_offset,
        VirtAddr(AP_MAILBOX_PHYS),
        PhysAddr(AP_MAILBOX_PHYS),
        PageFlags::kernel_rw(),
        alloc,
    )?;

    // Copy NASM trampoline binary into physical 0x8000 via writable HHDM alias.
    unsafe {
        let dst = (hhdm_offset + AP_TRAMPOLINE_PHYS) as *mut u8;
        core::ptr::write_bytes(dst, 0, 4096);
        core::ptr::copy_nonoverlapping(AP_BOOT_BIN.as_ptr(), dst, AP_BOOT_BIN.len());
    }

    let mut target_online = 1usize;

    for cpu in topo.cpus[..topo.cpu_count].iter() {
        if !cpu.enabled || cpu.is_bsp {
            continue;
        }

        // Allocate a 16 KiB contiguous AP stack mapped in HHDM.
        let stack_frame = alloc
            .alloc_contiguous_zeroed(4, hhdm_offset)
            .ok_or("OOM allocating AP stack")?;
        let stack_top_virt = hhdm_offset + stack_frame.as_u64() + (4 * 4096);

        let mailbox = (hhdm_offset + AP_MAILBOX_PHYS) as *mut ApMailbox;
        unsafe {
            write_volatile(
                mailbox,
                ApMailbox {
                    cr3_phys: root.as_u64(),
                    stack_top_virt,
                    entry_virt: ap_rust_entry as *const () as usize as u64,
                    logical_cpu_id: cpu.logical_id as u64,
                    gdt: [
                        0x0000_0000_0000_0000, // 0x00: Null
                        0x00CF_9B00_0000_FFFF, // 0x08: 32-bit Code (Accessed=1)
                        0x00CF_9300_0000_FFFF, // 0x10: 32-bit Data (Accessed=1)
                        0x00AF_9B00_0000_FFFF, // 0x18: 64-bit Code (Accessed=1)
                        0x00CF_9300_0000_FFFF, // 0x20: 64-bit Data (Accessed=1)
                    ],
                    gdtr_limit: 39,
                    gdtr_base: (AP_MAILBOX_PHYS + 0x20) as u32,
                },
            );
        }

        let prev_online = AP_ONLINE_COUNT.load(Ordering::Acquire);
        let apic_id = cpu.hw_id as u32;

        unsafe {
            // Clear APIC error status before INIT-SIPI-SIPI.
            lapic_write(0x280, 0);

            // INIT IPI (level assert, physical destination)
            lapic_write(LAPIC_ICR_HIGH, apic_id << 24);
            lapic_write(LAPIC_ICR_LOW, 0x0000_4500);
            wait_icr_idle();

            // De-assert INIT IPI
            lapic_write(LAPIC_ICR_HIGH, apic_id << 24);
            lapic_write(LAPIC_ICR_LOW, 0x0000_8500);
            wait_icr_idle();
            pit_sleep_10ms();

            // SIPI #1 -> vector 0x08 (0x8000)
            lapic_write(LAPIC_ICR_HIGH, apic_id << 24);
            lapic_write(LAPIC_ICR_LOW, 0x0000_4608);
            wait_icr_idle();

            // Wait up to ~10ms for AP acknowledgement; send SIPI #2 if needed.
            if !wait_for_ap(prev_online + 1, 500_000) {
                lapic_write(LAPIC_ICR_HIGH, apic_id << 24);
                lapic_write(LAPIC_ICR_LOW, 0x0000_4608);
                wait_icr_idle();
                let _ = wait_for_ap(prev_online + 1, 2_000_000);
            }
        }

        if AP_ONLINE_COUNT.load(Ordering::Acquire) > prev_online {
            target_online += 1;
        }
    }

    // Tear down temporary lower-half SMP trampoline mappings.
    let _ = X86Mmu::unmap_4k(root, hhdm_offset, VirtAddr(AP_TRAMPOLINE_PHYS));
    let _ = X86Mmu::unmap_4k(root, hhdm_offset, VirtAddr(AP_MAILBOX_PHYS));

    let _ = target_online;
    Ok(AP_ONLINE_COUNT.load(Ordering::Acquire))
}

unsafe fn wait_icr_idle() {
    unsafe {
        for _ in 0..1_000_000 {
            if (lapic_read(LAPIC_ICR_LOW) & (1 << 12)) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }
}

fn wait_for_ap(expected: usize, max_spins: usize) -> bool {
    for _ in 0..max_spins {
        if AP_ONLINE_COUNT.load(Ordering::Acquire) >= expected {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

extern "C" fn ap_rust_entry(logical_cpu_id: u64) -> ! {
    let idx = logical_cpu_id as usize;
    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
    }
    gdt_idt::init_ap(idx);
    context::init_per_cpu(idx, rsp);
    context::init_syscall_msrs();
    unsafe {
        lapic_write(LAPIC_SVR, 0x100 | 0xFF);
        // Enable 100 Hz periodic timer on APs as well so all cores drive preemption.
        lapic_write(LAPIC_TIMER_DIV, 0x03);
        lapic_write(LAPIC_LVT_TIMER, (1 << 17) | 32);
        lapic_write(LAPIC_TIMER_INIT, CALIBRATED_RELOAD.load(Ordering::Acquire));
    }
    AP_ONLINE_COUNT.fetch_add(1, Ordering::Release);
    let hook = AP_SCHED_ENTRY.load(Ordering::Acquire);
    if !hook.is_null() {
        let f: ApSchedEntryFn = unsafe { core::mem::transmute(hook) };
        f(idx);
    }
    loop {
        unsafe {
            core::arch::asm!("sti", "hlt", options(nomem, nostack));
        }
    }
}

pub fn online_cpu_count() -> usize {
    AP_ONLINE_COUNT.load(Ordering::Acquire)
}
