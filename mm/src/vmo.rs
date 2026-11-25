//! Capability-backed Virtual Memory Object (`Vmo`).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use knightos_hal::{PAGE_SIZE_4K, PhysAddr, PhysFrameAllocator};

use crate::pmm::{BitmapPmm, PmmRef};

struct VmoLock<T> {
    locked: AtomicBool,
    inner: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Send for VmoLock<T> {}
unsafe impl<T: Send> Sync for VmoLock<T> {}

impl<T> VmoLock<T> {
    const fn new(val: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            inner: core::cell::UnsafeCell::new(val),
        }
    }

    fn lock(&self) -> VmoGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        VmoGuard { l: self }
    }
}

struct VmoGuard<'a, T> {
    l: &'a VmoLock<T>,
}

impl<T> core::ops::Deref for VmoGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.l.inner.get() }
    }
}

impl<T> core::ops::DerefMut for VmoGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.l.inner.get() }
    }
}

impl<T> Drop for VmoGuard<'_, T> {
    fn drop(&mut self) {
        self.l.locked.store(false, Ordering::Release);
    }
}

struct VmoInner {
    frames: Vec<PhysAddr>,
    size_bytes: usize,
    hhdm_offset: u64,
    pmm: *const BitmapPmm,
}

unsafe impl Send for VmoInner {}

impl Drop for VmoInner {
    fn drop(&mut self) {
        if !self.pmm.is_null() {
            let mut pmm_ref = PmmRef(unsafe { &*self.pmm });
            for &f in &self.frames {
                pmm_ref.free_frame(f);
            }
        }
    }
}

/// A physical-page-backed memory object shareable across tasks via capabilities.
pub struct Vmo {
    inner: VmoLock<VmoInner>,
}

impl Vmo {
    pub fn create_anonymous(
        size_bytes: usize,
        pmm: &'static BitmapPmm,
        hhdm_offset: u64,
    ) -> Option<Arc<Self>> {
        let aligned_size = if size_bytes == 0 {
            PAGE_SIZE_4K
        } else {
            size_bytes
                .checked_add(PAGE_SIZE_4K - 1)?
                & !(PAGE_SIZE_4K - 1)
        };
        let page_count = aligned_size / PAGE_SIZE_4K;
        if page_count > (128 * 1024) {
            // Bound single VMO to 512 MiB for early safety
            return None;
        }
        let mut pmm_ref = PmmRef(pmm);
        let mut frames = Vec::with_capacity(page_count);

        for _ in 0..page_count {
            let Some(frame) = pmm_ref.alloc_frame_zeroed(hhdm_offset) else {
                for f in frames {
                    pmm_ref.free_frame(f);
                }
                return None;
            };
            frames.push(frame);
        }

        Some(Arc::new(Self {
            inner: VmoLock::new(VmoInner {
                frames,
                size_bytes: aligned_size,
                hhdm_offset,
                pmm: pmm as *const BitmapPmm,
            }),
        }))
    }

    /// Allocates physically contiguous zeroed DMA frames and returns `(Vmo, phys_base)`.
    pub fn create_contiguous_dma(
        size_bytes: usize,
        pmm: &'static BitmapPmm,
        hhdm_offset: u64,
    ) -> Option<(Arc<Self>, u64)> {
        let aligned_size = if size_bytes == 0 {
            PAGE_SIZE_4K
        } else {
            size_bytes
                .checked_add(PAGE_SIZE_4K - 1)?
                & !(PAGE_SIZE_4K - 1)
        };
        let page_count = aligned_size / PAGE_SIZE_4K;
        if page_count > 1024 {
            return None;
        }
        let mut pmm_ref = PmmRef(pmm);
        let phys_base = pmm_ref.alloc_contiguous_zeroed(page_count, hhdm_offset)?;
        let mut frames = Vec::with_capacity(page_count);
        for i in 0..page_count {
            frames.push(PhysAddr(phys_base.as_u64() + (i * PAGE_SIZE_4K) as u64));
        }
        Some((
            Arc::new(Self {
                inner: VmoLock::new(VmoInner {
                    frames,
                    size_bytes: aligned_size,
                    hhdm_offset,
                    pmm: pmm as *const BitmapPmm,
                }),
            }),
            phys_base.as_u64(),
        ))
    }

    pub fn size_bytes(&self) -> usize {
        self.inner.lock().size_bytes
    }

    pub fn page_count(&self) -> usize {
        self.inner.lock().frames.len()
    }

    pub fn frame_at(&self, page_index: usize) -> Option<PhysAddr> {
        self.inner.lock().frames.get(page_index).copied()
    }

    pub fn write(&self, offset: usize, src: &[u8]) -> Result<(), ()> {
        let guard = self.inner.lock();
        if offset.checked_add(src.len()).is_none_or(|end| end > guard.size_bytes) {
            return Err(());
        }
        let mut remaining = src.len();
        let mut src_off = 0usize;
        let mut cur = offset;
        while remaining > 0 {
            let page_idx = cur / PAGE_SIZE_4K;
            let page_off = cur % PAGE_SIZE_4K;
            let chunk = remaining.min(PAGE_SIZE_4K - page_off);
            let phys = guard.frames[page_idx].as_u64();
            let dst = (guard.hhdm_offset + phys + page_off as u64) as *mut u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src[src_off..].as_ptr(), dst, chunk);
            }
            cur += chunk;
            src_off += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    pub fn read(&self, offset: usize, dst: &mut [u8]) -> Result<(), ()> {
        let guard = self.inner.lock();
        if offset.checked_add(dst.len()).is_none_or(|end| end > guard.size_bytes) {
            return Err(());
        }
        let mut remaining = dst.len();
        let mut dst_off = 0usize;
        let mut cur = offset;
        while remaining > 0 {
            let page_idx = cur / PAGE_SIZE_4K;
            let page_off = cur % PAGE_SIZE_4K;
            let chunk = remaining.min(PAGE_SIZE_4K - page_off);
            let phys = guard.frames[page_idx].as_u64();
            let src = (guard.hhdm_offset + phys + page_off as u64) as *const u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst[dst_off..].as_mut_ptr(), chunk);
            }
            cur += chunk;
            dst_off += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    pub fn clone_snapshot(&self) -> Option<Arc<Self>> {
        let guard = self.inner.lock();
        if guard.pmm.is_null() {
            return None;
        }
        let pmm = unsafe { &*guard.pmm };
        let mut pmm_ref = PmmRef(pmm);
        let mut new_frames = Vec::with_capacity(guard.frames.len());
        for &src_frame in &guard.frames {
            let Some(dst_frame) = pmm_ref.alloc_frame_zeroed(guard.hhdm_offset) else {
                for f in new_frames {
                    pmm_ref.free_frame(f);
                }
                return None;
            };
            unsafe {
                let s = (guard.hhdm_offset + src_frame.as_u64()) as *const u8;
                let d = (guard.hhdm_offset + dst_frame.as_u64()) as *mut u8;
                core::ptr::copy_nonoverlapping(s, d, PAGE_SIZE_4K);
            }
            new_frames.push(dst_frame);
        }

        Some(Arc::new(Self {
            inner: VmoLock::new(VmoInner {
                frames: new_frames,
                size_bytes: guard.size_bytes,
                hhdm_offset: guard.hhdm_offset,
                pmm: guard.pmm,
            }),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pmm::BitmapPmm;

    #[test]
    fn vmo_overflow_and_oversized_rejected() {
        extern crate alloc;
        let pmm: &'static BitmapPmm = alloc::boxed::Box::leak(alloc::boxed::Box::new(BitmapPmm::new_uninit()));
        assert!(Vmo::create_anonymous(usize::MAX, pmm, 0).is_none());
        assert!(Vmo::create_anonymous(usize::MAX - 10, pmm, 0).is_none());
        assert!(Vmo::create_anonymous(1024 * 1024 * 1024, pmm, 0).is_none());
        assert!(Vmo::create_contiguous_dma(usize::MAX, pmm, 0).is_none());
    }
}
