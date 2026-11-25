//! UEFI BootServices page & pool allocation helpers.
//!
//! All allocations automatically refuse to run once `ExitBootServices` has succeeded.

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use super::boot_services_active;
use super::tables::BootServices;
use super::types::{AllocateType, EFI_PAGE_SIZE, MemoryType, PhysicalAddress, Status};

static BOOT_SERVICES_PTR: AtomicPtr<BootServices> = AtomicPtr::new(null_mut());

pub fn register_boot_services(bs: *mut BootServices) {
    BOOT_SERVICES_PTR.store(bs, Ordering::Release);
}

pub fn clear_boot_services() {
    BOOT_SERVICES_PTR.store(null_mut(), Ordering::Release);
}

fn bs() -> Result<&'static BootServices, Status> {
    if !boot_services_active() {
        return Err(Status::NOT_READY);
    }
    let ptr = BOOT_SERVICES_PTR.load(Ordering::Acquire);
    if ptr.is_null() {
        return Err(Status::NOT_READY);
    }
    // SAFETY: Valid until `ExitBootServices`, guarded by `boot_services_active()`.
    Ok(unsafe { &*ptr })
}

/// Allocates `pages` zero-initialized 4 KiB pages of `MemoryType::LoaderData`.
pub fn allocate_zeroed_pages(pages: usize) -> Result<PhysicalAddress, Status> {
    allocate_zeroed_pages_typed(pages, MemoryType::LoaderData)
}

/// Allocates `pages` zero-initialized 4 KiB pages of an explicit UEFI `MemoryType`.
pub fn allocate_zeroed_pages_typed(pages: usize, mem_type: MemoryType) -> Result<PhysicalAddress, Status> {
    if pages == 0 {
        return Err(Status::INVALID_PARAMETER);
    }
    let bs = bs()?;
    let mut addr: PhysicalAddress = 0;
    let status = unsafe {
        (bs.allocate_pages)(
            AllocateType::AllocateAnyPages,
            mem_type,
            pages,
            &mut addr,
        )
    };
    if status.is_error() {
        return Err(status);
    }
    // SAFETY: Newly allocated `pages * 4096` bytes are identity-mapped and exclusive to us.
    unsafe {
        core::ptr::write_bytes(addr as *mut u8, 0, pages * EFI_PAGE_SIZE);
    }
    Ok(addr)
}

#[allow(dead_code)]
pub fn free_pages(addr: PhysicalAddress, pages: usize) {
    if let Ok(bs) = bs() {
        let _ = unsafe { (bs.free_pages)(addr, pages) };
    }
}

pub fn allocate_pool(size: usize) -> Result<*mut u8, Status> {
    let bs = bs()?;
    let mut ptr: *mut c_void = null_mut();
    let status = unsafe { (bs.allocate_pool)(MemoryType::LoaderData, size, &mut ptr) };
    if status.is_error() || ptr.is_null() {
        return Err(if status.is_error() { status } else { Status::OUT_OF_RESOURCES });
    }
    unsafe { core::ptr::write_bytes(ptr as *mut u8, 0, size) };
    Ok(ptr as *mut u8)
}

pub fn free_pool(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    if let Ok(bs) = bs() {
        let _ = unsafe { (bs.free_pool)(ptr as *mut c_void) };
    }
}

/// RAII wrapper for a UEFI pool buffer that frees on drop while BootServices is active.
pub struct PoolBuffer {
    ptr: *mut u8,
    len: usize,
}

impl PoolBuffer {
    pub fn new_zeroed(len: usize) -> Result<Self, Status> {
        let ptr = allocate_pool(len.max(1))?;
        Ok(Self { ptr, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        if self.len == 0 {
            &mut []
        } else {
            unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
        }
    }

    #[allow(dead_code)]
    pub fn into_raw(self) -> (*mut u8, usize) {
        let out = (self.ptr, self.len);
        core::mem::forget(self);
        out
    }
}

impl Drop for PoolBuffer {
    fn drop(&mut self) {
        free_pool(self.ptr);
    }
}

/// Trait abstracting 4 KiB physical page allocation so paging code is unit-testable on host.
pub trait PageAllocator {
    fn alloc_zeroed_page(&mut self) -> Option<PhysicalAddress>;
}

/// BootServices-backed [`PageAllocator`] for real UEFI execution.
pub struct UefiPageAllocator;

impl PageAllocator for UefiPageAllocator {
    fn alloc_zeroed_page(&mut self) -> Option<PhysicalAddress> {
        allocate_zeroed_pages(1).ok()
    }
}
