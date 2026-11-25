#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(test, allow(dead_code))]

mod arch;
mod config;
mod console;
mod elf;
mod fs;
mod gop;
mod mmap;
mod paging;
mod uefi;

#[cfg(not(test))]
use core::ffi::c_void;
#[cfg(not(test))]
use core::ptr::null_mut;

#[cfg(not(test))]
use knightboot_protocol::{
    DEFAULT_HHDM_OFFSET, DEFAULT_KERNEL_VIRT_BASE, KbipBootInfo, KbipMemoryKind, KbipMemoryRegion,
    flags,
};

#[cfg(not(test))]
use crate::config::{BootConfig, DEFAULT_CONFIG_PATH};
#[cfg(not(test))]
use crate::console::{clear_console, init_console};
#[cfg(not(test))]
use crate::elf::{load_kernel_elf, validate_kernel_elf};
#[cfg(not(test))]
use crate::fs::{close_file, open_esp_root, query_loaded_image, read_file_pages, read_file_pool};
#[cfg(not(test))]
use crate::mmap::{
    MAX_NORMALIZED_REGIONS, acquire_mmap_and_exit_boot_services, map_uefi_memory_type,
    max_mapped_phys_end, normalize_memory_map,
};
#[cfg(not(test))]
use crate::paging::{
    BootstrapParams, IdentityRange, KERNEL_BOOT_STACK_PAGES, KERNEL_BOOT_STACK_SIZE,
    build_bootstrap_page_tables,
};
#[cfg(not(test))]
use crate::uefi::alloc::{
    PoolBuffer, UefiPageAllocator, allocate_zeroed_pages, register_boot_services,
};
#[cfg(not(test))]
use crate::uefi::protocols::{EFI_RNG_PROTOCOL_GUID, RngProtocol};
#[cfg(not(test))]
use crate::uefi::tables::{BootServices, SystemTable};
#[cfg(not(test))]
use crate::uefi::types::{
    EFI_ACPI_10_TABLE_GUID, EFI_ACPI_20_TABLE_GUID, EFI_DTB_TABLE_GUID,
    EFI_GLOBAL_VARIABLE_GUID, EFI_PAGE_SIZE, EFI_SMBIOS3_TABLE_GUID, Handle, MemoryDescriptor,
    Status,
};
#[cfg(not(test))]
use crate::uefi::{ascii_to_ucs2, find_config_table, mark_boot_services_active};

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    boot_println!("\n[KNIGHTBOOT PANIC] {}", info);
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(test))]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: Handle, system_table: *mut SystemTable) -> Status {
    if system_table.is_null() {
        return Status::INVALID_PARAMETER;
    }

    let st = unsafe { &*system_table };
    if st.boot_services.is_null() {
        return Status::UNSUPPORTED;
    }
    let bs = unsafe { &*st.boot_services };

    mark_boot_services_active();
    register_boot_services(st.boot_services);
    init_console(st.con_out);
    clear_console();

    // Disable firmware watchdog timer so loading/debugging never triggers a reboot.
    unsafe {
        let _ = (bs.set_watchdog_timer)(0, 0, 0, core::ptr::null());
    }

    match boot_sequence(image_handle, st, bs) {
        Ok(never) => match never {},
        Err(status) => {
            boot_println!("[knightboot] Fatal boot error: Status({:#x})", status.0);
            stall_forever(bs);
        }
    }
}

#[cfg(not(test))]
enum Never {}

#[cfg(not(test))]
fn stall_forever(bs: &BootServices) -> ! {
    loop {
        if crate::uefi::boot_services_active() {
            unsafe {
                let _ = (bs.stall)(1_000_000);
            }
        } else {
            core::hint::spin_loop();
        }
    }
}

#[cfg(not(test))]
fn boot_sequence(
    image_handle: Handle,
    st: &SystemTable,
    bs: &BootServices,
) -> Result<Never, Status> {
    let arch = arch::current_arch();
    boot_println!("==========================================");
    boot_println!(" KnightBoot UEFI Bootloader (KBIP v1)");
    boot_println!(" Target Arch: {:?}", arch);
    boot_println!("==========================================");

    let loaded_image = query_loaded_image(bs, image_handle)?;
    let esp_root = open_esp_root(bs, loaded_image.device_handle)?;

    // Stage 2: Configuration, GOP, ACPI/DTB, Entropy.
    let cfg = match read_file_pool(esp_root, DEFAULT_CONFIG_PATH) {
        Ok(buf) => {
            let text = core::str::from_utf8(buf.as_slice()).unwrap_or("");
            boot_println!("[knightboot] Loaded {}", DEFAULT_CONFIG_PATH);
            BootConfig::parse(text)
        }
        Err(_) => {
            boot_println!("[knightboot] No knightboot.toml found; using defaults.");
            BootConfig::defaults()
        }
    };

    let hhdm_offset = DEFAULT_HHDM_OFFSET;
    let framebuffer = match gop::init_framebuffer(bs, cfg.fb_width, cfg.fb_height, hhdm_offset) {
        Ok(fb) => {
            boot_println!(
                "[knightboot] GOP: {}x{} (pitch {} B, phys {:#x})",
                fb.width,
                fb.height,
                fb.pitch_bytes,
                fb.phys_base
            );
            fb
        }
        Err(e) => {
            boot_println!("[knightboot] GOP unavailable ({:#x}); headless boot.", e.0);
            knightboot_protocol::KbipFramebuffer::empty()
        }
    };

    let acpi_rsdp_phys = unsafe {
        find_config_table(st, &EFI_ACPI_20_TABLE_GUID)
            .or_else(|| find_config_table(st, &EFI_ACPI_10_TABLE_GUID))
            .map(|p| p as u64)
            .unwrap_or(0)
    };
    let dtb_phys = unsafe {
        find_config_table(st, &EFI_DTB_TABLE_GUID)
            .map(|p| p as u64)
            .unwrap_or(0)
    };
    let smbios_phys = unsafe {
        find_config_table(st, &EFI_SMBIOS3_TABLE_GUID)
            .map(|p| p as u64)
            .unwrap_or(0)
    };

    let secure_boot = query_secure_boot(st);
    let el2_present = arch::is_el2_present();

    let mut kaslr_seed = [0u8; 32];
    gather_entropy(bs, &mut kaslr_seed);
    let kaslr_slide = if cfg.kaslr {
        // Bounded 256 MiB slide in 2 MiB increments for PIE/ET_DYN kernels.
        let raw = u64::from_le_bytes(kaslr_seed[0..8].try_into().unwrap());
        raw & 0x0FE0_0000
    } else {
        0
    };

    boot_println!(
        "[knightboot] Firmware tables: ACPI={:#x} DTB={:#x} SecureBoot={}",
        acpi_rsdp_phys,
        dtb_phys,
        secure_boot
    );

    // Stage 3: Load Kernel ELF64 and optional initrd.
    boot_println!("[knightboot] Loading kernel: {}", cfg.kernel_path());
    let kernel_file = read_file_pool(esp_root, cfg.kernel_path())?;
    let validated_elf = validate_kernel_elf(kernel_file.as_slice(), arch).map_err(|e| {
        boot_println!("[knightboot] ELF validation failed: {:?}", e);
        Status::LOAD_ERROR
    })?;
    let loaded_kernel = load_kernel_elf(&validated_elf, DEFAULT_KERNEL_VIRT_BASE, kaslr_slide)?;
    drop(kernel_file);

    let (initrd_phys, initrd_size) = match read_file_pages(esp_root, cfg.initrd_path()) {
        Ok((p, s)) => {
            boot_println!("[knightboot] Loaded initrd: {} bytes at {:#x}", s, p);
            (p, s as u64)
        }
        Err(_) => {
            boot_println!("[knightboot] No initrd at {}; continuing without initrd.", cfg.initrd_path());
            (0, 0)
        }
    };
    close_file(esp_root);

    // Allocate KBIP struct page, cmdline page, and guard-paged kernel boot stack.
    let kbip_phys = allocate_zeroed_pages(1)?;
    let kernel_stack_phys = allocate_zeroed_pages(KERNEL_BOOT_STACK_PAGES)?;

    let (cmdline_phys, cmdline_len) = if !cfg.cmdline().is_empty() {
        let cmd_phys = allocate_zeroed_pages(1)?;
        let bytes = cfg.cmdline().as_bytes();
        let copy_len = bytes.len().min(EFI_PAGE_SIZE);
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), cmd_phys as *mut u8, copy_len);
        }
        (cmd_phys, copy_len as u32)
    } else {
        (0, 0)
    };

    // Probe physical RAM upper bound before building HHDM + bootstrap page tables.
    let mut probed_max_phys = probe_max_phys_end(bs).unwrap_or(0x1_0000_0000);
    if framebuffer.phys_base != 0 && framebuffer.size > 0 {
        probed_max_phys = probed_max_phys.max(framebuffer.phys_base + framebuffer.size);
    }

    let stack_probe: u64 = 0;
    let current_stack_addr = (&stack_probe as *const u64) as u64;
    let stack_window_start = current_stack_addr.saturating_sub(64 * 1024);

    let identity_ranges = [
        IdentityRange {
            start: loaded_image.image_base,
            size: loaded_image.image_size,
            executable: true,
        },
        IdentityRange {
            start: stack_window_start,
            size: 128 * 1024,
            executable: false,
        },
    ];

    let mut page_alloc = UefiPageAllocator;
    let layout = build_bootstrap_page_tables(
        &mut page_alloc,
        &BootstrapParams {
            arch,
            kernel: &loaded_kernel,
            hhdm_offset,
            max_phys_end: probed_max_phys,
            framebuffer: &framebuffer,
            kernel_stack_phys,
            identity_ranges: &identity_ranges,
        },
    )
    .map_err(|msg| {
        boot_println!("[knightboot] Page table build failed: {}", msg);
        Status::OUT_OF_RESOURCES
    })?;

    boot_println!(
        "[knightboot] Kernel entry={:#x} stack_top={:#x}; exiting UEFI BootServices...",
        loaded_kernel.entry_virt,
        layout.kernel_stack_top_virt
    );

    // Stage 4: Acquire final memory map and exit UEFI Boot Services.
    let (raw_mmap, norm_mmap_phys) = acquire_mmap_and_exit_boot_services(bs, image_handle)?;

    // Build normalized region overlays for KernelAndModules and Framebuffer.
    let mut overlays = [(0u64, 0u64, KbipMemoryKind::Reserved); 20];
    let mut ov_count = 0usize;
    for seg in loaded_kernel.segments[..loaded_kernel.segment_count].iter().flatten() {
        overlays[ov_count] = (
            seg.phys_base,
            (seg.page_count as u64) * (EFI_PAGE_SIZE as u64),
            KbipMemoryKind::KernelAndModules,
        );
        ov_count += 1;
    }
    overlays[ov_count] = (
        kernel_stack_phys,
        KERNEL_BOOT_STACK_SIZE,
        KbipMemoryKind::KernelAndModules,
    );
    ov_count += 1;
    if initrd_phys != 0 && initrd_size > 0 {
        overlays[ov_count] = (
            initrd_phys,
            crate::elf::align_up(initrd_size, EFI_PAGE_SIZE as u64),
            KbipMemoryKind::KernelAndModules,
        );
        ov_count += 1;
    }
    if framebuffer.phys_base != 0 && framebuffer.size > 0 {
        overlays[ov_count] = (
            crate::elf::align_down(framebuffer.phys_base, EFI_PAGE_SIZE as u64),
            crate::elf::align_up(framebuffer.size, EFI_PAGE_SIZE as u64),
            KbipMemoryKind::Framebuffer,
        );
        ov_count += 1;
    }

    let norm_slice = unsafe {
        core::slice::from_raw_parts_mut(norm_mmap_phys as *mut KbipMemoryRegion, MAX_NORMALIZED_REGIONS)
    };
    let norm_count = unsafe { normalize_memory_map(&raw_mmap, norm_slice, &overlays[..ov_count]) };

    // Populate KBIP v1 handoff structure.
    let mut kbip = KbipBootInfo::new(arch);
    let mut kbip_flags = flags::HHDM_PRESENT | flags::UEFI_MMAP_PRESENT;
    if secure_boot {
        kbip_flags |= flags::SECURE_BOOT;
    }
    if loaded_kernel.kaslr_slide != 0 {
        kbip_flags |= flags::KASLR_ENABLED;
    }
    if el2_present {
        kbip_flags |= flags::EL2_PRESENT;
    }
    if framebuffer.phys_base != 0 {
        kbip_flags |= flags::FRAMEBUFFER_PRESENT;
    }
    if acpi_rsdp_phys != 0 {
        kbip_flags |= flags::ACPI_PRESENT;
    }
    if dtb_phys != 0 {
        kbip_flags |= flags::DTB_PRESENT;
    }
    if initrd_phys != 0 && initrd_size > 0 {
        kbip_flags |= flags::INITRD_PRESENT;
    }

    kbip.flags = kbip_flags;
    kbip.hhdm_offset = hhdm_offset;
    kbip.kernel_phys_base = loaded_kernel.phys_base;
    kbip.kernel_virt_base = loaded_kernel.virt_base;
    kbip.kernel_size = loaded_kernel.total_size;
    kbip.kernel_entry_virt = loaded_kernel.entry_virt;
    kbip.kernel_stack_top_virt = layout.kernel_stack_top_virt;
    kbip.kernel_stack_size = KERNEL_BOOT_STACK_SIZE;

    kbip.memory_map_phys = norm_mmap_phys;
    kbip.memory_map_ptr = hhdm_offset + norm_mmap_phys;
    kbip.memory_map_count = norm_count as u64;

    kbip.uefi_mmap_phys = raw_mmap.buffer_phys;
    kbip.uefi_mmap_ptr = hhdm_offset + raw_mmap.buffer_phys;
    kbip.uefi_mmap_size = raw_mmap.map_size as u64;
    kbip.uefi_mmap_desc_size = raw_mmap.desc_size as u64;
    kbip.uefi_mmap_desc_version = raw_mmap.desc_version;

    kbip.framebuffer = framebuffer;
    kbip.acpi_rsdp_phys = acpi_rsdp_phys;
    kbip.dtb_phys = dtb_phys;
    kbip.smbios_phys = smbios_phys;
    kbip.uefi_system_table_phys = st as *const SystemTable as u64;
    kbip.initrd_phys = initrd_phys;
    kbip.initrd_size = initrd_size;
    kbip.cmdline_ptr = if cmdline_phys != 0 { hhdm_offset + cmdline_phys } else { 0 };
    kbip.cmdline_len = cmdline_len;
    kbip.boot_slot = cfg.slot;
    kbip.kaslr_seed = kaslr_seed;
    kbip.finalize_checksum();

    unsafe {
        core::ptr::write(kbip_phys as *mut KbipBootInfo, kbip);
    }

    // Stage 5: Architecture-specific CPU state transition and jump to kernel entry.
    unsafe {
        arch::handoff_to_kernel(&layout, loaded_kernel.entry_virt, hhdm_offset + kbip_phys);
    }
}

#[cfg(not(test))]
fn gather_entropy(bs: &BootServices, seed: &mut [u8; 32]) {
    let mut rng_ptr: *mut c_void = null_mut();
    let status = unsafe {
        (bs.locate_protocol)(&EFI_RNG_PROTOCOL_GUID, core::ptr::null(), &mut rng_ptr)
    };
    if status.is_success() && !rng_ptr.is_null() {
        let rng = rng_ptr as *mut RngProtocol;
        let _ = unsafe {
            ((*rng).get_rng)(rng, core::ptr::null(), seed.len(), seed.as_mut_ptr())
        };
    }
    arch::mix_hardware_entropy(seed);
}

#[cfg(not(test))]
fn query_secure_boot(st: &SystemTable) -> bool {
    if st.runtime_services.is_null() {
        return false;
    }
    let rt = unsafe { &*st.runtime_services };
    let Ok(name) = ascii_to_ucs2::<16>("SecureBoot") else {
        return false;
    };
    let mut val: u8 = 0;
    let mut size: usize = 1;
    let mut attrs: u32 = 0;
    let status = unsafe {
        (rt.get_variable)(
            name.as_ptr(),
            &EFI_GLOBAL_VARIABLE_GUID,
            &mut attrs,
            &mut size,
            (&mut val as *mut u8) as *mut c_void,
        )
    };
    status.is_success() && val == 1
}

#[cfg(not(test))]
fn probe_max_phys_end(bs: &BootServices) -> Result<u64, Status> {
    let mut size: usize = 0;
    let mut map_key: usize = 0;
    let mut desc_size: usize = 0;
    let mut desc_ver: u32 = 0;
    let s = unsafe {
        (bs.get_memory_map)(
            &mut size,
            null_mut(),
            &mut map_key,
            &mut desc_size,
            &mut desc_ver,
        )
    };
    if s != Status::BUFFER_TOO_SMALL && s.is_error() {
        return Err(s);
    }
    let stride = desc_size.max(core::mem::size_of::<MemoryDescriptor>());
    let total = size + stride * 16;
    let mut buf = PoolBuffer::new_zeroed(total)?;
    let mut current = buf.as_slice().len();
    let s2 = unsafe {
        (bs.get_memory_map)(
            &mut current,
            buf.as_mut_slice().as_mut_ptr() as *mut MemoryDescriptor,
            &mut map_key,
            &mut desc_size,
            &mut desc_ver,
        )
    };
    if s2.is_error() {
        return Err(s2);
    }
    let mut tmp = [KbipMemoryRegion::new(0, 0, KbipMemoryKind::Reserved, 0); 1];
    let mut max_end = 0x1000_0000u64;
    let mut off = 0usize;
    let slice = buf.as_slice();
    while off + core::mem::size_of::<MemoryDescriptor>() <= current {
        let desc = unsafe {
            core::ptr::read_unaligned(slice.as_ptr().add(off) as *const MemoryDescriptor)
        };
        off += desc_size;
        tmp[0] = KbipMemoryRegion::new(
            desc.physical_start,
            desc.number_of_pages.saturating_mul(EFI_PAGE_SIZE as u64),
            map_uefi_memory_type(desc.ty),
            desc.attribute,
        );
        max_end = max_end.max(max_mapped_phys_end(&tmp));
    }
    Ok(max_end)
}
