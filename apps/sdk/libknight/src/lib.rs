#![no_std]
//! `libknight` — User-space syscall wrappers and minimal runtime for KnightOS services.

pub use knight_abi::*;

use core::fmt::{self, Write};

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub unsafe fn syscall6(
    num: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> KnightStatus {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") num as i64 => ret,
            inlateout("rdi") a0 => _,
            inlateout("rsi") a1 => _,
            inlateout("rdx") a2 => _,
            inlateout("r10") a3 => _,
            inlateout("r8") a4 => _,
            inlateout("r9") a5 => _,
            out("rcx") _,
            out("r11") _,
            options(nostack)
        );
    }
    ret
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn syscall6(
    num: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> KnightStatus {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") num,
            inlateout("x0") a0 as i64 => ret,
            inlateout("x1") a1 => _,
            inlateout("x2") a2 => _,
            inlateout("x3") a3 => _,
            inlateout("x4") a4 => _,
            inlateout("x5") a5 => _,
            options(nostack)
        );
    }
    ret
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub unsafe fn syscall6(
    _num: u64,
    _a0: u64,
    _a1: u64,
    _a2: u64,
    _a3: u64,
    _a4: u64,
    _a5: u64,
) -> KnightStatus {
    status::ERR_INTERNAL
}

#[inline(always)]
pub fn debug_log(msg: &str) -> KnightStatus {
    unsafe {
        syscall6(
            syscall::DEBUG_LOG,
            msg.as_ptr() as u64,
            msg.len() as u64,
            0,
            0,
            0,
            0,
        )
    }
}

pub fn thread_exit(code: i64) -> ! {
    unsafe {
        let _ = syscall6(syscall::THREAD_EXIT, code as u64, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

pub fn task_exit(code: i64) -> ! {
    unsafe {
        let _ = syscall6(syscall::TASK_EXIT, code as u64, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

pub fn task_create(
    elf_vmo: Handle,
    offset: usize,
    len: usize,
    init_chan: Handle,
    extra_cap: Handle,
) -> Result<Handle, KnightStatus> {
    let r = unsafe {
        syscall6(
            syscall::TASK_CREATE,
            elf_vmo as u64,
            offset as u64,
            len as u64,
            init_chan as u64,
            extra_cap as u64,
            0,
        )
    };
    if r < 0 { Err(r) } else { Ok(r as Handle) }
}

/// Finds `(offset, size)` of a named ELF/blob inside an initrd KPKG [`Vmo`](Handle).
pub fn kpkg_find_in_vmo(vmo: Handle, name: &str) -> Option<(usize, usize)> {
    let mut hdr_bytes = [0u8; core::mem::size_of::<KpkgHeader>()];
    if vmo_read(vmo, 0, &mut hdr_bytes) != status::OK {
        return None;
    }
    let hdr = unsafe { core::ptr::read_unaligned(hdr_bytes.as_ptr() as *const KpkgHeader) };
    if hdr.magic != KPKG_MAGIC || hdr.entry_count > 64 {
        return None;
    }
    let mut ent_bytes = [0u8; core::mem::size_of::<KpkgEntry>()];
    for i in 0..hdr.entry_count as usize {
        let off = hdr_bytes.len() + i * ent_bytes.len();
        if vmo_read(vmo, off, &mut ent_bytes) != status::OK {
            return None;
        }
        let ent = unsafe { core::ptr::read_unaligned(ent_bytes.as_ptr() as *const KpkgEntry) };
        let nul = ent.name.iter().position(|&b| b == 0).unwrap_or(ent.name.len());
        if &ent.name[..nul] == name.as_bytes() {
            return Some((ent.offset as usize, ent.size as usize));
        }
    }
    None
}

pub fn thread_yield() {
    unsafe {
        let _ = syscall6(syscall::THREAD_YIELD, 0, 0, 0, 0, 0, 0);
    }
}

pub fn thread_create(entry: u64, stack_top: u64, arg: u64) -> Result<Handle, KnightStatus> {
    let r = unsafe { syscall6(syscall::THREAD_CREATE, entry, stack_top, arg, 0, 0, 0) };
    if r < 0 { Err(r) } else { Ok(r as Handle) }
}

pub fn handle_close(h: Handle) -> KnightStatus {
    unsafe { syscall6(syscall::HANDLE_CLOSE, h as u64, 0, 0, 0, 0, 0) }
}

pub fn handle_duplicate(h: Handle, rights: Rights) -> Result<Handle, KnightStatus> {
    let r = unsafe { syscall6(syscall::HANDLE_DUPLICATE, h as u64, rights.0 as u64, 0, 0, 0, 0) };
    if r < 0 { Err(r) } else { Ok(r as Handle) }
}

pub fn handle_info(h: Handle) -> Result<Rights, KnightStatus> {
    let r = unsafe { syscall6(syscall::HANDLE_INFO, h as u64, 0, 0, 0, 0, 0) };
    if r < 0 { Err(r) } else { Ok(Rights(r as u32)) }
}

pub fn vmo_create(size: usize) -> Result<Handle, KnightStatus> {
    let r = unsafe { syscall6(syscall::VMO_CREATE, size as u64, 0, 0, 0, 0, 0) };
    if r < 0 { Err(r) } else { Ok(r as Handle) }
}

pub fn vmo_create_dma(hw_root: Handle, size: usize) -> Result<(Handle, u64), KnightStatus> {
    let mut phys_out: u64 = 0;
    let r = unsafe {
        syscall6(
            syscall::VMO_CREATE_DMA,
            hw_root as u64,
            size as u64,
            (&mut phys_out as *mut u64) as u64,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok((r as Handle, phys_out))
    }
}

pub fn mmio_map(hw_root: Handle, phys_base: u64, len: usize) -> Result<u64, KnightStatus> {
    let mut out_vaddr: u64 = 0;
    let r = unsafe {
        syscall6(
            syscall::MMIO_MAP,
            hw_root as u64,
            phys_base,
            len as u64,
            (&mut out_vaddr as *mut u64) as u64,
            0,
            0,
        )
    };
    if r < 0 { Err(r) } else { Ok(out_vaddr) }
}

pub fn pci_ecam_info(hw_root: Handle) -> Result<(u64, u64), KnightStatus> {
    let mut out: [u64; 2] = [0; 2]; // [ecam_phys, irq_ctrl_phys]
    let r = unsafe {
        syscall6(
            syscall::PCI_ECAM_INFO,
            hw_root as u64,
            out.as_mut_ptr() as u64,
            0,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok((out[0], out[1]))
    }
}

pub fn fb_info(hw_root: Handle) -> Result<FbInfo, KnightStatus> {
    let mut out = FbInfo::default();
    let r = unsafe {
        syscall6(
            syscall::FB_INFO,
            hw_root as u64,
            (&mut out as *mut FbInfo) as u64,
            0,
            0,
            0,
            0,
        )
    };
    if r < 0 { Err(r) } else { Ok(out) }
}

pub fn vmo_write(h: Handle, offset: usize, data: &[u8]) -> KnightStatus {
    unsafe {
        syscall6(
            syscall::VMO_WRITE,
            h as u64,
            offset as u64,
            data.as_ptr() as u64,
            data.len() as u64,
            0,
            0,
        )
    }
}

pub fn vmo_read(h: Handle, offset: usize, out: &mut [u8]) -> KnightStatus {
    unsafe {
        syscall6(
            syscall::VMO_READ,
            h as u64,
            offset as u64,
            out.as_mut_ptr() as u64,
            out.len() as u64,
            0,
            0,
        )
    }
}

pub fn vmar_map(vmo: Handle, vmo_offset: usize, len: usize, flags: u32) -> Result<u64, KnightStatus> {
    let mut out_vaddr: u64 = 0;
    let r = unsafe {
        syscall6(
            syscall::VMAR_MAP,
            vmo as u64,
            vmo_offset as u64,
            len as u64,
            flags as u64,
            (&mut out_vaddr as *mut u64) as u64,
            0,
        )
    };
    if r < 0 { Err(r) } else { Ok(out_vaddr) }
}

pub fn vmar_unmap(vaddr: u64, len: usize) -> KnightStatus {
    unsafe { syscall6(syscall::VMAR_UNMAP, vaddr, len as u64, 0, 0, 0, 0) }
}

pub fn channel_create() -> Result<(Handle, Handle), KnightStatus> {
    let mut ends: [Handle; 2] = [0; 2];
    let r = unsafe { syscall6(syscall::CHANNEL_CREATE, ends.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok((ends[0], ends[1]))
    }
}

pub fn channel_write(ch: Handle, data: &[u8], handles: &[Handle]) -> KnightStatus {
    unsafe {
        syscall6(
            syscall::CHANNEL_WRITE,
            ch as u64,
            data.as_ptr() as u64,
            data.len() as u64,
            handles.as_ptr() as u64,
            handles.len() as u64,
            0,
        )
    }
}

pub fn channel_read(
    ch: Handle,
    data_out: &mut [u8],
    handles_out: &mut [Handle],
) -> Result<(usize, usize), KnightStatus> {
    let mut actual_bytes: u64 = 0;
    let mut actual_handles: u64 = 0;
    let r = unsafe {
        syscall6(
            syscall::CHANNEL_READ,
            ch as u64,
            data_out.as_mut_ptr() as u64,
            ((data_out.len() as u64) << 32) | (handles_out.len() as u64 & 0xFFFF_FFFF),
            handles_out.as_mut_ptr() as u64,
            (&mut actual_bytes as *mut u64) as u64,
            (&mut actual_handles as *mut u64) as u64,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok((actual_bytes as usize, actual_handles as usize))
    }
}

pub fn port_create() -> Result<Handle, KnightStatus> {
    let r = unsafe { syscall6(syscall::PORT_CREATE, 0, 0, 0, 0, 0, 0) };
    if r < 0 { Err(r) } else { Ok(r as Handle) }
}

pub fn port_queue(port: Handle, pkt: &PortPacket) -> KnightStatus {
    unsafe { syscall6(syscall::PORT_QUEUE, port as u64, (pkt as *const PortPacket) as u64, 0, 0, 0, 0) }
}

pub fn port_wait(port: Handle, pkt: &mut PortPacket) -> KnightStatus {
    unsafe { syscall6(syscall::PORT_WAIT, port as u64, (pkt as *mut PortPacket) as u64, 0, 0, 0, 0) }
}

pub fn futex_wait(addr: *const core::sync::atomic::AtomicU32, expected: u32) -> KnightStatus {
    unsafe { syscall6(syscall::FUTEX_WAIT, addr as u64, expected as u64, 0, 0, 0, 0) }
}

pub fn futex_wake(addr: *const core::sync::atomic::AtomicU32, count: u32) -> KnightStatus {
    unsafe { syscall6(syscall::FUTEX_WAKE, addr as u64, count as u64, 0, 0, 0, 0) }
}

pub fn clock_monotonic() -> u64 {
    unsafe { syscall6(syscall::CLOCK_MONOTONIC, 0, 0, 0, 0, 0, 0) as u64 }
}

pub fn sys_info(out: &mut SysInfo) -> KnightStatus {
    unsafe { syscall6(syscall::SYS_INFO, (out as *mut SysInfo) as u64, 0, 0, 0, 0, 0) }
}

struct LogBuf {
    buf: [u8; 256],
    len: usize,
}

impl Write for LogBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                if let Ok(text) = core::str::from_utf8(&self.buf[..self.len]) {
                    let _ = debug_log(text);
                }
                self.len = 0;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}

pub fn _log(args: fmt::Arguments<'_>) {
    let mut lb = LogBuf {
        buf: [0; 256],
        len: 0,
    };
    let _ = lb.write_fmt(args);
    if lb.len > 0
        && let Ok(text) = core::str::from_utf8(&lb.buf[..lb.len])
    {
        let _ = debug_log(text);
    }
}

#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {{
        $crate::_log(core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! klogln {
    () => {
        $crate::klog!("\n")
    };
    ($($arg:tt)*) => {{
        $crate::_log(core::format_args!($($arg)*));
        $crate::klog!("\n");
    }};
}
