//! UEFI Protocol C-ABI definitions: ConOut, LoadedImage, SimpleFS, File, GOP, RNG.

#![allow(dead_code)]

use core::ffi::c_void;
use super::types::{Char16, Guid, Handle, MemoryType, PhysicalAddress, Status};

pub const EFI_LOADED_IMAGE_PROTOCOL_GUID: Guid = Guid::new(
    0x5B1B_31A1,
    0x9562,
    0x11D2,
    [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
);

pub const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: Guid = Guid::new(
    0x964E_5B22,
    0x6459,
    0x11D2,
    [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
);

pub const EFI_FILE_INFO_ID: Guid = Guid::new(
    0x0957_6E92,
    0x6D3F,
    0x11D2,
    [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
);

pub const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: Guid = Guid::new(
    0x9042_A9DE,
    0x23DC,
    0x4A38,
    [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A],
);

pub const EFI_RNG_PROTOCOL_GUID: Guid = Guid::new(
    0x3152_BCA5,
    0xEADE,
    0x433D,
    [0x86, 0x2E, 0xC0, 0x1C, 0xDC, 0x29, 0x1F, 0x44],
);

pub const EFI_FILE_MODE_READ: u64 = 0x0000_0000_0000_0001;

#[repr(C)]
pub struct SimpleTextOutputProtocol {
    pub reset: unsafe extern "efiapi" fn(this: *mut Self, extended_verification: bool) -> Status,
    pub output_string: unsafe extern "efiapi" fn(this: *mut Self, string: *const Char16) -> Status,
    pub test_string: *const c_void,
    pub query_mode: *const c_void,
    pub set_mode: *const c_void,
    pub set_attribute: *const c_void,
    pub clear_screen: unsafe extern "efiapi" fn(this: *mut Self) -> Status,
}

#[repr(C)]
pub struct LoadedImageProtocol {
    pub revision: u32,
    pub _pad: u32,
    pub parent_handle: Handle,
    pub system_table: *mut c_void,
    pub device_handle: Handle,
    pub file_path: *const c_void,
    pub reserved: *const c_void,
    pub load_options_size: u32,
    pub _pad2: u32,
    pub load_options: *const c_void,
    pub image_base: *const c_void,
    pub image_size: u64,
    pub image_code_type: MemoryType,
    pub image_data_type: MemoryType,
}

#[repr(C)]
pub struct SimpleFileSystemProtocol {
    pub revision: u64,
    pub open_volume:
        unsafe extern "efiapi" fn(this: *mut Self, root: *mut *mut FileProtocol) -> Status,
}

#[repr(C)]
pub struct FileProtocol {
    pub revision: u64,
    pub open: unsafe extern "efiapi" fn(
        this: *mut Self,
        new_handle: *mut *mut FileProtocol,
        file_name: *const Char16,
        open_mode: u64,
        attributes: u64,
    ) -> Status,
    pub close: unsafe extern "efiapi" fn(this: *mut Self) -> Status,
    pub delete: *const c_void,
    pub read: unsafe extern "efiapi" fn(
        this: *mut Self,
        buffer_size: *mut usize,
        buffer: *mut c_void,
    ) -> Status,
    pub write: *const c_void,
    pub get_position: *const c_void,
    pub set_position: unsafe extern "efiapi" fn(this: *mut Self, position: u64) -> Status,
    pub get_info: unsafe extern "efiapi" fn(
        this: *mut Self,
        information_type: *const Guid,
        buffer_size: *mut usize,
        buffer: *mut c_void,
    ) -> Status,
}

#[repr(C)]
pub struct FileInfoHeader {
    pub size: u64,
    pub file_size: u64,
    pub physical_size: u64,
    pub create_time: [u8; 16],
    pub last_access_time: [u8; 16],
    pub modification_time: [u8; 16],
    pub attribute: u64,
    pub file_name: [Char16; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PixelBitmask {
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphicsPixelFormat {
    PixelRedGreenBlueReserved8BitPerColor = 0,
    PixelBlueGreenRedReserved8BitPerColor = 1,
    PixelBitMask = 2,
    PixelBltOnly = 3,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GraphicsOutputModeInformation {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: GraphicsPixelFormat,
    pub pixel_information: PixelBitmask,
    pub pixels_per_scan_line: u32,
}

#[repr(C)]
pub struct GraphicsOutputProtocolMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *const GraphicsOutputModeInformation,
    pub size_of_info: usize,
    pub frame_buffer_base: PhysicalAddress,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct GraphicsOutputProtocol {
    pub query_mode: unsafe extern "efiapi" fn(
        this: *mut Self,
        mode_number: u32,
        size_of_info: *mut usize,
        info: *mut *const GraphicsOutputModeInformation,
    ) -> Status,
    pub set_mode: unsafe extern "efiapi" fn(this: *mut Self, mode_number: u32) -> Status,
    pub blt: *const c_void,
    pub mode: *mut GraphicsOutputProtocolMode,
}

#[repr(C)]
pub struct RngProtocol {
    pub get_info: *const c_void,
    pub get_rng: unsafe extern "efiapi" fn(
        this: *mut Self,
        rng_algorithm: *const Guid,
        rng_value_length: usize,
        rng_value: *mut u8,
    ) -> Status,
}
