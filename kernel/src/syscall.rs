//! KnightOS Syscall Dispatcher enforcing capability rights and user pointer validation.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use knight_abi::{
    FbInfo, HANDLE_INVALID, Handle, KnightStatus, PortPacket, Rights, SysInfo, packet_kind,
    status, syscall, vmar_flags,
};
use knightos_hal::{MmuArch, PAGE_SIZE_4K};
use knightos_ipc::{ChannelEndpoint, ChannelMessage, MAX_CHANNEL_BYTES, MAX_CHANNEL_HANDLES, Port, TransferredCap};
use knightos_mm::vmar::Vmar;
use knightos_mm::vmo::Vmo;
use knightos_sched::SchedClass;

use crate::arch_select::{ActiveMmu, online_cpus, timer_ticks};
use crate::task::{
    FUTEX_TABLE, KernelObject, Task, ThreadState, arch_id, block_current, current_task,
    current_thread, exit_current_task, exit_current_thread, get_fb_info, hhdm_offset,
    irq_ctrl_phys, pcie_ecam_phys, pmm, spawn_user_thread, wake_thread, yield_current,
};
use crate::{kprint, kprintln};

pub extern "C" fn dispatch_syscall(
    num: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> i64 {
    match num {
        syscall::THREAD_EXIT => exit_current_thread(),
        syscall::TASK_EXIT => exit_current_task(),
        syscall::THREAD_YIELD => {
            yield_current();
            status::OK
        }
        syscall::THREAD_CREATE => sys_thread_create(a0, a1, a2),
        syscall::TASK_CREATE => sys_task_create(a0 as Handle, a1 as usize, a2 as usize, a3 as Handle, a4 as Handle),
        syscall::HANDLE_CLOSE => sys_handle_close(a0 as Handle),
        syscall::HANDLE_DUPLICATE => sys_handle_duplicate(a0 as Handle, Rights(a1 as u32)),
        syscall::HANDLE_INFO => sys_handle_info(a0 as Handle),
        syscall::VMO_CREATE => sys_vmo_create(a0 as usize),
        syscall::VMO_READ => sys_vmo_rw(a0 as Handle, a1 as usize, a2, a3 as usize, false),
        syscall::VMO_WRITE => sys_vmo_rw(a0 as Handle, a1 as usize, a2, a3 as usize, true),
        syscall::VMO_CLONE => sys_vmo_clone(a0 as Handle),
        syscall::VMAR_MAP => sys_vmar_map(a0 as Handle, a1 as usize, a2 as usize, a3 as u32, a4),
        syscall::VMAR_UNMAP => sys_vmar_unmap(a0, a1 as usize),
        syscall::CHANNEL_CREATE => sys_channel_create(a0),
        syscall::CHANNEL_WRITE => sys_channel_write(a0 as Handle, a1, a2 as usize, a3, a4 as usize),
        syscall::CHANNEL_READ => sys_channel_read(a0 as Handle, a1, a2, a3, a4, a5),
        syscall::PORT_CREATE => sys_port_create(),
        syscall::PORT_QUEUE => sys_port_queue(a0 as Handle, a1),
        syscall::PORT_WAIT => sys_port_wait(a0 as Handle, a1),
        syscall::FUTEX_WAIT => sys_futex_wait(a0, a1 as u32),
        syscall::FUTEX_WAKE => sys_futex_wake(a0, a1 as u32),
        syscall::CLOCK_MONOTONIC => (timer_ticks() * 10_000_000) as i64,
        syscall::DEBUG_LOG => sys_debug_log(a0, a1 as usize),
        syscall::SYS_INFO => sys_sys_info(a0),
        syscall::VMO_CREATE_DMA => sys_vmo_create_dma(a0 as Handle, a1 as usize, a2),
        syscall::MMIO_MAP => sys_mmio_map(a0 as Handle, a1, a2 as usize, a3),
        syscall::PCI_ECAM_INFO => sys_pci_ecam_info(a0 as Handle, a1),
        syscall::FB_INFO => sys_fb_info(a0 as Handle, a1),
        _ => status::ERR_INVALID_ARG,
    }
}

fn validate_user(ptr: u64, len: usize, write: bool) -> Result<(), KnightStatus> {
    let task = current_task().ok_or(status::ERR_INTERNAL)?;
    if task.vmar.lock().validate_user_range(ptr, len, write) {
        Ok(())
    } else {
        Err(status::ERR_INVALID_ARG)
    }
}

fn sys_debug_log(ptr: u64, len: usize) -> KnightStatus {
    if len > 4096 {
        return status::ERR_INVALID_ARG;
    }
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let copy_vec = {
        let vmar = task.vmar.lock();
        if !vmar.validate_user_range(ptr, len, false) {
            return status::ERR_INVALID_ARG;
        }
        unsafe { core::slice::from_raw_parts(ptr as *const u8, len) }.to_vec()
    };
    if let Ok(text) = core::str::from_utf8(&copy_vec) {
        kprint!("{}", text);
        status::OK
    } else {
        status::ERR_INVALID_ARG
    }
}

fn sys_thread_create(entry: u64, user_sp: u64, arg: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    if !task.vmar.lock().validate_user_range(entry, 4, false)
        || !task.vmar.lock().validate_user_range(user_sp.saturating_sub(16), 16, true)
    {
        return status::ERR_INVALID_ARG;
    }
    let Ok(th) = spawn_user_thread(task.clone(), entry, user_sp, arg, SchedClass::Interactive) else {
        return status::ERR_NO_MEMORY;
    };
    match task.insert_handle(KernelObject::Thread(th), Rights::BASIC | Rights::MANAGE) {
        Ok(h) => h as i64,
        Err(e) => e,
    }
}

fn sys_handle_close(h: Handle) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    task.cnode.lock().close(h)
}

fn sys_handle_duplicate(h: Handle, mask: Rights) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    match task.cnode.lock().duplicate(h, mask) {
        Ok(new_h) => new_h as i64,
        Err(e) => e,
    }
}

fn sys_handle_info(h: Handle) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    match task.cnode.lock().rights(h) {
        Ok(r) => r.0 as i64,
        Err(e) => e,
    }
}

fn sys_vmo_create(size: usize) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let Some(vmo) = Vmo::create_anonymous(size, pmm(), hhdm_offset()) else {
        return status::ERR_NO_MEMORY;
    };
    match task.insert_handle(KernelObject::Vmo(vmo), Rights::DEFAULT_VMO) {
        Ok(h) => h as i64,
        Err(e) => e,
    }
}

fn sys_vmo_clone(h: Handle) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (obj, rights) = match task.cnode.lock().get(h, Rights::READ | Rights::DUPLICATE) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Vmo(vmo) = obj else {
        return status::ERR_BAD_HANDLE;
    };
    let Some(cloned) = vmo.clone_snapshot() else {
        return status::ERR_NO_MEMORY;
    };
    match task.insert_handle(KernelObject::Vmo(cloned), rights) {
        Ok(new_h) => new_h as i64,
        Err(e) => e,
    }
}

fn sys_vmo_rw(h: Handle, offset: usize, buf_ptr: u64, len: usize, write: bool) -> KnightStatus {
    if len == 0 {
        return status::OK;
    }
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let req = if write { Rights::WRITE } else { Rights::READ };
    let (obj, _) = match task.cnode.lock().get(h, req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Vmo(vmo) = obj else {
        return status::ERR_BAD_HANDLE;
    };
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(buf_ptr, len, !write) {
        return status::ERR_INVALID_ARG;
    }
    if write {
        let src = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len) };
        if vmo.write(offset, src).is_ok() {
            status::OK
        } else {
            status::ERR_INVALID_ARG
        }
    } else {
        let dst = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len) };
        if vmo.read(offset, dst).is_ok() {
            status::OK
        } else {
            status::ERR_INVALID_ARG
        }
    }
}

fn sys_vmar_map(
    vmo_h: Handle,
    vmo_offset: usize,
    len: usize,
    flags_bits: u32,
    out_vaddr_ptr: u64,
) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let writable = (flags_bits & vmar_flags::WRITE) != 0;
    let executable = (flags_bits & vmar_flags::EXEC) != 0;
    if writable && executable {
        return status::ERR_WX_VIOLATION;
    }
    let mut req = Rights::MAP | Rights::READ;
    if writable {
        req = req.union(Rights::WRITE);
    }
    if executable {
        req = req.union(Rights::EXEC);
    }

    let (obj, _) = match task.cnode.lock().get(vmo_h, req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Vmo(vmo) = obj else {
        return status::ERR_BAD_HANDLE;
    };
    let Ok(page_flags) = Vmar::user_page_flags(writable, executable) else {
        return status::ERR_WX_VIOLATION;
    };
    let mut alloc = knightos_mm::pmm::PmmRef(pmm());
    let mut vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_vaddr_ptr, core::mem::size_of::<u64>(), true) {
        return status::ERR_INVALID_ARG;
    }
    let mapped = match vmar.map_vmo_auto::<ActiveMmu>(vmo, vmo_offset, len, page_flags, &mut alloc) {
        Ok(va) => va,
        Err(_) => return status::ERR_INVALID_ARG,
    };
    unsafe {
        core::ptr::write(out_vaddr_ptr as *mut u64, mapped);
    }
    status::OK
}

fn sys_vmar_unmap(vaddr: u64, len: usize) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    if task.vmar.lock().unmap::<ActiveMmu>(vaddr, len).is_ok() {
        ActiveMmu::flush_tlb_all();
        status::OK
    } else {
        status::ERR_INVALID_ARG
    }
}

fn sys_channel_create(out_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    if validate_user(out_ptr, core::mem::size_of::<[Handle; 2]>(), true).is_err() {
        return status::ERR_INVALID_ARG;
    }
    let (ep0, ep1) = ChannelEndpoint::<KernelObject>::create_pair();
    let Ok(h0) = task.insert_handle(KernelObject::Channel(ep0), Rights::DEFAULT_CHANNEL) else {
        return status::ERR_NO_MEMORY;
    };
    let Ok(h1) = task.insert_handle(KernelObject::Channel(ep1), Rights::DEFAULT_CHANNEL) else {
        let _ = task.cnode.lock().close(h0);
        return status::ERR_NO_MEMORY;
    };
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_ptr, core::mem::size_of::<[Handle; 2]>(), true) {
        drop(vmar);
        let mut cnode = task.cnode.lock();
        let _ = cnode.close(h0);
        let _ = cnode.close(h1);
        return status::ERR_INVALID_ARG;
    }
    unsafe {
        core::ptr::write(out_ptr as *mut [Handle; 2], [h0, h1]);
    }
    status::OK
}

fn sys_channel_write(
    ch_h: Handle,
    data_ptr: u64,
    data_len: usize,
    handles_ptr: u64,
    handles_len: usize,
) -> KnightStatus {
    if data_len > MAX_CHANNEL_BYTES || handles_len > MAX_CHANNEL_HANDLES {
        return status::ERR_INVALID_ARG;
    }
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };

    let (obj, _) = match task.cnode.lock().get(ch_h, Rights::WRITE) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Channel(ep) = obj else {
        return status::ERR_BAD_HANDLE;
    };

    let (bytes, in_handles) = {
        let vmar = task.vmar.lock();
        if data_len > 0 && !vmar.validate_user_range(data_ptr, data_len, false) {
            return status::ERR_INVALID_ARG;
        }
        if handles_len > 0
            && !vmar.validate_user_range(
                handles_ptr,
                handles_len * core::mem::size_of::<Handle>(),
                false,
            )
        {
            return status::ERR_INVALID_ARG;
        }
        let b = if data_len > 0 {
            unsafe { core::slice::from_raw_parts(data_ptr as *const u8, data_len) }.to_vec()
        } else {
            Vec::new()
        };
        let hs = if handles_len > 0 {
            unsafe { core::slice::from_raw_parts(handles_ptr as *const Handle, handles_len) }
                .to_vec()
        } else {
            Vec::new()
        };
        (b, hs)
    };

    let mut caps: Vec<TransferredCap<KernelObject>> = Vec::with_capacity(in_handles.len());
    if !in_handles.is_empty() {
        let mut cnode = task.cnode.lock();
        for &h in &in_handles {
            if h == ch_h {
                for cap in caps {
                    let _ = cnode.insert(cap.object, cap.rights);
                }
                return status::ERR_INVALID_ARG;
            }
            match cnode.take(h, Rights::TRANSFER) {
                Ok((obj, rights)) => caps.push(TransferredCap { object: obj, rights }),
                Err(e) => {
                    // Re-insert any already-taken handles back into caller CNode on error.
                    for cap in caps {
                        let _ = cnode.insert(cap.object, cap.rights);
                    }
                    return e;
                }
            }
        }
    }

    match ep.write(ChannelMessage { bytes, caps }) {
        Ok(maybe_waiter) => {
            if let Some(tid) = maybe_waiter {
                wake_thread(tid);
            }
            status::OK
        }
        Err(e) => e,
    }
}

fn sys_channel_read(
    ch_h: Handle,
    data_out_ptr: u64,
    packed_caps: u64,
    handles_out_ptr: u64,
    actual_bytes_ptr: u64,
    actual_handles_ptr: u64,
) -> KnightStatus {
    let max_bytes = (packed_caps >> 32) as usize;
    let max_handles = (packed_caps & 0xFFFF_FFFF) as usize;
    if max_bytes > 0 && validate_user(data_out_ptr, max_bytes, true).is_err() {
        return status::ERR_INVALID_ARG;
    }
    if max_handles > 0
        && validate_user(
            handles_out_ptr,
            max_handles * core::mem::size_of::<Handle>(),
            true,
        )
        .is_err()
    {
        return status::ERR_INVALID_ARG;
    }
    if validate_user(actual_bytes_ptr, core::mem::size_of::<u64>(), true).is_err()
        || validate_user(actual_handles_ptr, core::mem::size_of::<u64>(), true).is_err()
    {
        return status::ERR_INVALID_ARG;
    }

    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let Some(thread) = current_thread() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(ch_h, Rights::READ) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Channel(ep) = obj else {
        return status::ERR_BAD_HANDLE;
    };

    let write_actual = |nb: usize, nh: usize| -> Result<(), KnightStatus> {
        let vmar = task.vmar.lock();
        if !vmar.validate_user_range(actual_bytes_ptr, core::mem::size_of::<u64>(), true)
            || !vmar.validate_user_range(actual_handles_ptr, core::mem::size_of::<u64>(), true)
        {
            return Err(status::ERR_INVALID_ARG);
        }
        unsafe {
            core::ptr::write(actual_bytes_ptr as *mut u64, nb as u64);
            core::ptr::write(actual_handles_ptr as *mut u64, nh as u64);
        }
        Ok(())
    };

    let msg = loop {
        match ep.try_read_checked(max_bytes, max_handles) {
            Ok(m) => break m,
            Err((status::ERR_SHOULD_WAIT, _, _)) => {
                ep.register_waiter(thread.id);
                // Re-check once after registering waiter to prevent missed wakeup.
                match ep.try_read_checked(max_bytes, max_handles) {
                    Ok(m) => break m,
                    Err((status::ERR_SHOULD_WAIT, _, _)) => block_current(),
                    Err((status::ERR_BUFFER_TOO_SMALL, nb, nh)) => {
                        let _ = write_actual(nb, nh);
                        return status::ERR_BUFFER_TOO_SMALL;
                    }
                    Err((e, _, _)) => return e,
                }
            }
            Err((status::ERR_BUFFER_TOO_SMALL, nb, nh)) => {
                let _ = write_actual(nb, nh);
                return status::ERR_BUFFER_TOO_SMALL;
            }
            Err((e, _, _)) => return e,
        }
    };

    let nbytes = msg.bytes.len();
    let nhandles = msg.caps.len();
    let mut inserted_handles = Vec::with_capacity(nhandles);
    if nhandles > 0 {
        let mut cnode = task.cnode.lock();
        for cap in msg.caps {
            inserted_handles.push(cnode.insert(cap.object, cap.rights).unwrap_or(0));
        }
    }

    let vmar = task.vmar.lock();
    if (nbytes > 0 && !vmar.validate_user_range(data_out_ptr, nbytes, true))
        || (nhandles > 0
            && !vmar.validate_user_range(
                handles_out_ptr,
                nhandles * core::mem::size_of::<Handle>(),
                true,
            ))
        || !vmar.validate_user_range(actual_bytes_ptr, core::mem::size_of::<u64>(), true)
        || !vmar.validate_user_range(actual_handles_ptr, core::mem::size_of::<u64>(), true)
    {
        drop(vmar);
        if !inserted_handles.is_empty() {
            let mut cnode = task.cnode.lock();
            for h in inserted_handles {
                if h != 0 {
                    let _ = cnode.close(h);
                }
            }
        }
        return status::ERR_INVALID_ARG;
    }

    if nbytes > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(msg.bytes.as_ptr(), data_out_ptr as *mut u8, nbytes);
        }
    }
    if nhandles > 0 {
        let out_handles =
            unsafe { core::slice::from_raw_parts_mut(handles_out_ptr as *mut Handle, nhandles) };
        out_handles.copy_from_slice(&inserted_handles);
    }
    unsafe {
        core::ptr::write(actual_bytes_ptr as *mut u64, nbytes as u64);
        core::ptr::write(actual_handles_ptr as *mut u64, nhandles as u64);
    }
    status::OK
}

fn sys_port_create() -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let port = Port::new();
    match task.insert_handle(KernelObject::Port(port), Rights::DEFAULT_PORT) {
        Ok(h) => h as i64,
        Err(e) => e,
    }
}

fn sys_port_queue(port_h: Handle, pkt_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(port_h, Rights::WRITE) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Port(port) = obj else {
        return status::ERR_BAD_HANDLE;
    };
    let mut pkt = {
        let vmar = task.vmar.lock();
        if !vmar.validate_user_range(pkt_ptr, core::mem::size_of::<PortPacket>(), false) {
            return status::ERR_INVALID_ARG;
        }
        unsafe { core::ptr::read(pkt_ptr as *const PortPacket) }
    };
    pkt.kind = packet_kind::USER;
    if let Some(tid) = port.queue(pkt) {
        wake_thread(tid);
    }
    status::OK
}

fn sys_port_wait(port_h: Handle, out_pkt_ptr: u64) -> KnightStatus {
    if validate_user(out_pkt_ptr, core::mem::size_of::<PortPacket>(), true).is_err() {
        return status::ERR_INVALID_ARG;
    }
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let Some(thread) = current_thread() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(port_h, Rights::READ) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Port(port) = obj else {
        return status::ERR_BAD_HANDLE;
    };

    let pkt = loop {
        if let Some(p) = port.try_pop() {
            break p;
        }
        port.register_waiter(thread.id);
        if let Some(p) = port.try_pop() {
            break p;
        }
        block_current();
    };
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_pkt_ptr, core::mem::size_of::<PortPacket>(), true) {
        return status::ERR_INVALID_ARG;
    }
    unsafe {
        core::ptr::write(out_pkt_ptr as *mut PortPacket, pkt);
    }
    status::OK
}

fn sys_futex_wait(addr: u64, expected: u32) -> KnightStatus {
    if (addr & 3) != 0 || validate_user(addr, 4, false).is_err() {
        return status::ERR_INVALID_ARG;
    }
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let Some(thread) = current_thread() else {
        return status::ERR_INTERNAL;
    };
    let atom = unsafe { &*(addr as *const AtomicU32) };
    if atom.load(Ordering::Acquire) != expected {
        return status::ERR_SHOULD_WAIT;
    }
    FUTEX_TABLE.enqueue(task.id, addr, thread.id);
    if atom.load(Ordering::Acquire) != expected {
        // Value changed before enqueue completed; wake any queued self or proceed cleanly.
        let _ = FUTEX_TABLE.wake(task.id, addr, 1);
        return status::OK;
    }
    block_current();
    status::OK
}

fn sys_futex_wake(addr: u64, count: u32) -> KnightStatus {
    if (addr & 3) != 0 || validate_user(addr, 4, false).is_err() {
        return status::ERR_INVALID_ARG;
    }
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let woken = FUTEX_TABLE.wake(task.id, addr, count as usize);
    let woken_count = woken.len() as i64;
    for tid in woken {
        wake_thread(tid);
    }
    woken_count
}

fn sys_sys_info(out_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let st = pmm().stats();
    let info = SysInfo {
        arch_id: arch_id(),
        online_cpus: online_cpus() as u32,
        total_usable_bytes: st.total_usable_bytes,
        free_bytes: st.free_bytes,
    };
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_ptr, core::mem::size_of::<SysInfo>(), true) {
        return status::ERR_INVALID_ARG;
    }
    unsafe {
        core::ptr::write(out_ptr as *mut SysInfo, info);
    }
    status::OK
}

fn sys_vmo_create_dma(hw_root: Handle, size: usize, out_phys_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(hw_root, Rights::WRITE | Rights::MAP) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::HardwareRoot = obj else {
        return status::ERR_BAD_HANDLE;
    };
    if size == 0 || size > 4 * 1024 * 1024 {
        return status::ERR_INVALID_ARG;
    }
    let Some((vmo, phys_base)) = Vmo::create_contiguous_dma(size, pmm(), hhdm_offset()) else {
        return status::ERR_NO_MEMORY;
    };
    let Ok(h) = task.insert_handle(KernelObject::Vmo(vmo), Rights::DEFAULT_VMO) else {
        return status::ERR_NO_MEMORY;
    };
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_phys_ptr, core::mem::size_of::<u64>(), true) {
        drop(vmar);
        let _ = task.cnode.lock().close(h);
        return status::ERR_INVALID_ARG;
    }
    unsafe {
        core::ptr::write(out_phys_ptr as *mut u64, phys_base);
    }
    h as i64
}

fn sys_mmio_map(hw_root: Handle, phys_base: u64, len: usize, out_vaddr_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(hw_root, Rights::MAP) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::HardwareRoot = obj else {
        return status::ERR_BAD_HANDLE;
    };
    if (phys_base & (PAGE_SIZE_4K as u64 - 1)) != 0 || len == 0 || len > 256 * 1024 * 1024 {
        return status::ERR_INVALID_ARG;
    }
    let mut alloc = knightos_mm::pmm::PmmRef(pmm());
    let mut vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_vaddr_ptr, core::mem::size_of::<u64>(), true) {
        return status::ERR_INVALID_ARG;
    }
    let mapped = match vmar.map_mmio_auto::<ActiveMmu>(phys_base, len, &mut alloc) {
        Ok(va) => va,
        Err(_) => return status::ERR_INVALID_ARG,
    };
    unsafe {
        core::ptr::write(out_vaddr_ptr as *mut u64, mapped);
    }
    status::OK
}

fn sys_pci_ecam_info(hw_root: Handle, out_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(hw_root, Rights::READ) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::HardwareRoot = obj else {
        return status::ERR_BAD_HANDLE;
    };
    let info = [pcie_ecam_phys(), irq_ctrl_phys()];
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_ptr, core::mem::size_of::<[u64; 2]>(), true) {
        return status::ERR_INVALID_ARG;
    }
    unsafe {
        core::ptr::write(out_ptr as *mut [u64; 2], info);
    }
    status::OK
}

fn sys_fb_info(hw_root: Handle, out_ptr: u64) -> KnightStatus {
    let Some(task) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (obj, _) = match task.cnode.lock().get(hw_root, Rights::READ) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::HardwareRoot = obj else {
        return status::ERR_BAD_HANDLE;
    };
    let info = get_fb_info();
    let vmar = task.vmar.lock();
    if !vmar.validate_user_range(out_ptr, core::mem::size_of::<FbInfo>(), true) {
        return status::ERR_INVALID_ARG;
    }
    unsafe {
        core::ptr::write(out_ptr as *mut FbInfo, info);
    }
    status::OK
}

fn sys_task_create(
    elf_vmo_h: Handle,
    offset: usize,
    len: usize,
    init_chan_h: Handle,
    extra_cap_h: Handle,
) -> KnightStatus {
    if len < 64 || len > 32 * 1024 * 1024 {
        return status::ERR_INVALID_ARG;
    }
    let Some(parent) = current_task() else {
        return status::ERR_INTERNAL;
    };
    let (vmo_obj, _) = match parent.cnode.lock().get(elf_vmo_h, Rights::READ) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let KernelObject::Vmo(elf_vmo) = vmo_obj else {
        return status::ERR_BAD_HANDLE;
    };

    let mut elf_bytes = alloc::vec![0u8; len];
    if elf_vmo.read(offset, &mut elf_bytes).is_err() {
        return status::ERR_INVALID_ARG;
    }

    let Ok(child_task) = Task::create_user() else {
        return status::ERR_NO_MEMORY;
    };
    let mut alloc = knightos_mm::pmm::PmmRef(pmm());
    let loaded = {
        let mut child_vmar = child_task.vmar.lock();
        match crate::elf_user::load_user_elf::<ActiveMmu>(
            &elf_bytes,
            &mut child_vmar,
            pmm(),
            hhdm_offset(),
            &mut alloc,
        ) {
            Ok(l) => l,
            Err(_) => return status::ERR_INVALID_ARG,
        }
    };

    let mut child_chan = HANDLE_INVALID;
    let mut child_extra = HANDLE_INVALID;
    let mut taken_chan: Option<(KernelObject, Rights)> = None;
    let mut taken_extra: Option<(KernelObject, Rights)> = None;

    {
        let mut parent_cnode = parent.cnode.lock();
        if init_chan_h != HANDLE_INVALID {
            match parent_cnode.take(init_chan_h, Rights::TRANSFER) {
                Ok(cap) => taken_chan = Some(cap),
                Err(e) => return e,
            }
        }
        if extra_cap_h != HANDLE_INVALID {
            match parent_cnode.take(extra_cap_h, Rights::TRANSFER) {
                Ok(cap) => taken_extra = Some(cap),
                Err(e) => {
                    if let Some((o, r)) = taken_chan {
                        let _ = parent_cnode.insert(o, r);
                    }
                    return e;
                }
            }
        }
    }

    if let Some((obj, rights)) = taken_chan {
        child_chan = match child_task.insert_handle(obj, rights) {
            Ok(h) => h,
            Err(e) => return e,
        };
    }
    if let Some((obj, rights)) = taken_extra {
        child_extra = match child_task.insert_handle(obj, rights) {
            Ok(h) => h,
            Err(e) => return e,
        };
    }

    let Ok(task_handle) = parent.insert_handle(
        KernelObject::Task(child_task.clone()),
        Rights::BASIC | Rights::WAIT | Rights::MANAGE,
    ) else {
        return status::ERR_NO_MEMORY;
    };

    let child_arg = (child_chan as u64) | ((child_extra as u64) << 32);
    if spawn_user_thread(
        child_task,
        loaded.entry_va,
        loaded.stack_top_va,
        child_arg,
        SchedClass::Interactive,
    )
    .is_err()
    {
        let _ = parent.cnode.lock().close(task_handle);
        return status::ERR_NO_MEMORY;
    }

    task_handle as i64
}

#[allow(dead_code)]
fn _unused_kprintln_check() {
    kprintln!("");
    let _ = (PAGE_SIZE_4K, ThreadState::Ready);
}
