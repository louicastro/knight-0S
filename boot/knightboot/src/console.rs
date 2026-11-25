//! Early bootloader diagnostics to UEFI ConOut and architecture serial UART.

use core::fmt::{self, Write};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::uefi::boot_services_active;
use crate::uefi::protocols::SimpleTextOutputProtocol;

static CON_OUT: AtomicPtr<SimpleTextOutputProtocol> = AtomicPtr::new(null_mut());

pub fn init_console(con_out: *mut SimpleTextOutputProtocol) {
    CON_OUT.store(con_out, Ordering::Release);
    #[cfg(all(not(test), target_arch = "x86_64"))]
    x86_serial::init();
}

pub fn clear_console() {
    let ptr = CON_OUT.load(Ordering::Acquire);
    if !ptr.is_null() && boot_services_active() {
        unsafe {
            let _ = ((*ptr).clear_screen)(ptr);
        }
    }
}

pub struct BootWriter;

impl BootWriter {
    fn write_con_out(&self, s: &str) {
        if !boot_services_active() {
            return;
        }
        let ptr = CON_OUT.load(Ordering::Acquire);
        if ptr.is_null() {
            return;
        }

        // UEFI ConOut expects UTF-16 with CRLF line endings.
        let mut buf = [0u16; 128];
        let mut idx = 0usize;

        for b in s.bytes() {
            if b == b'\n' {
                if idx + 2 >= buf.len() {
                    buf[idx] = 0;
                    unsafe {
                        let _ = ((*ptr).output_string)(ptr, buf.as_ptr());
                    }
                    idx = 0;
                }
                buf[idx] = b'\r' as u16;
                buf[idx + 1] = b'\n' as u16;
                idx += 2;
            } else {
                if idx + 1 >= buf.len() {
                    buf[idx] = 0;
                    unsafe {
                        let _ = ((*ptr).output_string)(ptr, buf.as_ptr());
                    }
                    idx = 0;
                }
                buf[idx] = if b.is_ascii() { b as u16 } else { b'?' as u16 };
                idx += 1;
            }
        }

        if idx > 0 {
            buf[idx] = 0;
            unsafe {
                let _ = ((*ptr).output_string)(ptr, buf.as_ptr());
            }
        }
    }

    fn write_serial(&self, s: &str) {
        #[cfg(all(not(test), target_arch = "x86_64"))]
        x86_serial::write_str(s);

        #[cfg(not(all(not(test), target_arch = "x86_64")))]
        let _ = s;
    }
}

impl Write for BootWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let ptr = CON_OUT.load(Ordering::Acquire);
        if !ptr.is_null() && boot_services_active() {
            self.write_con_out(s);
        } else {
            self.write_serial(s);
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments<'_>) {
    let mut writer = BootWriter;
    let _ = writer.write_fmt(args);
}

#[macro_export]
macro_rules! boot_print {
    ($($arg:tt)*) => {{
        $crate::console::_print(core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! boot_println {
    () => {
        $crate::boot_print!("\n")
    };
    ($($arg:tt)*) => {{
        $crate::console::_print(core::format_args!($($arg)*));
        $crate::boot_print!("\n");
    }};
}

#[cfg(all(not(test), target_arch = "x86_64"))]
mod x86_serial {
    use core::arch::asm;

    const COM1: u16 = 0x3F8;

    unsafe fn outb(port: u16, val: u8) {
        unsafe {
            asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
        }
    }

    unsafe fn inb(port: u16) -> u8 {
        let val: u8;
        unsafe {
            asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        val
    }

    pub fn init() {
        unsafe {
            outb(COM1 + 1, 0x00); // Disable interrupts
            outb(COM1 + 3, 0x80); // Enable DLAB
            outb(COM1, 0x01); // 115200 baud low byte
            outb(COM1 + 1, 0x00); // 115200 baud high byte
            outb(COM1 + 3, 0x03); // 8N1
            outb(COM1 + 2, 0xC7); // Enable FIFO
            outb(COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set
        }
    }

    pub fn write_str(s: &str) {
        for b in s.bytes() {
            if b == b'\n' {
                write_byte(b'\r');
            }
            write_byte(b);
        }
    }

    fn write_byte(b: u8) {
        unsafe {
            // Bounded wait so QEMU/hardware without COM1 never stalls indefinitely.
            for _ in 0..10_000 {
                if (inb(COM1 + 5) & 0x20) != 0 {
                    break;
                }
            }
            outb(COM1, b);
        }
    }
}
