//! Pure-Rust ACPI RSDP -> XSDT/RSDT -> MADT & MCFG parser for x86-64.

use knightos_hal::{CpuCoreDesc, MAX_CPUS, PlatformTopology};

#[repr(C, packed)]
struct RsdpV1 {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
}

#[repr(C, packed)]
struct RsdpV2 {
    v1: RsdpV1,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[repr(C, packed)]
struct MadtHeader {
    sdt: SdtHeader,
    local_apic_addr: u32,
    flags: u32,
}

/// Parses ACPI tables starting from `rsdp_phys` via `hhdm_offset` to build [`PlatformTopology`].
pub fn discover_topology(rsdp_phys: u64, hhdm_offset: u64, bsp_apic_id: u32) -> PlatformTopology {
    let mut topo = PlatformTopology::empty();
    topo.irq_controller_phys = 0xFEE0_0000;

    if rsdp_phys == 0 {
        topo.cpus[0] = CpuCoreDesc {
            logical_id: 0,
            hw_id: bsp_apic_id as u64,
            is_bsp: true,
            enabled: true,
        };
        topo.cpu_count = 1;
        return topo;
    }

    let rsdp_v1 = unsafe { &*((rsdp_phys + hhdm_offset) as *const RsdpV1) };
    if &rsdp_v1.signature != b"RSD PTR " {
        return topo;
    }

    let use_xsdt = rsdp_v1.revision >= 2 && {
        let rsdp_v2 = unsafe { &*((rsdp_phys + hhdm_offset) as *const RsdpV2) };
        rsdp_v2.xsdt_address != 0
    };

    if use_xsdt {
        let rsdp_v2 = unsafe { &*((rsdp_phys + hhdm_offset) as *const RsdpV2) };
        parse_root_sdt(rsdp_v2.xsdt_address, true, hhdm_offset, bsp_apic_id, &mut topo);
    } else {
        parse_root_sdt(rsdp_v1.rsdt_address as u64, false, hhdm_offset, bsp_apic_id, &mut topo);
    }

    if topo.cpu_count == 0 {
        topo.cpus[0] = CpuCoreDesc {
            logical_id: 0,
            hw_id: bsp_apic_id as u64,
            is_bsp: true,
            enabled: true,
        };
        topo.cpu_count = 1;
    }

    topo
}

fn parse_root_sdt(
    sdt_phys: u64,
    is_xsdt: bool,
    hhdm_offset: u64,
    bsp_apic_id: u32,
    topo: &mut PlatformTopology,
) {
    if sdt_phys == 0 {
        return;
    }
    let hdr_ptr = (sdt_phys + hhdm_offset) as *const SdtHeader;
    let hdr = unsafe { *hdr_ptr };
    let hdr_size = core::mem::size_of::<SdtHeader>();
    if (hdr.length as usize) < hdr_size {
        return;
    }

    let payload_bytes = (hdr.length as usize) - hdr_size;
    let entry_size = if is_xsdt { 8 } else { 4 };
    let count = payload_bytes / entry_size;
    let base = unsafe { (hdr_ptr as *const u8).add(hdr_size) };

    for i in 0..count {
        let table_phys = unsafe {
            let p = base.add(i * entry_size);
            if is_xsdt {
                core::ptr::read_unaligned(p as *const u64)
            } else {
                core::ptr::read_unaligned(p as *const u32) as u64
            }
        };
        if table_phys == 0 {
            continue;
        }
        let tbl_hdr = unsafe { *((table_phys + hhdm_offset) as *const SdtHeader) };
        if &tbl_hdr.signature == b"APIC" {
            parse_madt(table_phys, hhdm_offset, bsp_apic_id, topo);
        } else if &tbl_hdr.signature == b"MCFG" {
            parse_mcfg(table_phys, hhdm_offset, topo);
        } else if &tbl_hdr.signature == b"DMAR" {
            parse_dmar(table_phys, hhdm_offset, topo);
        } else if &tbl_hdr.signature == b"IVRS" && (tbl_hdr.length as usize) >= 56 {
            topo.iommu_kind = knightos_hal::IommuKind::AmdVi;
        }
    }
}

fn parse_madt(madt_phys: u64, hhdm_offset: u64, bsp_apic_id: u32, topo: &mut PlatformTopology) {
    let madt_ptr = (madt_phys + hhdm_offset) as *const MadtHeader;
    let madt = unsafe { &*madt_ptr };
    if madt.local_apic_addr != 0 {
        topo.irq_controller_phys = madt.local_apic_addr as u64;
    }

    let total_len = madt.sdt.length as usize;
    let mut offset = core::mem::size_of::<MadtHeader>();
    let base = madt_ptr as *const u8;

    while offset + 2 <= total_len {
        let rec_ptr = unsafe { base.add(offset) };
        let rec_type = unsafe { *rec_ptr };
        let rec_len = unsafe { *rec_ptr.add(1) } as usize;
        if rec_len < 2 || offset + rec_len > total_len {
            break;
        }

        match rec_type {
            0 if rec_len >= 8 => {
                // Processor Local APIC
                let apic_id = unsafe { *rec_ptr.add(3) } as u64;
                let flags = unsafe { core::ptr::read_unaligned(rec_ptr.add(4) as *const u32) };
                let enabled = (flags & 0x3) != 0;
                if enabled && topo.cpu_count < MAX_CPUS {
                    let idx = topo.cpu_count;
                    topo.cpus[idx] = CpuCoreDesc {
                        logical_id: idx as u32,
                        hw_id: apic_id,
                        is_bsp: apic_id == bsp_apic_id as u64,
                        enabled: true,
                    };
                    topo.cpu_count += 1;
                }
            }
            1 if rec_len >= 12 => {
                // I/O APIC
                let ioapic_addr =
                    unsafe { core::ptr::read_unaligned(rec_ptr.add(4) as *const u32) };
                if topo.ioapic_phys == 0 {
                    topo.ioapic_phys = ioapic_addr as u64;
                }
            }
            5 if rec_len >= 12 => {
                // Local APIC Address Override
                let addr = unsafe { core::ptr::read_unaligned(rec_ptr.add(4) as *const u64) };
                topo.irq_controller_phys = addr;
            }
            9 if rec_len >= 16 => {
                // Processor Local x2APIC
                let x2apic_id =
                    unsafe { core::ptr::read_unaligned(rec_ptr.add(4) as *const u32) } as u64;
                let flags = unsafe { core::ptr::read_unaligned(rec_ptr.add(8) as *const u32) };
                let enabled = (flags & 0x3) != 0;
                if enabled && topo.cpu_count < MAX_CPUS {
                    let idx = topo.cpu_count;
                    topo.cpus[idx] = CpuCoreDesc {
                        logical_id: idx as u32,
                        hw_id: x2apic_id,
                        is_bsp: x2apic_id == bsp_apic_id as u64,
                        enabled: true,
                    };
                    topo.cpu_count += 1;
                }
            }
            _ => {}
        }

        offset += rec_len;
    }
}

fn parse_mcfg(mcfg_phys: u64, hhdm_offset: u64, topo: &mut PlatformTopology) {
    let hdr = unsafe { &*((mcfg_phys + hhdm_offset) as *const SdtHeader) };
    let base_off = core::mem::size_of::<SdtHeader>() + 8; // SDT header + 8 reserved bytes
    if (hdr.length as usize) >= base_off + 16 {
        let entry_ptr = unsafe { ((mcfg_phys + hhdm_offset) as *const u8).add(base_off) };
        let ecam = unsafe { core::ptr::read_unaligned(entry_ptr as *const u64) };
        topo.pcie_ecam_phys = ecam;
    }
}

fn parse_dmar(dmar_phys: u64, hhdm_offset: u64, topo: &mut PlatformTopology) {
    let base = (dmar_phys + hhdm_offset) as *const u8;
    let total_len = unsafe { (*(base as *const SdtHeader)).length } as usize;
    // DMAR header: 36B SDT + 1B HostAddressWidth + 1B Flags + 10B Reserved = 48B.
    let mut off = 48usize;
    while off + 4 <= total_len {
        let rec = unsafe { base.add(off) };
        let rtype = unsafe { core::ptr::read_unaligned(rec as *const u16) };
        let rlen = unsafe { core::ptr::read_unaligned(rec.add(2) as *const u16) } as usize;
        if rlen < 4 || off + rlen > total_len {
            break;
        }
        if rtype == 0 && rlen >= 16 {
            let drhd_phys = unsafe { core::ptr::read_unaligned(rec.add(8) as *const u64) };
            if drhd_phys != 0 && topo.iommu_phys == 0 {
                topo.iommu_phys = drhd_phys;
                topo.iommu_kind = knightos_hal::IommuKind::IntelVtd;
            }
        }
        off += rlen;
    }
}
