//! Kernel synchronized serial + framebuffer console output (`kprint!`, `kprintln!`).

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use knightos_hal::SerialArch;

use crate::arch_select::ActiveSerial;
use crate::fb;

static CONSOLE_LOCK: AtomicBool = AtomicBool::new(false);

struct KernelWriter;

impl Write for KernelWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                ActiveSerial::write_byte(b'\r');
                fb::write_byte(b'\r');
            }
            ActiveSerial::write_byte(b);
            fb::write_byte(b);
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments<'_>) {
    while CONSOLE_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let mut writer = KernelWriter;
    let _ = writer.write_fmt(args);
    CONSOLE_LOCK.store(false, Ordering::Release);
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{
        $crate::console::_print(core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! kprintln {
    () => {
        $crate::kprint!("\n")
    };
    ($($arg:tt)*) => {{
        $crate::console::_print(core::format_args!($($arg)*));
        $crate::kprint!("\n");
    }};
}
