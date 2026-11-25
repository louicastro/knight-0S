//! ARM64 PL011 UART serial driver (mapped via HHDM).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use knightos_hal::{MmuArch, PageFlags, PhysAddr, PhysFrameAllocator, SerialArch, VirtAddr};

use crate::mmu::ArmMmu;

/// Default QEMU `virt` PL011 UART physical base.
pub const QEMU_VIRT_PL011_PHYS: u64 = 0x0900_0000;

static UART_VIRT_BASE: AtomicU64 = AtomicU64::new(0);

const UART_DR: usize = 0x00;
const UART_FR: usize = 0x18;
const UART_CR: usize = 0x30;
const FR_TXFF: u32 = 1 << 5;

pub struct ArmSerial;

impl ArmSerial {
    /// Maps the PL011 MMIO page in `TTBR1_EL1` so `ArmSerial` can write after `clear_lower_half`.
    pub fn map_and_init(
        hhdm_offset: u64,
        uart_phys: u64,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<(), &'static str> {
        let phys = if uart_phys != 0 {
            uart_phys & !0xFFF
        } else {
            QEMU_VIRT_PL011_PHYS
        };
        let virt = hhdm_offset + phys;
        let root = ArmMmu::active_kernel_root();
        ArmMmu::map_4k(
            root,
            hhdm_offset,
            VirtAddr(virt),
            PhysAddr(phys),
            PageFlags::mmio_rw(),
            alloc,
        )?;
        UART_VIRT_BASE.store(virt + (uart_phys & 0xFFF), Ordering::Release);
        unsafe {
            // Ensure UARTEN (bit 0) and TXE (bit 8) are enabled.
            let cr = read_volatile((virt + UART_CR as u64) as *const u32);
            write_volatile((virt + UART_CR as u64) as *mut u32, cr | (1 << 0) | (1 << 8));
        }
        Ok(())
    }
}

impl SerialArch for ArmSerial {
    fn init(_hhdm_offset: u64, _base_phys_override: u64) {
        // Call `map_and_init` once PMM is available so Device-nGnRnE MMIO is mapped safely.
    }

    fn write_byte(byte: u8) {
        let base = UART_VIRT_BASE.load(Ordering::Acquire);
        if base == 0 {
            return;
        }
        unsafe {
            for _ in 0..50_000 {
                if (read_volatile((base + UART_FR as u64) as *const u32) & FR_TXFF) == 0 {
                    break;
                }
            }
            write_volatile((base + UART_DR as u64) as *mut u32, byte as u32);
        }
    }
}
