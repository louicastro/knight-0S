#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
mod app {
    use knight_shell::{DesktopConfig, render_desktop_session};
    use knightos_devmgr::probe_virtio_gpu;
    use knightos_gpu_hal::{GpuFormat, LinearFbScanout, VirtioGpuScanout};
    use knightos_themes::GlassTheme;
    use libknight::{
        HANDLE_INVALID, Handle, SysInfo, channel_write, fb_info, klogln, mmio_map, status,
        sys_info, task_exit, vmar_flags, vmar_map, vmar_unmap, vmo_create,
    };

    #[panic_handler]
    fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
        klogln!("[shell PANIC] {}", info);
        task_exit(-1);
    }

    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text._start")]
    pub extern "C" fn _start(boot_arg: u64) -> ! {
        shell_main((boot_arg & 0xFFFF_FFFF) as Handle, (boot_arg >> 32) as Handle);
    }

    fn alloc_u32_buf(bytes: usize) -> (Handle, u64, &'static mut [u32]) {
        let vmo = vmo_create(bytes).expect("shell vmo_create");
        let va = vmar_map(vmo, 0, bytes, vmar_flags::READ | vmar_flags::WRITE)
            .expect("shell vmar_map");
        let slice = unsafe { core::slice::from_raw_parts_mut(va as *mut u32, bytes / 4) };
        (vmo, va, slice)
    }

    fn shell_main(ipc_chan: Handle, hw_root: Handle) -> ! {
        let mut sys = SysInfo::default();
        let _ = sys_info(&mut sys);
        let arch = match sys.arch_id {
            1 => "x86_64",
            2 => "aarch64",
            _ => "unknown",
        };
        let theme = GlassTheme::dark(1);
        let cfg = DesktopConfig {
            arch,
            cpus: sys.online_cpus,
            usable_mib: sys.total_usable_bytes / (1024 * 1024),
            iommu_enforced: true,
            theme,
        };

        let (v_mb, va_mb, mb_buf) = alloc_u32_buf(1024 * 32 * 4);
        let (v_dk, va_dk, dock_buf) = alloc_u32_buf(320 * 48 * 4);
        let (v_tm, va_tm, term_buf) = alloc_u32_buf(360 * 160 * 4);
        let (v_st, va_st, set_buf) = alloc_u32_buf(280 * 140 * 4);

        let mut frames = 0usize;
        let mut backend_tag = "offscreen";

        // Prefer PCIe VirtIO-GPU if present, then UEFI GOP linear framebuffer, else offscreen.
        if hw_root != HANDLE_INVALID && let Ok(vgpu) = probe_virtio_gpu(hw_root) {
            let mut scanout = VirtioGpuScanout::new(vgpu.gpu);
            let (_, _, back) = alloc_u32_buf(1280 * 768 * 4);
            let (_, f) = render_desktop_session(
                &mut scanout,
                back,
                mb_buf,
                dock_buf,
                term_buf,
                set_buf,
                &cfg,
            )
            .expect("virtio-gpu render");
            frames = f;
            backend_tag = "virtio-gpu";
        } else if hw_root != HANDLE_INVALID
            && let Ok(fb) = fb_info(hw_root)
            && fb.phys_base != 0
            && fb.width >= 320
            && fb.height >= 240
        {
            let w = fb.width.min(1024);
            let h = fb.height.min(768);
            let map_bytes = ((fb.pitch_bytes as usize) * (h as usize) + 4095) & !4095;
            let fb_va = mmio_map(hw_root, fb.phys_base, map_bytes).expect("mmio_map gop") as usize;
            let fmt = if fb.pixel_format == 1 {
                GpuFormat::Rgbx8888
            } else {
                GpuFormat::Bgrx8888
            };
            let mut scanout = LinearFbScanout::new(fb_va, w, h, fb.pitch_bytes, fmt);
            let (_, _, back) = alloc_u32_buf((w as usize) * (h as usize) * 4);
            let (_, f) = render_desktop_session(
                &mut scanout,
                back,
                mb_buf,
                dock_buf,
                term_buf,
                set_buf,
                &cfg,
            )
            .expect("gop render");
            frames = f;
            backend_tag = "gop";
        } else {
            let (v_vram, _, vram) = alloc_u32_buf(640 * 480 * 4);
            let (v_back, _, back) = alloc_u32_buf(640 * 480 * 4);
            let mut scanout =
                LinearFbScanout::new(vram.as_mut_ptr() as usize, 640, 480, 640 * 4, GpuFormat::Bgrx8888);
            let (_, f) = render_desktop_session(
                &mut scanout,
                back,
                mb_buf,
                dock_buf,
                term_buf,
                set_buf,
                &cfg,
            )
            .expect("offscreen render");
            frames = f;
            let _ = (v_vram, v_back);
        }

        let _ = (v_mb, va_mb, v_dk, va_dk, v_tm, va_tm, v_st, va_st, vmar_unmap);
        klogln!(
            "[shell] KnightCompositor + desktop rendered {} frames (backend={})",
            frames,
            backend_tag
        );
        if ipc_chan != HANDLE_INVALID {
            let _ = channel_write(ipc_chan, b"SHELL_OK", &[]);
        }
        let _ = status::OK;
        task_exit(0);
    }
}

#[cfg(not(target_os = "none"))]
fn main() {}
