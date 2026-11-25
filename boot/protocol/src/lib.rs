#![cfg_attr(not(test), no_std)]
//! KnightBoot Info Protocol (KBIP) — Versioned Boot Handoff ABI.
//!
//! Shared `#![no_std]` C-ABI contract passed from `knightboot` to the KnightOS
//! kernel (`_start(kbip: *const KbipBootInfo) -> !`). Zero external dependencies.

use core::mem::size_of;

/// Magic number `"KNIGHTB1"` in little-endian (`0x3142_5448_4749_4E4B`).
pub const KBIP_MAGIC: u64 = u64::from_le_bytes(*b"KNIGHTB1");

/// Current KBIP protocol version.
pub const KBIP_VERSION_1: u32 = 1;

/// Canonical higher-half direct physical map base used on both x86-64 and ARM64.
pub const DEFAULT_HHDM_OFFSET: u64 = 0xFFFF_8000_0000_0000;

/// Canonical higher-half kernel virtual base.
pub const DEFAULT_KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Supported target architectures reported in [`KbipBootInfo::arch`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbipArch {
    Unknown = 0,
    X86_64 = 1,
    AArch64 = 2,
}

impl KbipArch {
    pub const fn from_u32(val: u32) -> Self {
        match val {
            1 => Self::X86_64,
            2 => Self::AArch64,
            _ => Self::Unknown,
        }
    }
}

/// Bitflags for [`KbipBootInfo::flags`].
pub mod flags {
    pub const SECURE_BOOT: u32 = 1 << 0;
    pub const KASLR_ENABLED: u32 = 1 << 1;
    pub const EL2_PRESENT: u32 = 1 << 2;
    pub const HHDM_PRESENT: u32 = 1 << 3;
    pub const FRAMEBUFFER_PRESENT: u32 = 1 << 4;
    pub const ACPI_PRESENT: u32 = 1 << 5;
    pub const DTB_PRESENT: u32 = 1 << 6;
    pub const INITRD_PRESENT: u32 = 1 << 7;
    pub const UEFI_MMAP_PRESENT: u32 = 1 << 8;
}

/// Pixel format of the linear framebuffer reported by UEFI GOP.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbipPixelFormat {
    Unknown = 0,
    /// 32-bit `R8 G8 B8 X8` in memory order (little-endian `0xXXBBGGRR`).
    Rgbx8888 = 1,
    /// 32-bit `B8 G8 R8 X8` in memory order (little-endian `0xXXRRGGBB`).
    Bgrx8888 = 2,
    /// Custom channel bitmasks specified in [`KbipFramebuffer`].
    BitMask = 3,
}

impl KbipPixelFormat {
    pub const fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Rgbx8888,
            2 => Self::Bgrx8888,
            3 => Self::BitMask,
            _ => Self::Unknown,
        }
    }
}

/// Framebuffer geometry and pixel layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KbipFramebuffer {
    pub phys_base: u64,
    pub virt_base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub pixels_per_scanline: u32,
    pub pitch_bytes: u32,
    pub bpp: u8,
    pub pixel_format: u8,
    pub _reserved: [u8; 2],
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub _pad_end: u32,
}

impl KbipFramebuffer {
    pub const fn empty() -> Self {
        Self {
            phys_base: 0,
            virt_base: 0,
            size: 0,
            width: 0,
            height: 0,
            pixels_per_scanline: 0,
            pitch_bytes: 0,
            bpp: 0,
            pixel_format: KbipPixelFormat::Unknown as u8,
            _reserved: [0; 2],
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
            _pad_end: 0,
        }
    }

    pub const fn format(&self) -> KbipPixelFormat {
        KbipPixelFormat::from_u8(self.pixel_format)
    }
}

/// Normalized physical memory region classification.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbipMemoryKind {
    /// Free conventional RAM available for immediate PMM allocation.
    Usable = 1,
    /// Firmware/hardware reserved; never allocate.
    Reserved = 2,
    /// ACPI tables that the kernel may reclaim after parsing.
    AcpiReclaimable = 3,
    /// ACPI Non-Volatile Storage; must remain preserved.
    AcpiNvs = 4,
    /// Bootloader page tables, KBIP struct, and scratch buffers reclaimable after early init.
    BootloaderReclaimable = 5,
    /// Loaded kernel segments, kernel boot stack, and `initrd` module.
    KernelAndModules = 6,
    /// Linear framebuffer MMIO aperture.
    Framebuffer = 7,
    /// UEFI Runtime Services code/data regions; must stay mapped if runtime calls are used.
    UefiRuntimeServices = 8,
    /// Defective / unusable memory reported by firmware.
    Unusable = 9,
}

impl KbipMemoryKind {
    pub const fn from_u32(val: u32) -> Option<Self> {
        match val {
            1 => Some(Self::Usable),
            2 => Some(Self::Reserved),
            3 => Some(Self::AcpiReclaimable),
            4 => Some(Self::AcpiNvs),
            5 => Some(Self::BootloaderReclaimable),
            6 => Some(Self::KernelAndModules),
            7 => Some(Self::Framebuffer),
            8 => Some(Self::UefiRuntimeServices),
            9 => Some(Self::Unusable),
            _ => None,
        }
    }
}

/// A single normalized, page-aligned physical memory descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KbipMemoryRegion {
    pub phys_start: u64,
    pub length: u64,
    pub kind: u32,
    pub _reserved: u32,
    /// Raw UEFI memory attribute flags preserved for cacheability/runtime audit.
    pub attributes: u64,
}

impl KbipMemoryRegion {
    pub const fn new(phys_start: u64, length: u64, kind: KbipMemoryKind, attributes: u64) -> Self {
        Self {
            phys_start,
            length,
            kind: kind as u32,
            _reserved: 0,
            attributes,
        }
    }

    pub const fn region_kind(&self) -> Option<KbipMemoryKind> {
        KbipMemoryKind::from_u32(self.kind)
    }

    pub const fn phys_end(&self) -> u64 {
        self.phys_start.saturating_add(self.length)
    }
}

/// Top-level KnightBoot Info Protocol (KBIP) v1 handoff structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KbipBootInfo {
    pub magic: u64,
    pub version: u32,
    pub struct_size: u32,
    pub total_size: u32,
    pub checksum: u32,
    pub arch: u32,
    pub flags: u32,

    pub hhdm_offset: u64,
    pub kernel_phys_base: u64,
    pub kernel_virt_base: u64,
    pub kernel_size: u64,
    pub kernel_entry_virt: u64,
    pub kernel_stack_top_virt: u64,
    pub kernel_stack_size: u64,

    /// Normalized KnightOS memory map (virtual/HHDM pointer and physical base).
    pub memory_map_ptr: u64,
    pub memory_map_phys: u64,
    pub memory_map_count: u64,
    pub memory_map_entry_size: u32,
    pub _reserved_mmap: u32,

    /// Raw UEFI memory map buffer and descriptor metadata from `GetMemoryMap()`.
    pub uefi_mmap_ptr: u64,
    pub uefi_mmap_phys: u64,
    pub uefi_mmap_size: u64,
    pub uefi_mmap_desc_size: u64,
    pub uefi_mmap_desc_version: u32,
    pub _reserved_uefi: u32,

    pub framebuffer: KbipFramebuffer,

    pub acpi_rsdp_phys: u64,
    pub dtb_phys: u64,
    pub smbios_phys: u64,
    pub uefi_system_table_phys: u64,

    pub initrd_phys: u64,
    pub initrd_size: u64,

    pub cmdline_ptr: u64,
    pub cmdline_len: u32,
    pub boot_slot: u32,

    pub kaslr_seed: [u8; 32],
}

const _: () = {
    assert!(size_of::<KbipFramebuffer>() == 64);
    assert!(size_of::<KbipMemoryRegion>() == 32);
    assert!(size_of::<KbipBootInfo>() == 320);
};

/// Validation failures returned by [`KbipBootInfo::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbipError {
    BadMagic,
    UnsupportedVersion(u32),
    InvalidStructSize(u32),
    BadChecksum { expected: u32, actual: u32 },
    UnsupportedArch(u32),
    MissingMemoryMap,
}

impl KbipBootInfo {
    /// Creates a zero-initialized KBIP v1 struct with magic, version, and struct size populated.
    pub const fn new(arch: KbipArch) -> Self {
        let size = size_of::<Self>() as u32;
        Self {
            magic: KBIP_MAGIC,
            version: KBIP_VERSION_1,
            struct_size: size,
            total_size: size,
            checksum: 0,
            arch: arch as u32,
            flags: 0,
            hhdm_offset: DEFAULT_HHDM_OFFSET,
            kernel_phys_base: 0,
            kernel_virt_base: DEFAULT_KERNEL_VIRT_BASE,
            kernel_size: 0,
            kernel_entry_virt: 0,
            kernel_stack_top_virt: 0,
            kernel_stack_size: 0,
            memory_map_ptr: 0,
            memory_map_phys: 0,
            memory_map_count: 0,
            memory_map_entry_size: size_of::<KbipMemoryRegion>() as u32,
            _reserved_mmap: 0,
            uefi_mmap_ptr: 0,
            uefi_mmap_phys: 0,
            uefi_mmap_size: 0,
            uefi_mmap_desc_size: 0,
            uefi_mmap_desc_version: 0,
            _reserved_uefi: 0,
            framebuffer: KbipFramebuffer::empty(),
            acpi_rsdp_phys: 0,
            dtb_phys: 0,
            smbios_phys: 0,
            uefi_system_table_phys: 0,
            initrd_phys: 0,
            initrd_size: 0,
            cmdline_ptr: 0,
            cmdline_len: 0,
            boot_slot: 0,
            kaslr_seed: [0; 32],
        }
    }

    /// Computes the IEEE 802.3 CRC32 over this header with `checksum` treated as 0.
    pub fn compute_checksum(&self) -> u32 {
        let mut copy = *self;
        copy.checksum = 0;
        // SAFETY: `copy` is a `#[repr(C)]` plain-old-data struct with no padding invariants
        // breached when read as initialized bytes (all fields are initialized via `new`).
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&copy as *const Self) as *const u8,
                size_of::<Self>(),
            )
        };
        crc32(bytes)
    }

    /// Populates `self.checksum` before handing control to the kernel.
    pub fn finalize_checksum(&mut self) {
        self.checksum = self.compute_checksum();
    }

    /// Validates magic, version, structure size, architecture, memory map presence, and CRC32.
    pub fn validate(&self) -> Result<(), KbipError> {
        if self.magic != KBIP_MAGIC {
            return Err(KbipError::BadMagic);
        }
        if self.version != KBIP_VERSION_1 {
            return Err(KbipError::UnsupportedVersion(self.version));
        }
        if self.struct_size as usize != size_of::<Self>() {
            return Err(KbipError::InvalidStructSize(self.struct_size));
        }
        if KbipArch::from_u32(self.arch) == KbipArch::Unknown {
            return Err(KbipError::UnsupportedArch(self.arch));
        }
        let expected = self.compute_checksum();
        if self.checksum != expected {
            return Err(KbipError::BadChecksum {
                expected,
                actual: self.checksum,
            });
        }
        if self.memory_map_count == 0 || (self.memory_map_ptr == 0 && self.memory_map_phys == 0) {
            return Err(KbipError::MissingMemoryMap);
        }
        Ok(())
    }

    /// Returns the normalized memory map slice when `memory_map_ptr` is readable in the current VA space.
    ///
    /// # Safety
    /// Caller must ensure `memory_map_ptr` points to `memory_map_count` valid, initialized
    /// [`KbipMemoryRegion`] entries in the active address space.
    pub unsafe fn memory_regions(&self) -> &[KbipMemoryRegion] {
        if self.memory_map_ptr == 0 || self.memory_map_count == 0 {
            return &[];
        }
        unsafe {
            core::slice::from_raw_parts(
                self.memory_map_ptr as *const KbipMemoryRegion,
                self.memory_map_count as usize,
            )
        }
    }
}

/// Standard IEEE 802.3 CRC32 (bitwise implementation for minimal footprint).
pub const fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    let mut i = 0;
    while i < data.len() {
        crc ^= data[i] as u32;
        let mut bit = 0;
        while bit < 8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            bit += 1;
        }
        i += 1;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kbip_roundtrip_and_checksum() {
        let mut info = KbipBootInfo::new(KbipArch::X86_64);
        info.kernel_phys_base = 0x0020_0000;
        info.kernel_virt_base = DEFAULT_KERNEL_VIRT_BASE;
        info.kernel_entry_virt = DEFAULT_KERNEL_VIRT_BASE + 0x1000;
        info.memory_map_phys = 0x0010_0000;
        info.memory_map_ptr = DEFAULT_HHDM_OFFSET + 0x0010_0000;
        info.memory_map_count = 4;
        info.flags = flags::HHDM_PRESENT | flags::ACPI_PRESENT | flags::UEFI_MMAP_PRESENT;
        info.finalize_checksum();

        assert_eq!(info.validate(), Ok(()));

        // Corrupt a field and ensure checksum catches it.
        info.kernel_phys_base += 0x1000;
        assert!(matches!(info.validate(), Err(KbipError::BadChecksum { .. })));
    }

    #[test]
    fn kbip_rejects_bad_magic_and_missing_mmap() {
        let mut info = KbipBootInfo::new(KbipArch::AArch64);
        info.finalize_checksum();
        assert_eq!(info.validate(), Err(KbipError::MissingMemoryMap));

        info.magic = 0xDEADBEEF;
        assert_eq!(info.validate(), Err(KbipError::BadMagic));
    }
}
