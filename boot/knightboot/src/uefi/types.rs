//! Fundamental UEFI C-ABI scalar, status, GUID, and memory descriptor types.

#![allow(dead_code, clippy::enum_variant_names)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Event = *mut c_void;
pub type PhysicalAddress = u64;
pub type VirtualAddress = u64;
pub type Char16 = u16;

pub const EFI_PAGE_SIZE: usize = 4096;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status(pub usize);

impl Status {
    const ERROR_BIT: usize = 1usize << (usize::BITS - 1);

    pub const SUCCESS: Self = Self(0);
    pub const LOAD_ERROR: Self = Self(Self::ERROR_BIT | 1);
    pub const INVALID_PARAMETER: Self = Self(Self::ERROR_BIT | 2);
    pub const UNSUPPORTED: Self = Self(Self::ERROR_BIT | 3);
    pub const BAD_BUFFER_SIZE: Self = Self(Self::ERROR_BIT | 4);
    pub const BUFFER_TOO_SMALL: Self = Self(Self::ERROR_BIT | 5);
    pub const NOT_READY: Self = Self(Self::ERROR_BIT | 6);
    pub const DEVICE_ERROR: Self = Self(Self::ERROR_BIT | 7);
    pub const OUT_OF_RESOURCES: Self = Self(Self::ERROR_BIT | 9);
    pub const NOT_FOUND: Self = Self(Self::ERROR_BIT | 14);
    pub const SECURITY_VIOLATION: Self = Self(Self::ERROR_BIT | 26);

    pub const fn is_success(self) -> bool {
        self.0 == 0
    }

    pub const fn is_error(self) -> bool {
        (self.0 & Self::ERROR_BIT) != 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

pub const EFI_ACPI_20_TABLE_GUID: Guid = Guid::new(
    0x8868_E871,
    0xE4F1,
    0x11D3,
    [0xBC, 0x22, 0x00, 0x80, 0xC7, 0x3C, 0x88, 0x81],
);

pub const EFI_ACPI_10_TABLE_GUID: Guid = Guid::new(
    0xEB9D_2D30,
    0x2D88,
    0x11D3,
    [0x9A, 0x16, 0x00, 0x90, 0x27, 0x3F, 0xC1, 0x4D],
);

pub const EFI_DTB_TABLE_GUID: Guid = Guid::new(
    0xB1B6_21D5,
    0xF19C,
    0x41A5,
    [0x83, 0x0B, 0xD9, 0x15, 0x2C, 0x69, 0xAA, 0xE0],
);

pub const EFI_SMBIOS3_TABLE_GUID: Guid = Guid::new(
    0xF2FD_1544,
    0x9794,
    0x4A2C,
    [0x99, 0x2E, 0xE5, 0xBB, 0xCF, 0x20, 0xE3, 0x94],
);

pub const EFI_GLOBAL_VARIABLE_GUID: Guid = Guid::new(
    0x8BE4_DF61,
    0x93CA,
    0x11D2,
    [0xAA, 0x0D, 0x00, 0xE0, 0x98, 0x03, 0x2B, 0x8C],
);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllocateType {
    AllocateAnyPages = 0,
    AllocateMaxAddress = 1,
    AllocateAddress = 2,
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryType {
    ReservedMemoryType = 0,
    LoaderCode = 1,
    LoaderData = 2,
    BootServicesCode = 3,
    BootServicesData = 4,
    RuntimeServicesCode = 5,
    RuntimeServicesData = 6,
    ConventionalMemory = 7,
    UnusableMemory = 8,
    AcpiReclaimMemory = 9,
    AcpiMemoryNvs = 10,
    MemoryMappedIo = 11,
    MemoryMappedIoPortSpace = 12,
    PalCode = 13,
    PersistentMemory = 14,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryDescriptor {
    pub ty: u32,
    pub _pad: u32,
    pub physical_start: PhysicalAddress,
    pub virtual_start: VirtualAddress,
    pub number_of_pages: u64,
    pub attribute: u64,
}
