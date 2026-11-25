//! ARM64 platform topology discovery from ACPI (SBBR) and Flattened Device Tree (DTB).

use knightos_hal::{CpuCoreDesc, MAX_CPUS, PlatformTopology};

pub const DEFAULT_GICD_PHYS: u64 = 0x0800_0000;
pub const DEFAULT_GICR_PHYS: u64 = 0x080A_0000;
pub const DEFAULT_PL011_PHYS: u64 = 0x0900_0000;
pub const DEFAULT_PCIE_ECAM_PHYS: u64 = 0x40_1000_0000;

/// Discovers CPUs, GICv3 bases, PL011 UART, PCIe ECAM, and PSCI method from ACPI or DTB.
pub fn discover_topology(
    acpi_rsdp_phys: u64,
    dtb_phys: u64,
    hhdm_offset: u64,
    bsp_mpidr: u64,
) -> PlatformTopology {
    let mut topo = PlatformTopology::empty();
    topo.irq_controller_phys = DEFAULT_GICD_PHYS;
    topo.irq_redistributor_phys = DEFAULT_GICR_PHYS;
    topo.uart_phys = DEFAULT_PL011_PHYS;
    topo.pcie_ecam_phys = DEFAULT_PCIE_ECAM_PHYS;
    topo.psci_use_hvc = true;

    if acpi_rsdp_phys != 0 {
        parse_acpi(acpi_rsdp_phys, hhdm_offset, bsp_mpidr, &mut topo);
    }
    if topo.cpu_count == 0 && dtb_phys != 0 {
        parse_dtb(dtb_phys, hhdm_offset, bsp_mpidr, &mut topo);
    }
    if topo.cpu_count == 0 {
        topo.cpus[0] = CpuCoreDesc {
            logical_id: 0,
            hw_id: bsp_mpidr,
            is_bsp: true,
            enabled: true,
        };
        topo.cpu_count = 1;
    }

    topo
}

#[repr(C, packed)]
struct RsdpV2 {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
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

fn parse_acpi(rsdp_phys: u64, hhdm_offset: u64, bsp_mpidr: u64, topo: &mut PlatformTopology) {
    let rsdp = unsafe { &*((rsdp_phys + hhdm_offset) as *const RsdpV2) };
    if &rsdp.signature != b"RSD PTR " || rsdp.xsdt_address == 0 {
        return;
    }

    let xsdt_ptr = (rsdp.xsdt_address + hhdm_offset) as *const SdtHeader;
    let xsdt = unsafe { *xsdt_ptr };
    let hdr_size = core::mem::size_of::<SdtHeader>();
    if (xsdt.length as usize) < hdr_size {
        return;
    }
    let count = ((xsdt.length as usize) - hdr_size) / 8;
    let base = unsafe { (xsdt_ptr as *const u8).add(hdr_size) };

    for i in 0..count {
        let tbl_phys = unsafe { core::ptr::read_unaligned(base.add(i * 8) as *const u64) };
        if tbl_phys == 0 {
            continue;
        }
        let tbl_hdr = unsafe { *((tbl_phys + hhdm_offset) as *const SdtHeader) };
        if &tbl_hdr.signature == b"APIC" {
            parse_madt_arm(tbl_phys, hhdm_offset, bsp_mpidr, topo);
        } else if &tbl_hdr.signature == b"FACP" {
            // FADT ARM_BOOT_ARCH (offset 129 in ACPI 5.1+, 2 bytes): bit 0 = PSCI, bit 1 = HVC.
            if (tbl_hdr.length as usize) >= 131 {
                let arm_boot_arch = unsafe {
                    core::ptr::read_unaligned(((tbl_phys + hhdm_offset) as *const u8).add(129) as *const u16)
                };
                if (arm_boot_arch & 1) != 0 {
                    topo.psci_use_hvc = (arm_boot_arch & 2) != 0;
                }
            }
        } else if &tbl_hdr.signature == b"SPCR" && (tbl_hdr.length as usize) >= 56 {
            // SPCR GAS base address at offset 40..52 (Address at +44).
            let addr = unsafe {
                core::ptr::read_unaligned(((tbl_phys + hhdm_offset) as *const u8).add(44) as *const u64)
            };
            if addr != 0 {
                topo.uart_phys = addr;
            }
        } else if &tbl_hdr.signature == b"MCFG" && (tbl_hdr.length as usize) >= 60 {
            let base_addr = unsafe {
                core::ptr::read_unaligned(((tbl_phys + hhdm_offset) as *const u8).add(44) as *const u64)
            };
            if base_addr != 0 {
                topo.pcie_ecam_phys = base_addr;
            }
        } else if &tbl_hdr.signature == b"IORT" && (tbl_hdr.length as usize) >= 48 {
            parse_iort(tbl_phys, hhdm_offset, topo);
        }
    }
}

fn parse_iort(iort_phys: u64, hhdm_offset: u64, topo: &mut PlatformTopology) {
    let base = (iort_phys + hhdm_offset) as *const u8;
    let total_len = unsafe { (*(base as *const SdtHeader)).length } as usize;
    let node_count = unsafe { core::ptr::read_unaligned(base.add(36) as *const u32) } as usize;
    let mut off = unsafe { core::ptr::read_unaligned(base.add(40) as *const u32) } as usize;
    for _ in 0..node_count {
        if off + 4 > total_len {
            break;
        }
        let rec = unsafe { base.add(off) };
        let node_type = unsafe { *rec };
        let node_len = unsafe { core::ptr::read_unaligned(rec.add(1) as *const u16) } as usize;
        if node_len < 4 || off + node_len > total_len {
            break;
        }
        // Node Type 3 = SMMUv1/v2, Node Type 4 = SMMUv3 (Base Address at offset 16).
        if (node_type == 3 || node_type == 4) && node_len >= 24 {
            let smmu_base = unsafe { core::ptr::read_unaligned(rec.add(16) as *const u64) };
            if smmu_base != 0 && topo.iommu_phys == 0 {
                topo.iommu_phys = smmu_base;
                topo.iommu_kind = knightos_hal::IommuKind::ArmSmmuv3;
            }
        }
        off += node_len;
    }
}

fn parse_madt_arm(madt_phys: u64, hhdm_offset: u64, bsp_mpidr: u64, topo: &mut PlatformTopology) {
    let base = (madt_phys + hhdm_offset) as *const u8;
    let total_len = unsafe { (*(base as *const SdtHeader)).length } as usize;
    let mut offset = core::mem::size_of::<SdtHeader>() + 8; // SDT + local_int_ctrl + flags

    while offset + 2 <= total_len {
        let rec = unsafe { base.add(offset) };
        let rec_type = unsafe { *rec };
        let rec_len = unsafe { *rec.add(1) } as usize;
        if rec_len < 2 || offset + rec_len > total_len {
            break;
        }

        match rec_type {
            // MADT Type 11: GICC
            11 if rec_len >= 76 => {
                let flags = unsafe { core::ptr::read_unaligned(rec.add(12) as *const u32) };
                let enabled = (flags & 1) != 0;
                let mpidr = unsafe { core::ptr::read_unaligned(rec.add(68) as *const u64) } & 0xFF_FFFF;
                if enabled && topo.cpu_count < MAX_CPUS {
                    let idx = topo.cpu_count;
                    topo.cpus[idx] = CpuCoreDesc {
                        logical_id: idx as u32,
                        hw_id: mpidr,
                        is_bsp: mpidr == (bsp_mpidr & 0xFF_FFFF),
                        enabled: true,
                    };
                    topo.cpu_count += 1;
                }
            }
            // MADT Type 12: GICD
            12 if rec_len >= 20 => {
                let gicd = unsafe { core::ptr::read_unaligned(rec.add(8) as *const u64) };
                if gicd != 0 {
                    topo.irq_controller_phys = gicd;
                }
            }
            // MADT Type 14: GICR
            14 if rec_len >= 16 => {
                let gicr = unsafe { core::ptr::read_unaligned(rec.add(4) as *const u64) };
                if gicr != 0 {
                    topo.irq_redistributor_phys = gicr;
                }
            }
            _ => {}
        }

        offset += rec_len;
    }
}

#[repr(C)]
struct FdtHeader {
    magic: u32,
    totalsize: u32,
    off_dt_struct: u32,
    off_dt_strings: u32,
    off_mem_rsvmap: u32,
    version: u32,
    last_comp_version: u32,
    boot_cpuid_phys: u32,
    size_dt_strings: u32,
    size_dt_struct: u32,
}

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

fn parse_dtb(dtb_phys: u64, hhdm_offset: u64, bsp_mpidr: u64, topo: &mut PlatformTopology) {
    let base = (dtb_phys + hhdm_offset) as *const u8;
    let hdr = unsafe { &*(base as *const FdtHeader) };
    if u32::from_be(hdr.magic) != 0xD00D_FEED {
        return;
    }

    let struct_off = u32::from_be(hdr.off_dt_struct) as usize;
    let strings_off = u32::from_be(hdr.off_dt_strings) as usize;
    let struct_size = u32::from_be(hdr.size_dt_struct) as usize;
    let total_size = u32::from_be(hdr.totalsize) as usize;
    if struct_off + struct_size > total_size {
        return;
    }

    let blob = unsafe { core::slice::from_raw_parts(base, total_size) };
    let mut off = struct_off;
    let end = struct_off + struct_size;
    let mut depth = 0usize;
    let mut in_cpu_node = false;
    let mut in_psci_node = false;
    let mut in_gic_node = false;
    let mut in_pl011_node = false;
    let mut in_pcie_node = false;

    while off + 4 <= end {
        let token = read_be_u32(blob, off);
        off += 4;
        match token {
            FDT_BEGIN_NODE => {
                let name = read_cstr(&blob[off..]);
                let padded = (name.len() + 1 + 3) & !3;
                off += padded;
                depth += 1;
                in_cpu_node = name.starts_with(b"cpu@");
                in_psci_node = name == b"psci";
                in_gic_node = name.starts_with(b"intc@");
                in_pl011_node = name.starts_with(b"pl011@");
                in_pcie_node = name.starts_with(b"pcie@") || name.starts_with(b"pci@");
            }
            FDT_END_NODE => {
                depth = depth.saturating_sub(1);
                in_cpu_node = false;
                in_psci_node = false;
                in_gic_node = false;
                in_pl011_node = false;
                in_pcie_node = false;
            }
            FDT_PROP => {
                if off + 8 > end {
                    break;
                }
                let len = read_be_u32(blob, off) as usize;
                let nameoff = read_be_u32(blob, off + 4) as usize;
                off += 8;
                if off + len > end {
                    break;
                }
                let val = &blob[off..off + len];
                let prop_name = read_cstr(&blob[strings_off + nameoff..]);
                off += (len + 3) & !3;

                if in_cpu_node && prop_name == b"reg" && (len == 4 || len == 8) {
                    let mpidr = if len == 8 {
                        u64::from_be_bytes(val[0..8].try_into().unwrap())
                    } else {
                        u32::from_be_bytes(val[0..4].try_into().unwrap()) as u64
                    };
                    if topo.cpu_count < MAX_CPUS {
                        let idx = topo.cpu_count;
                        topo.cpus[idx] = CpuCoreDesc {
                            logical_id: idx as u32,
                            hw_id: mpidr,
                            is_bsp: (mpidr & 0xFF_FFFF) == (bsp_mpidr & 0xFF_FFFF),
                            enabled: true,
                        };
                        topo.cpu_count += 1;
                    }
                } else if in_psci_node && prop_name == b"method" {
                    topo.psci_use_hvc = val.starts_with(b"hvc");
                } else if in_gic_node && prop_name == b"reg" && len >= 32 {
                    topo.irq_controller_phys = u64::from_be_bytes(val[0..8].try_into().unwrap());
                    topo.irq_redistributor_phys = u64::from_be_bytes(val[16..24].try_into().unwrap());
                } else if in_pl011_node && prop_name == b"reg" && len >= 8 {
                    topo.uart_phys = u64::from_be_bytes(val[0..8].try_into().unwrap());
                } else if in_pcie_node && prop_name == b"reg" && len >= 16 {
                    let base = u64::from_be_bytes(val[0..8].try_into().unwrap());
                    if base != 0 {
                        topo.pcie_ecam_phys = base;
                    }
                }
            }
            FDT_NOP => {}
            FDT_END | _ => break,
        }
    }
}

fn read_be_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_cstr(buf: &[u8]) -> &[u8] {
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    &buf[..len]
}
