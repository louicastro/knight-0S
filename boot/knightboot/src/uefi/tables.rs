//! UEFI System Table, Boot Services, Runtime Services, and Configuration Table definitions.

#![allow(dead_code)]

use core::ffi::c_void;
use super::protocols::SimpleTextOutputProtocol;
use super::types::{
    AllocateType, Char16, Event, Guid, Handle, MemoryDescriptor, MemoryType, PhysicalAddress,
    Status, TableHeader,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ConfigurationTable {
    pub vendor_guid: Guid,
    pub vendor_table: *const c_void,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const Char16,
    pub firmware_revision: u32,
    pub _pad: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut c_void,
    pub console_out_handle: Handle,
    pub con_out: *mut SimpleTextOutputProtocol,
    pub standard_error_handle: Handle,
    pub std_err: *mut SimpleTextOutputProtocol,
    pub runtime_services: *mut RuntimeServices,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *const ConfigurationTable,
}

#[repr(C)]
pub struct RuntimeServices {
    pub hdr: TableHeader,
    pub get_time: *const c_void,
    pub set_time: *const c_void,
    pub get_wakeup_time: *const c_void,
    pub set_wakeup_time: *const c_void,
    pub set_virtual_address_map: *const c_void,
    pub convert_pointer: *const c_void,
    pub get_variable: unsafe extern "efiapi" fn(
        variable_name: *const Char16,
        vendor_guid: *const Guid,
        attributes: *mut u32,
        data_size: *mut usize,
        data: *mut c_void,
    ) -> Status,
    pub get_next_variable_name: *const c_void,
    pub set_variable: *const c_void,
    pub get_next_high_monotonic_count: *const c_void,
    pub reset_system: unsafe extern "efiapi" fn(
        reset_type: u32,
        reset_status: Status,
        data_size: usize,
        reset_data: *const c_void,
    ) -> !,
}

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,

    // Task Priority Services
    pub raise_tpl: *const c_void,
    pub restore_tpl: *const c_void,

    // Memory Services
    pub allocate_pages: unsafe extern "efiapi" fn(
        alloc_type: AllocateType,
        memory_type: MemoryType,
        pages: usize,
        memory: *mut PhysicalAddress,
    ) -> Status,
    pub free_pages: unsafe extern "efiapi" fn(memory: PhysicalAddress, pages: usize) -> Status,
    pub get_memory_map: unsafe extern "efiapi" fn(
        memory_map_size: *mut usize,
        memory_map: *mut MemoryDescriptor,
        map_key: *mut usize,
        descriptor_size: *mut usize,
        descriptor_version: *mut u32,
    ) -> Status,
    pub allocate_pool: unsafe extern "efiapi" fn(
        pool_type: MemoryType,
        size: usize,
        buffer: *mut *mut c_void,
    ) -> Status,
    pub free_pool: unsafe extern "efiapi" fn(buffer: *mut c_void) -> Status,

    // Event & Timer Services
    pub create_event: *const c_void,
    pub set_timer: *const c_void,
    pub wait_for_event: *const c_void,
    pub signal_event: *const c_void,
    pub close_event: *const c_void,
    pub check_event: *const c_void,

    // Protocol Handler Services
    pub install_protocol_interface: *const c_void,
    pub reinstall_protocol_interface: *const c_void,
    pub uninstall_protocol_interface: *const c_void,
    pub handle_protocol: unsafe extern "efiapi" fn(
        handle: Handle,
        protocol: *const Guid,
        interface: *mut *mut c_void,
    ) -> Status,
    pub reserved: *const c_void,
    pub register_protocol_notify: *const c_void,
    pub locate_handle: *const c_void,
    pub locate_device_path: *const c_void,
    pub install_configuration_table: *const c_void,

    // Image Services
    pub load_image: *const c_void,
    pub start_image: *const c_void,
    pub exit: *const c_void,
    pub unload_image: *const c_void,
    pub exit_boot_services: unsafe extern "efiapi" fn(image_handle: Handle, map_key: usize) -> Status,

    // Miscellaneous Services
    pub get_next_monotonic_count: *const c_void,
    pub stall: unsafe extern "efiapi" fn(microseconds: usize) -> Status,
    pub set_watchdog_timer: unsafe extern "efiapi" fn(
        timeout: usize,
        watchdog_code: u64,
        data_size: usize,
        watchdog_data: *const Char16,
    ) -> Status,

    // DriverSupport Services
    pub connect_controller: *const c_void,
    pub disconnect_controller: *const c_void,

    // Open and Close Protocol Services
    pub open_protocol: unsafe extern "efiapi" fn(
        handle: Handle,
        protocol: *const Guid,
        interface: *mut *mut c_void,
        agent_handle: Handle,
        controller_handle: Handle,
        attributes: u32,
    ) -> Status,
    pub close_protocol: *const c_void,
    pub open_protocol_information: *const c_void,

    // Library Services
    pub protocols_per_handle: *const c_void,
    pub locate_handle_buffer: unsafe extern "efiapi" fn(
        search_type: u32,
        protocol: *const Guid,
        search_key: *const c_void,
        no_handles: *mut usize,
        buffer: *mut *mut Handle,
    ) -> Status,
    pub locate_protocol: unsafe extern "efiapi" fn(
        protocol: *const Guid,
        registration: *const c_void,
        interface: *mut *mut c_void,
    ) -> Status,

    // Unused trailing pointers omitted intentionally; C-ABI order up to locate_protocol matches UEFI 2.x.
    pub _unused_event: Event,
}
