//! UEFI Graphics Output Protocol (GOP) initialization and framebuffer extraction.

use core::ffi::c_void;
use core::ptr::null_mut;

use knightboot_protocol::{KbipFramebuffer, KbipPixelFormat};

use crate::uefi::protocols::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, GraphicsOutputModeInformation, GraphicsOutputProtocol,
    GraphicsPixelFormat,
};
use crate::uefi::tables::BootServices;
use crate::uefi::types::Status;

/// Queries UEFI GOP, optionally selects a preferred or highest-resolution linear framebuffer
/// mode, and populates [`KbipFramebuffer`].
pub fn init_framebuffer(
    bs: &BootServices,
    preferred_width: Option<u32>,
    preferred_height: Option<u32>,
    hhdm_offset: u64,
) -> Result<KbipFramebuffer, Status> {
    let mut gop_ptr: *mut c_void = null_mut();
    let status = unsafe {
        (bs.locate_protocol)(
            &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
            core::ptr::null(),
            &mut gop_ptr,
        )
    };
    if status.is_error() || gop_ptr.is_null() {
        return Err(if status.is_error() { status } else { Status::UNSUPPORTED });
    }

    let gop = gop_ptr as *mut GraphicsOutputProtocol;
    let mode = unsafe { &*(*gop).mode };

    let mut best_mode = mode.mode;
    let mut best_score: u64 = 0;

    for m in 0..mode.max_mode {
        let mut info_size: usize = 0;
        let mut info_ptr: *const GraphicsOutputModeInformation = core::ptr::null();
        let q = unsafe { ((*gop).query_mode)(gop, m, &mut info_size, &mut info_ptr) };
        if q.is_error() || info_ptr.is_null() {
            continue;
        }
        let info = unsafe { &*info_ptr };
        if info.pixel_format == GraphicsPixelFormat::PixelBltOnly {
            continue;
        }

        if let (Some(pw), Some(ph)) = (preferred_width, preferred_height)
            && info.horizontal_resolution == pw
            && info.vertical_resolution == ph
        {
            best_mode = m;
            best_score = u64::MAX;
            break;
        }

        // Prefer 1080p / highest native linear pixel area capped below absurd virtual modes if needed.
        let area = (info.horizontal_resolution as u64) * (info.vertical_resolution as u64);
        if area > best_score {
            best_score = area;
            best_mode = m;
        }
    }

    if best_mode != mode.mode && best_score > 0 {
        let _ = unsafe { ((*gop).set_mode)(gop, best_mode) };
    }

    let active_mode = unsafe { &*(*gop).mode };
    if active_mode.info.is_null() || active_mode.frame_buffer_base == 0 {
        return Err(Status::UNSUPPORTED);
    }
    let info = unsafe { &*active_mode.info };
    if info.pixel_format == GraphicsPixelFormat::PixelBltOnly {
        return Err(Status::UNSUPPORTED);
    }

    let (pixel_format, red_mask, green_mask, blue_mask) = match info.pixel_format {
        GraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor => (
            KbipPixelFormat::Rgbx8888,
            0x0000_00FF,
            0x0000_FF00,
            0x00FF_0000,
        ),
        GraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor => (
            KbipPixelFormat::Bgrx8888,
            0x00FF_0000,
            0x0000_FF00,
            0x0000_00FF,
        ),
        GraphicsPixelFormat::PixelBitMask => (
            KbipPixelFormat::BitMask,
            info.pixel_information.red_mask,
            info.pixel_information.green_mask,
            info.pixel_information.blue_mask,
        ),
        GraphicsPixelFormat::PixelBltOnly => (KbipPixelFormat::Unknown, 0, 0, 0),
    };

    let bpp = 32u8;
    let pitch_bytes = info.pixels_per_scan_line.saturating_mul(4);
    let computed_size = (pitch_bytes as u64).saturating_mul(info.vertical_resolution as u64);
    let fb_size = (active_mode.frame_buffer_size as u64).max(computed_size);

    Ok(KbipFramebuffer {
        phys_base: active_mode.frame_buffer_base,
        virt_base: hhdm_offset.wrapping_add(active_mode.frame_buffer_base),
        size: fb_size,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        pixels_per_scanline: info.pixels_per_scan_line,
        pitch_bytes,
        bpp,
        pixel_format: pixel_format as u8,
        _reserved: [0; 2],
        red_mask,
        green_mask,
        blue_mask,
        _pad_end: 0,
    })
}
