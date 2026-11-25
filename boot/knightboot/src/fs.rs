//! UEFI Simple File System file reader for loading config, kernel ELF, and initrd.

use core::ffi::c_void;
use core::ptr::null_mut;

use crate::uefi::alloc::{PoolBuffer, allocate_zeroed_pages};
use crate::uefi::ascii_to_ucs2;
use crate::uefi::protocols::{
    EFI_FILE_INFO_ID, EFI_FILE_MODE_READ, EFI_LOADED_IMAGE_PROTOCOL_GUID,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, FileInfoHeader, FileProtocol, LoadedImageProtocol,
    SimpleFileSystemProtocol,
};
use crate::uefi::tables::BootServices;
use crate::uefi::types::{EFI_PAGE_SIZE, Handle, PhysicalAddress, Status};

pub struct LoadedImageInfo {
    pub device_handle: Handle,
    pub image_base: u64,
    pub image_size: u64,
}

/// Queries the UEFI LoadedImageProtocol for the running `knightboot` EFI image.
pub fn query_loaded_image(
    bs: &BootServices,
    image_handle: Handle,
) -> Result<LoadedImageInfo, Status> {
    let mut iface: *mut c_void = null_mut();
    let status = unsafe {
        (bs.handle_protocol)(
            image_handle,
            &EFI_LOADED_IMAGE_PROTOCOL_GUID,
            &mut iface,
        )
    };
    if status.is_error() || iface.is_null() {
        return Err(if status.is_error() { status } else { Status::LOAD_ERROR });
    }
    let loaded = unsafe { &*(iface as *const LoadedImageProtocol) };
    Ok(LoadedImageInfo {
        device_handle: loaded.device_handle,
        image_base: loaded.image_base as u64,
        image_size: loaded.image_size,
    })
}

/// Opens the ESP root directory for `device_handle`.
pub fn open_esp_root(bs: &BootServices, device_handle: Handle) -> Result<*mut FileProtocol, Status> {
    let mut fs_iface: *mut c_void = null_mut();
    let status = unsafe {
        (bs.handle_protocol)(
            device_handle,
            &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
            &mut fs_iface,
        )
    };
    if status.is_error() || fs_iface.is_null() {
        return Err(if status.is_error() { status } else { Status::UNSUPPORTED });
    }

    let fs = fs_iface as *mut SimpleFileSystemProtocol;
    let mut root: *mut FileProtocol = null_mut();
    let status = unsafe { ((*fs).open_volume)(fs, &mut root) };
    if status.is_error() || root.is_null() {
        return Err(if status.is_error() { status } else { Status::DEVICE_ERROR });
    }
    Ok(root)
}

fn open_file(root: *mut FileProtocol, ascii_path: &str) -> Result<*mut FileProtocol, Status> {
    let path_ucs2: [u16; 256] = ascii_to_ucs2(ascii_path)?;
    let mut file: *mut FileProtocol = null_mut();
    let status = unsafe {
        ((*root).open)(
            root,
            &mut file,
            path_ucs2.as_ptr(),
            EFI_FILE_MODE_READ,
            0,
        )
    };
    if status.is_error() || file.is_null() {
        return Err(if status.is_error() { status } else { Status::NOT_FOUND });
    }
    Ok(file)
}

fn query_file_size(file: *mut FileProtocol) -> Result<usize, Status> {
    let mut info_size = core::mem::size_of::<FileInfoHeader>() + 256;
    let mut buf = PoolBuffer::new_zeroed(info_size)?;
    let mut status = unsafe {
        ((*file).get_info)(
            file,
            &EFI_FILE_INFO_ID,
            &mut info_size,
            buf.as_mut_slice().as_mut_ptr() as *mut c_void,
        )
    };
    if status == Status::BUFFER_TOO_SMALL {
        buf = PoolBuffer::new_zeroed(info_size)?;
        status = unsafe {
            ((*file).get_info)(
                file,
                &EFI_FILE_INFO_ID,
                &mut info_size,
                buf.as_mut_slice().as_mut_ptr() as *mut c_void,
            )
        };
    }
    if status.is_error() {
        return Err(status);
    }
    let hdr = unsafe { &*(buf.as_slice().as_ptr() as *const FileInfoHeader) };
    usize::try_from(hdr.file_size).map_err(|_| Status::OUT_OF_RESOURCES)
}

fn read_exact_into_buf(file: *mut FileProtocol, dst: &mut [u8]) -> Result<(), Status> {
    let mut offset = 0usize;
    while offset < dst.len() {
        let mut chunk = dst.len() - offset;
        let status = unsafe {
            ((*file).read)(
                file,
                &mut chunk,
                dst[offset..].as_mut_ptr() as *mut c_void,
            )
        };
        if status.is_error() {
            return Err(status);
        }
        if chunk == 0 {
            return Err(Status::LOAD_ERROR);
        }
        offset += chunk;
    }
    Ok(())
}

/// Reads an entire file into a UEFI pool buffer.
pub fn read_file_pool(root: *mut FileProtocol, ascii_path: &str) -> Result<PoolBuffer, Status> {
    let file = open_file(root, ascii_path)?;
    let size_res = query_file_size(file);
    let size = match size_res {
        Ok(s) if s > 0 => s,
        Ok(_) => {
            unsafe { ((*file).close)(file) };
            return Err(Status::LOAD_ERROR);
        }
        Err(e) => {
            unsafe { ((*file).close)(file) };
            return Err(e);
        }
    };
    let mut buf = match PoolBuffer::new_zeroed(size) {
        Ok(b) => b,
        Err(e) => {
            unsafe { ((*file).close)(file) };
            return Err(e);
        }
    };
    let read_res = read_exact_into_buf(file, &mut buf.as_mut_slice()[..size]);
    unsafe { ((*file).close)(file) };
    read_res?;
    Ok(buf)
}

/// Reads a file directly into page-aligned `EfiLoaderData` pages (ideal for `initrd`).
pub fn read_file_pages(
    root: *mut FileProtocol,
    ascii_path: &str,
) -> Result<(PhysicalAddress, usize), Status> {
    let file = open_file(root, ascii_path)?;
    let size = match query_file_size(file) {
        Ok(s) => s,
        Err(e) => {
            unsafe { ((*file).close)(file) };
            return Err(e);
        }
    };
    if size == 0 {
        unsafe { ((*file).close)(file) };
        return Err(Status::LOAD_ERROR);
    }
    let pages = size.div_ceil(EFI_PAGE_SIZE);
    let phys = match allocate_zeroed_pages(pages) {
        Ok(p) => p,
        Err(e) => {
            unsafe { ((*file).close)(file) };
            return Err(e);
        }
    };
    let slice = unsafe { core::slice::from_raw_parts_mut(phys as *mut u8, size) };
    let res = read_exact_into_buf(file, slice);
    unsafe { ((*file).close)(file) };
    res?;
    Ok((phys, size))
}

pub fn close_file(file: *mut FileProtocol) {
    if !file.is_null() {
        unsafe {
            let _ = ((*file).close)(file);
        }
    }
}
