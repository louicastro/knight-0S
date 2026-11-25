//! x86-64 COM1 (0x3F8) 16550 UART serial driver.

use core::arch::asm;
use knightos_hal::SerialArch;

const COM1: u16 = 0x3F8;

pub struct X86Serial;

#[inline(always)]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

impl SerialArch for X86Serial {
    fn init(_hhdm_offset: u64, _base_phys_override: u64) {
        unsafe {
            outb(COM1 + 1, 0x00); // Disable interrupts
            outb(COM1 + 3, 0x80); // DLAB on
            outb(COM1, 0x01);     // 115200 baud (divisor 1)
            outb(COM1 + 1, 0x00);
            outb(COM1 + 3, 0x03); // 8N1
            outb(COM1 + 2, 0xC7); // Enable FIFO, clear 14-byte threshold
            outb(COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set
        }
    }

    fn write_byte(byte: u8) {
        unsafe {
            for _ in 0..20_000 {
                if (inb(COM1 + 5) & 0x20) != 0 {
                    break;
                }
            }
            outb(COM1, byte);
        }
    }
}
