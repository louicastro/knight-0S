//! Minimal, zero-dependency UEFI 2.8+ C-ABI bindings and safe wrappers.

pub mod alloc;
pub mod protocols;
pub mod tables;
pub mod types;

use core::sync::atomic::{AtomicBool, Ordering};
use types::{Guid, Handle, Status};

pub static BOOT_SERVICES_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Converts an ASCII string slice into a stack-allocated UCS-2 (UTF-16) NUL-terminated buffer
/// suitable for UEFI file path APIs.
pub fn ascii_to_ucs2<const N: usize>(s: &str) -> Result<[u16; N], Status> {
    let bytes = s.as_bytes();
    if bytes.len() + 1 > N {
        return Err(Status::BUFFER_TOO_SMALL);
    }
    let mut buf = [0u16; N];
    for (i, &b) in bytes.iter().enumerate() {
        if !b.is_ascii() {
            return Err(Status::INVALID_PARAMETER);
        }
        buf[i] = b as u16;
    }
    buf[bytes.len()] = 0;
    Ok(buf)
}

/// Searches the UEFI Configuration Table for a matching vendor GUID.
pub unsafe fn find_config_table(
    st: &tables::SystemTable,
    target: &Guid,
) -> Option<*const core::ffi::c_void> {
    if st.configuration_table.is_null() || st.number_of_table_entries == 0 {
        return None;
    }
    let entries = unsafe {
        core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries)
    };
    entries
        .iter()
        .find(|entry| &entry.vendor_guid == target)
        .map(|entry| entry.vendor_table)
}

pub fn boot_services_active() -> bool {
    BOOT_SERVICES_ACTIVE.load(Ordering::Acquire)
}

pub fn mark_boot_services_exited() {
    BOOT_SERVICES_ACTIVE.store(false, Ordering::Release);
}

pub fn mark_boot_services_active() {
    BOOT_SERVICES_ACTIVE.store(true, Ordering::Release);
}

#[allow(dead_code)]
pub fn is_valid_handle(h: Handle) -> bool {
    !h.is_null()
}
