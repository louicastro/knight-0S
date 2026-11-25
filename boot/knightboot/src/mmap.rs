//! UEFI `GetMemoryMap` + `ExitBootServices` acquisition and region normalization.

use knightboot_protocol::{KbipMemoryKind, KbipMemoryRegion};

use crate::uefi::alloc::{allocate_zeroed_pages, clear_boot_services};
use crate::uefi::mark_boot_services_exited;
use crate::uefi::tables::BootServices;
use crate::uefi::types::{
    EFI_PAGE_SIZE, Handle, MemoryDescriptor, MemoryType, PhysicalAddress, Status,
};

/// Maximum normalized memory map regions supported in KBIP v1 bootstrap buffer.
pub const MAX_NORMALIZED_REGIONS: usize = 1024;

pub struct RawUefiMemoryMap {
    pub buffer_phys: PhysicalAddress,
    #[allow(dead_code)]
    pub buffer_len: usize,
    pub map_size: usize,
    pub desc_size: usize,
    pub desc_version: u32,
}

/// Converts a UEFI [`MemoryType`] numeric discriminant to [`KbipMemoryKind`].
pub const fn map_uefi_memory_type(ty: u32) -> KbipMemoryKind {
    match ty {
        x if x == MemoryType::ConventionalMemory as u32 => KbipMemoryKind::Usable,
        x if x == MemoryType::BootServicesCode as u32
            || x == MemoryType::BootServicesData as u32 =>
        {
            // Boot services code/data become free RAM once ExitBootServices returns.
            KbipMemoryKind::Usable
        }
        x if x == MemoryType::LoaderCode as u32 || x == MemoryType::LoaderData as u32 => {
            KbipMemoryKind::BootloaderReclaimable
        }
        x if x == MemoryType::AcpiReclaimMemory as u32 => KbipMemoryKind::AcpiReclaimable,
        x if x == MemoryType::AcpiMemoryNvs as u32 => KbipMemoryKind::AcpiNvs,
        x if x == MemoryType::RuntimeServicesCode as u32
            || x == MemoryType::RuntimeServicesData as u32 =>
        {
            KbipMemoryKind::UefiRuntimeServices
        }
        x if x == MemoryType::UnusableMemory as u32 => KbipMemoryKind::Unusable,
        _ => KbipMemoryKind::Reserved,
    }
}

/// Acquires the final UEFI memory map and calls `ExitBootServices` with the standard
/// zero-allocation retry loop on `EFI_INVALID_PARAMETER`.
///
/// Pre-allocates the normalized KBIP region array *before* exiting boot services because
/// no UEFI allocator calls are permitted afterwards.
pub fn acquire_mmap_and_exit_boot_services(
    bs: &BootServices,
    image_handle: Handle,
) -> Result<(RawUefiMemoryMap, PhysicalAddress), Status> {
    // Pre-allocate space for normalized KbipMemoryRegion entries before ExitBootServices.
    let norm_bytes = MAX_NORMALIZED_REGIONS * core::mem::size_of::<KbipMemoryRegion>();
    let norm_pages = norm_bytes.div_ceil(EFI_PAGE_SIZE);
    let norm_phys = allocate_zeroed_pages(norm_pages)?;

    // Probe required memory map size.
    let mut probe_size: usize = 0;
    let mut map_key: usize = 0;
    let mut desc_size: usize = core::mem::size_of::<MemoryDescriptor>();
    let mut desc_version: u32 = 1;

    let probe_status = unsafe {
        (bs.get_memory_map)(
            &mut probe_size,
            core::ptr::null_mut(),
            &mut map_key,
            &mut desc_size,
            &mut desc_version,
        )
    };
    if probe_status != Status::BUFFER_TOO_SMALL && probe_status.is_error() {
        return Err(probe_status);
    }

    // Add generous slack (+32 descriptors) because AllocatePages may split descriptors.
    let desc_stride = desc_size.max(core::mem::size_of::<MemoryDescriptor>());
    let buf_bytes = probe_size + desc_stride * 32;
    let buf_pages = buf_bytes.div_ceil(EFI_PAGE_SIZE);
    let buf_phys = allocate_zeroed_pages(buf_pages)?;
    let buf_cap = buf_pages * EFI_PAGE_SIZE;

    // Retry loop: no allocations allowed inside this loop.
    for _attempt in 0..16 {
        let mut current_size = buf_cap;
        let status = unsafe {
            (bs.get_memory_map)(
                &mut current_size,
                buf_phys as *mut MemoryDescriptor,
                &mut map_key,
                &mut desc_size,
                &mut desc_version,
            )
        };
        if status.is_error() {
            return Err(status);
        }

        let exit_status = unsafe { (bs.exit_boot_services)(image_handle, map_key) };
        if exit_status.is_success() {
            mark_boot_services_exited();
            clear_boot_services();
            return Ok((
                RawUefiMemoryMap {
                    buffer_phys: buf_phys,
                    buffer_len: buf_cap,
                    map_size: current_size,
                    desc_size,
                    desc_version,
                },
                norm_phys,
            ));
        }

        // UEFI spec allows EFI_INVALID_PARAMETER when map_key went stale due to timer events;
        // any other failure is fatal.
        if exit_status != Status::INVALID_PARAMETER {
            return Err(exit_status);
        }
    }

    Err(Status::ABORTED_RETRY())
}

trait StatusExt {
    #[allow(non_snake_case)]
    fn ABORTED_RETRY() -> Self;
}

impl StatusExt for Status {
    fn ABORTED_RETRY() -> Self {
        Status::LOAD_ERROR
    }
}

/// Parses raw UEFI memory descriptors into `out`, applies explicit overrides (e.g. loaded
/// kernel/initrd/framebuffer), sorts by `phys_start`, and coalesces adjacent identical entries.
///
/// # Safety
/// `raw` must point to a valid UEFI memory map buffer captured by `GetMemoryMap`.
pub unsafe fn normalize_memory_map(
    raw: &RawUefiMemoryMap,
    out: &mut [KbipMemoryRegion],
    overlays: &[(u64, u64, KbipMemoryKind)],
) -> usize {
    let mut count = 0usize;
    let mut offset = 0usize;
    let base_ptr = raw.buffer_phys as *const u8;

    while offset + core::mem::size_of::<MemoryDescriptor>() <= raw.map_size {
        let desc = unsafe {
            core::ptr::read_unaligned(base_ptr.add(offset) as *const MemoryDescriptor)
        };
        offset += raw.desc_size;

        if desc.number_of_pages == 0 {
            continue;
        }
        let length = desc.number_of_pages.saturating_mul(EFI_PAGE_SIZE as u64);
        let kind = map_uefi_memory_type(desc.ty);
        let region = KbipMemoryRegion::new(desc.physical_start, length, kind, desc.attribute);

        if count < out.len() {
            out[count] = region;
            count += 1;
        }
    }

    for &(ov_start, ov_len, ov_kind) in overlays {
        if ov_len > 0 {
            count = apply_overlay(out, count, ov_start, ov_len, ov_kind);
        }
    }

    sort_and_coalesce(out, count)
}

/// Splits any region overlapping `[ov_start, ov_start + ov_len)` and inserts/reclassifies the
/// overlapped span as `ov_kind`.
pub fn apply_overlay(
    regions: &mut [KbipMemoryRegion],
    mut count: usize,
    ov_start: u64,
    ov_len: u64,
    ov_kind: KbipMemoryKind,
) -> usize {
    let ov_end = ov_start.saturating_add(ov_len);
    if ov_end <= ov_start {
        return count;
    }

    let mut idx = 0usize;
    let mut found_overlap = false;

    while idx < count {
        let r = regions[idx];
        let r_start = r.phys_start;
        let r_end = r.phys_end();

        if r_end <= ov_start || r_start >= ov_end {
            idx += 1;
            continue;
        }

        found_overlap = true;
        if r_start < ov_start && r_end > ov_end {
            // Split into 3 parts: left (original), middle (overlay), right (original).
            regions[idx] = KbipMemoryRegion::new(r_start, ov_start - r_start, r.region_kind().unwrap_or(KbipMemoryKind::Reserved), r.attributes);
            if count + 2 <= regions.len() {
                regions[count] = KbipMemoryRegion::new(ov_start, ov_end - ov_start, ov_kind, r.attributes);
                regions[count + 1] = KbipMemoryRegion::new(ov_end, r_end - ov_end, r.region_kind().unwrap_or(KbipMemoryKind::Reserved), r.attributes);
                count += 2;
            }
            idx += 1;
        } else if r_start < ov_start {
            regions[idx].length = ov_start - r_start;
            if count < regions.len() {
                regions[count] = KbipMemoryRegion::new(ov_start, r_end - ov_start, ov_kind, r.attributes);
                count += 1;
            }
            idx += 1;
        } else if r_end > ov_end {
            regions[idx] = KbipMemoryRegion::new(r_start, ov_end - r_start, ov_kind, r.attributes);
            if count < regions.len() {
                regions[count] = KbipMemoryRegion::new(ov_end, r_end - ov_end, r.region_kind().unwrap_or(KbipMemoryKind::Reserved), r.attributes);
                count += 1;
            }
            idx += 1;
        } else {
            regions[idx].kind = ov_kind as u32;
            idx += 1;
        }
    }

    if !found_overlap && count < regions.len() {
        // Firmware may omit MMIO apertures such as the GOP framebuffer from the memory map.
        regions[count] = KbipMemoryRegion::new(ov_start, ov_len, ov_kind, 0);
        count += 1;
    }

    count
}

/// Sorts `regions[..count]` by `phys_start` and merges adjacent regions with identical
/// `kind` and `attributes`.
pub fn sort_and_coalesce(regions: &mut [KbipMemoryRegion], count: usize) -> usize {
    if count <= 1 {
        return count;
    }

    // Insertion sort (no alloc required, fast on <= 1024 regions).
    for i in 1..count {
        let key = regions[i];
        let mut j = i;
        while j > 0 && regions[j - 1].phys_start > key.phys_start {
            regions[j] = regions[j - 1];
            j -= 1;
        }
        regions[j] = key;
    }

    let mut write = 0usize;
    for read in 1..count {
        let cur = regions[read];
        let prev = &mut regions[write];
        if prev.phys_end() == cur.phys_start
            && prev.kind == cur.kind
            && prev.attributes == cur.attributes
        {
            prev.length = prev.length.saturating_add(cur.length);
        } else {
            write += 1;
            regions[write] = cur;
        }
    }

    write + 1
}

/// Computes the highest physical address across RAM/framebuffer regions for HHDM sizing.
pub fn max_mapped_phys_end(regions: &[KbipMemoryRegion]) -> u64 {
    regions
        .iter()
        .filter(|r| {
            matches!(
                r.region_kind(),
                Some(
                    KbipMemoryKind::Usable
                        | KbipMemoryKind::AcpiReclaimable
                        | KbipMemoryKind::AcpiNvs
                        | KbipMemoryKind::BootloaderReclaimable
                        | KbipMemoryKind::KernelAndModules
                        | KbipMemoryKind::UefiRuntimeServices
                )
            )
        })
        .map(|r| r.phys_end())
        .max()
        .unwrap_or(0x1000_0000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesce_and_overlay_splits_cleanly() {
        let mut buf = [KbipMemoryRegion::new(0, 0, KbipMemoryKind::Reserved, 0); 16];
        buf[0] = KbipMemoryRegion::new(0x10_0000, 0x10_0000, KbipMemoryKind::Usable, 0xF);
        buf[1] = KbipMemoryRegion::new(0x20_0000, 0x10_0000, KbipMemoryKind::Usable, 0xF);
        let mut n = sort_and_coalesce(&mut buf, 2);
        assert_eq!(n, 1);
        assert_eq!(buf[0].phys_start, 0x10_0000);
        assert_eq!(buf[0].length, 0x20_0000);

        // Carve out kernel inside the middle of the usable region.
        n = apply_overlay(&mut buf, n, 0x18_0000, 0x04_0000, KbipMemoryKind::KernelAndModules);
        n = sort_and_coalesce(&mut buf, n);
        assert_eq!(n, 3);
        assert_eq!(buf[0].region_kind(), Some(KbipMemoryKind::Usable));
        assert_eq!(buf[0].length, 0x08_0000);
        assert_eq!(buf[1].region_kind(), Some(KbipMemoryKind::KernelAndModules));
        assert_eq!(buf[1].phys_start, 0x18_0000);
        assert_eq!(buf[1].length, 0x04_0000);
        assert_eq!(buf[2].region_kind(), Some(KbipMemoryKind::Usable));
        assert_eq!(buf[2].phys_start, 0x1C_0000);
        assert_eq!(buf[2].length, 0x14_0000);
    }
}
