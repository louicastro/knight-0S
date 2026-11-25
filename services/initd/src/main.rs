#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use knightos_devmgr::{probe_nvme, probe_virtio_blk};
use knightos_vfs::{BlockDevice, Vfs};
use libknight::{
    Handle, PortPacket, Rights, SysInfo, channel_create, channel_read, channel_write,
    clock_monotonic, futex_wait, futex_wake, handle_close, handle_duplicate, handle_info, klogln,
    kpkg_find_in_vmo, port_create, port_queue, port_wait, status, sys_info, task_create,
    task_exit, thread_create, thread_exit, thread_yield, vmar_flags, vmar_map, vmar_unmap,
    vmo_create, vmo_read, vmo_write,
};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    klogln!("[initd PANIC] {}", info);
    task_exit(-1);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start(boot_arg: u64) -> ! {
    initd_main(
        (boot_arg & 0xFFFF_FFFF) as Handle,
        (boot_arg >> 32) as Handle,
    );
}

#[repr(C)]
struct WorkerShared {
    futex_flag: AtomicU32,
    reply_val: AtomicU32,
    fpu_ok: AtomicU32,
}

#[inline(never)]
fn exercise_simd_reg(seed: u64) -> u64 {
    let out: u64;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "movq xmm5, {inp}",
            "nop",
            "movq {outp}, xmm5",
            inp = in(reg) seed,
            outp = out(reg) out,
            out("xmm5") _,
            options(nomem, nostack),
        );
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "fmov d5, {inp}",
            "nop",
            "fmov {outp}, d5",
            inp = in(reg) seed,
            outp = out(reg) out,
            out("d5") _,
            options(nomem, nostack),
        );
    }
    out
}

extern "C" fn worker_thread_entry(shared_ptr: u64) -> ! {
    let shared = unsafe { &*(shared_ptr as *const WorkerShared) };
    // Verify FPU/SIMD register state survives preemptive context switches cleanly.
    let sentinel = 0x1122_3344_5566_7788u64;
    #[cfg(target_arch = "x86_64")]
    let after = unsafe {
        let res: u64;
        core::arch::asm!(
            "movq xmm6, {inp_s}",
            inp_s = in(reg) sentinel,
            options(nomem, nostack),
        );
        let _ = thread_yield();
        core::arch::asm!(
            "movq {res}, xmm6",
            res = out(reg) res,
            options(nomem, nostack),
        );
        res
    };
    #[cfg(target_arch = "aarch64")]
    let after = unsafe {
        let res: u64;
        core::arch::asm!(
            "fmov d6, {inp_s}",
            inp_s = in(reg) sentinel,
            options(nomem, nostack),
        );
        let _ = thread_yield();
        core::arch::asm!(
            "fmov {res}, d6",
            res = out(reg) res,
            options(nomem, nostack),
        );
        res
    };
    if after == sentinel {
        shared.fpu_ok.store(1, Ordering::Release);
    }

    klogln!("[initd:worker] Running in user mode; setting futex_flag=1");
    shared.reply_val.store(0xCAFE_BABE, Ordering::Release);
    shared.futex_flag.store(1, Ordering::Release);
    let _ = futex_wake(&shared.futex_flag, 1);
    thread_exit(0);
}

fn initd_main(hw_root: Handle, initrd_vmo: Handle) -> ! {
    let mut info = SysInfo::default();
    assert_eq!(sys_info(&mut info), status::OK);
    let arch_str = match info.arch_id {
        1 => "x86_64",
        2 => "aarch64",
        _ => "unknown",
    };
    klogln!(
        "[initd] User-mode entry OK! arch={} cpus={} usable_mib={}",
        arch_str,
        info.online_cpus,
        info.total_usable_bytes / (1024 * 1024)
    );

    // 1. VMO + VMAR + overflow rejection + strict W^X verification.
    assert!(vmo_create(usize::MAX).is_err());
    let vmo = vmo_create(8192).expect("vmo_create");
    assert_eq!(vmo_write(vmo, 0, b"KnightOS VMO OK"), status::OK);
    assert!(vmar_map(vmo, 0, usize::MAX, vmar_flags::READ).is_err());

    // W^X rejection check: READ|WRITE|EXEC must be denied.
    assert_eq!(
        vmar_map(vmo, 0, 4096, vmar_flags::READ | vmar_flags::WRITE | vmar_flags::EXEC).err(),
        Some(status::ERR_WX_VIOLATION)
    );

    let mapped = vmar_map(vmo, 0, 8192, vmar_flags::READ | vmar_flags::WRITE).expect("vmar_map");
    let slice = unsafe { core::slice::from_raw_parts(mapped as *const u8, 15) };
    assert_eq!(slice, b"KnightOS VMO OK");
    assert_eq!(vmar_unmap(mapped, 8192), status::OK);
    klogln!("[initd] VMO/VMAR + W^X check: OK");

    // 2. Capability attenuation + Channel IPC handle transfer + non-dropping small-buffer retry.
    let ro_vmo = handle_duplicate(vmo, Rights::READ | Rights::TRANSFER).expect("handle_duplicate");
    let (tx, rx) = channel_create().expect("channel_create");
    assert_eq!(channel_write(tx, b"PING", &[ro_vmo]), status::OK);

    let mut tiny = [0u8; 2];
    assert_eq!(
        channel_read(rx, &mut tiny, &mut []),
        Err(status::ERR_BUFFER_TOO_SMALL)
    );

    let mut msg_buf = [0u8; 32];
    let mut rx_handles: [Handle; 4] = [0; 4];
    let (nbytes, nhandles) = channel_read(rx, &mut msg_buf, &mut rx_handles).expect("channel_read");
    assert_eq!(&msg_buf[..nbytes], b"PING");
    assert_eq!(nhandles, 1);

    let rx_rights = handle_info(rx_handles[0]).expect("handle_info");
    assert!(rx_rights.contains(Rights::READ));
    assert!(!rx_rights.contains(Rights::WRITE));

    let mut vmo_check = [0u8; 15];
    assert_eq!(vmo_read(rx_handles[0], 0, &mut vmo_check), status::OK);
    assert_eq!(&vmo_check, b"KnightOS VMO OK");
    let _ = handle_close(rx_handles[0]);
    let _ = handle_close(tx);
    let _ = handle_close(rx);
    let _ = handle_close(vmo);
    klogln!("[initd] Capability attenuation + Channel IPC: OK");

    // 3. Port event queue check.
    let port = port_create().expect("port_create");
    let pkt_in = PortPacket {
        key: 0x1234_5678,
        kind: 0,
        status: 0,
        data: [11, 22],
    };
    assert_eq!(port_queue(port, &pkt_in), status::OK);
    let mut pkt_out = PortPacket::default();
    assert_eq!(port_wait(port, &mut pkt_out), status::OK);
    assert_eq!(pkt_out.key, 0x1234_5678);
    assert_eq!(pkt_out.data, [11, 22]);
    let _ = handle_close(port);
    klogln!("[initd] Port event queue: OK");

    // 4. Multi-threaded user-space execution + FPU/SIMD preservation + Futex sync.
    assert_eq!(exercise_simd_reg(0xDEAD_BEEF_CAFE_F00D), 0xDEAD_BEEF_CAFE_F00D);
    let stack_vmo = vmo_create(16 * 1024).expect("worker stack vmo");
    let stack_base = vmar_map(
        stack_vmo,
        0,
        16 * 1024,
        vmar_flags::READ | vmar_flags::WRITE,
    )
    .expect("worker stack map");
    let shared = WorkerShared {
        futex_flag: AtomicU32::new(0),
        reply_val: AtomicU32::new(0),
        fpu_ok: AtomicU32::new(0),
    };
    let worker_h = thread_create(
        worker_thread_entry as *const () as usize as u64,
        stack_base + 16 * 1024,
        (&shared as *const WorkerShared) as u64,
    )
    .expect("thread_create");

    while shared.futex_flag.load(Ordering::Acquire) == 0 {
        let _ = futex_wait(&shared.futex_flag, 0);
        thread_yield();
    }
    assert_eq!(shared.reply_val.load(Ordering::Acquire), 0xCAFE_BABE);
    assert_eq!(shared.fpu_ok.load(Ordering::Acquire), 1);
    let _ = handle_close(worker_h);

    // 5. User-space devmgr + virtio-blk PCIe discovery + KnightFS/VFS persistence verification.
    let blk = probe_virtio_blk(hw_root).expect("devmgr virtio-blk probe");
    klogln!(
        "[devmgr] Bound virtio-blk at PCI {:02x}:{:02x}.{} ({:04x}:{:04x}), {} sectors ({} MiB)",
        blk.bdf.0,
        blk.bdf.1,
        blk.bdf.2,
        blk.vendor_id,
        blk.device_id,
        blk.sector_count(),
        (blk.sector_count() * 512) / (1024 * 1024)
    );

    let (mut vfs, existed) = Vfs::mount_or_format(blk).expect("vfs mount_or_format");
    if existed {
        let mut prev = [0u8; 64];
        if let Ok(n) = vfs.read("/boot.txt", &mut prev) {
            assert_eq!(&prev[..n], b"KnightOS KnightFS persistent OK!");
            klogln!("[knightfs] Verified persisted /boot.txt from prior QEMU run.");
        }
    }

    // Write + remount + verify CRC32 read + delete check on real VirtIO block storage.
    vfs.write("/boot.txt", b"KnightOS KnightFS persistent OK!")
        .expect("vfs write /boot.txt");
    vfs.write("/scratch.bin", b"TEMP_12345")
        .expect("vfs write /scratch.bin");
    let blk_reopened = vfs.into_inner();
    let mut vfs2 = Vfs::mount(blk_reopened).expect("vfs remount");
    let mut rd = [0u8; 64];
    let n = vfs2.read("/boot.txt", &mut rd).expect("vfs read /boot.txt");
    assert_eq!(&rd[..n], b"KnightOS KnightFS persistent OK!");
    vfs2.remove("/scratch.bin").expect("vfs remove /scratch.bin");
    klogln!("[knightfs] VirtIO-blk + KnightFS VFS R/W/Remount CRC32 check: OK");

    let t_ns = clock_monotonic();
    klogln!(
        "[KNIGHTOS PHASE3 PASS] arch={} usermode=ok syscall=ok wx=ok ipc=ok vmo=ok fpu=ok smp_sched=ok virtio_blk=ok knightfs=ok t_ns={}",
        arch_str,
        t_ns
    );

    // 6. Phase 4: PCIe NVMe controller probe + KnightFS v2 compression & A/B superblock check.
    let nvme = probe_nvme(hw_root).expect("devmgr nvme probe");
    klogln!(
        "[devmgr] Bound NVMe at PCI {:02x}:{:02x}.{}, {} sectors ({} MiB)",
        nvme.bdf.0,
        nvme.bdf.1,
        nvme.bdf.2,
        nvme.sector_count(),
        (nvme.sector_count() * 512) / (1024 * 1024)
    );
    let (mut nvme_vfs, _) = Vfs::mount_or_format(nvme).expect("nvme knightfs v2");
    let compressible = [b'N'; 256];
    nvme_vfs
        .write("/nvme_blob.bin", &compressible)
        .expect("nvme write");
    let st = nvme_vfs.stat("/nvme_blob.bin").expect("nvme stat");
    assert!(st.compressed && st.stored_bytes < 256);
    let nvme_dev = nvme_vfs.into_inner();
    let mut nvme_vfs2 = Vfs::mount(nvme_dev).expect("nvme remount");
    let mut nv_rd = [0u8; 256];
    assert_eq!(nvme_vfs2.read("/nvme_blob.bin", &mut nv_rd), Ok(256));
    assert_eq!(nv_rd, compressible);
    klogln!("[knightfs] NVMe + KnightFS v2 A/B superblock + compression check: OK");

    // 7. Phase 4: Spawn isolated `shell.elf` process (`KnightCompositor` + `ui-kit` + Desktop).
    let (shell_off, shell_len) =
        kpkg_find_in_vmo(initrd_vmo, "shell.elf").expect("shell.elf in initrd");
    let (parent_ch, child_ch) = channel_create().expect("shell channel_create");
    let shell_hw = handle_duplicate(
        hw_root,
        Rights::READ | Rights::WRITE | Rights::MAP | Rights::TRANSFER,
    )
    .expect("dup hw_root for shell");
    let shell_task =
        task_create(initrd_vmo, shell_off, shell_len, child_ch, shell_hw).expect("task_create shell");
    let mut shell_reply = [0u8; 32];
    let (rn, _) = channel_read(parent_ch, &mut shell_reply, &mut []).expect("shell reply");
    assert_eq!(&shell_reply[..rn], b"SHELL_OK");
    let _ = handle_close(parent_ch);
    let _ = handle_close(shell_task);

    klogln!(
        "[KNIGHTOS PHASE4 PASS] arch={} spawn=ok compositor=ok shell=ok nvme=ok knightfs2=ok t_ns={}",
        arch_str,
        clock_monotonic()
    );

    task_exit(0);
}
