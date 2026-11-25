//! Bitmap Physical Memory Manager (`BitmapPmm`) seeded from KBIP memory regions.

use core::sync::atomic::{AtomicBool, Ordering};

use knightboot_protocol::{KbipMemoryKind, KbipMemoryRegion};
use knightos_hal::{PAGE_SIZE_4K, PhysAddr, PhysFrameAllocator};

/// Minimum physical base for general PMM allocations (reserves < 1 MiB for legacy/SMP trampolines).
const PMM_MIN_PHYS: u64 = 0x0010_0000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PmmStats {
    pub total_usable_bytes: u64,
    pub free_bytes: u64,
    pub allocated_bytes: u64,
}

struct SpinLock<T> {
    locked: AtomicBool,
    inner: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    const fn new(val: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            inner: core::cell::UnsafeCell::new(val),
        }
    }

    fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard { lock: self }
    }
}

struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> core::ops::Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.inner.get() }
    }
}

impl<T> core::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.inner.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

struct PmmState {
    bitmap_ptr: *mut u8,
    bitmap_bytes: usize,
    total_frames: usize,
    free_frames: usize,
    usable_frames: usize,
    search_hint: usize,
}

unsafe impl Send for PmmState {}

impl PmmState {
    const fn empty() -> Self {
        Self {
            bitmap_ptr: core::ptr::null_mut(),
            bitmap_bytes: 0,
            total_frames: 0,
            free_frames: 0,
            usable_frames: 0,
            search_hint: 0,
        }
    }

    fn bitmap(&self) -> &[u8] {
        if self.bitmap_ptr.is_null() {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(self.bitmap_ptr, self.bitmap_bytes) }
        }
    }

    fn bitmap_mut(&mut self) -> &mut [u8] {
        if self.bitmap_ptr.is_null() {
            &mut []
        } else {
            unsafe { core::slice::from_raw_parts_mut(self.bitmap_ptr, self.bitmap_bytes) }
        }
    }

    fn is_used(&self, frame: usize) -> bool {
        let byte = frame / 8;
        let bit = frame % 8;
        (self.bitmap()[byte] & (1 << bit)) != 0
    }

    fn set_used(&mut self, frame: usize) {
        let byte = frame / 8;
        let bit = frame % 8;
        let b = &mut self.bitmap_mut()[byte];
        if (*b & (1 << bit)) == 0 {
            *b |= 1 << bit;
            self.free_frames = self.free_frames.saturating_sub(1);
        }
    }

    fn set_free(&mut self, frame: usize) {
        let byte = frame / 8;
        let bit = frame % 8;
        let b = &mut self.bitmap_mut()[byte];
        if (*b & (1 << bit)) != 0 {
            *b &= !(1 << bit);
            self.free_frames += 1;
        }
    }

    fn alloc_run(&mut self, pages: usize) -> Option<PhysAddr> {
        if pages == 0 || self.free_frames < pages || self.total_frames == 0 {
            return None;
        }

        let start_search = self.search_hint.min(self.total_frames - 1);
        if let Some(frame) = self.find_free_run(start_search, self.total_frames, pages) {
            for f in frame..frame + pages {
                self.set_used(f);
            }
            self.search_hint = (frame + pages).min(self.total_frames);
            return Some(PhysAddr((frame as u64) * (PAGE_SIZE_4K as u64)));
        }

        if start_search > 0
            && let Some(frame) = self.find_free_run(0, start_search, pages)
        {
            for f in frame..frame + pages {
                self.set_used(f);
            }
            self.search_hint = (frame + pages).min(self.total_frames);
            return Some(PhysAddr((frame as u64) * (PAGE_SIZE_4K as u64)));
        }

        None
    }

    fn find_free_run(&self, start: usize, end: usize, pages: usize) -> Option<usize> {
        let mut run_start = start;
        let mut run_len = 0usize;

        for f in start..end {
            if !self.is_used(f) {
                if run_len == 0 {
                    run_start = f;
                }
                run_len += 1;
                if run_len == pages {
                    return Some(run_start);
                }
            } else {
                run_len = 0;
            }
        }
        None
    }
}

pub struct BitmapPmm {
    state: SpinLock<PmmState>,
}

impl BitmapPmm {
    pub const fn new_uninit() -> Self {
        Self {
            state: SpinLock::new(PmmState::empty()),
        }
    }

    /// Initializes the physical frame bitmap from `regions` using `hhdm_offset` to access RAM.
    ///
    /// # Safety
    /// `regions` must describe actual physical RAM mapped at `hhdm_offset + phys`.
    pub unsafe fn init(
        &self,
        regions: &[KbipMemoryRegion],
        hhdm_offset: u64,
    ) -> Result<PmmStats, &'static str> {
        let max_usable_end = regions
            .iter()
            .filter(|r| r.region_kind() == Some(KbipMemoryKind::Usable))
            .map(|r| r.phys_end())
            .max()
            .ok_or("no usable physical memory regions in KBIP")?;

        let total_frames = (max_usable_end as usize) / PAGE_SIZE_4K;
        let bitmap_bytes = total_frames.div_ceil(8);
        let bitmap_aligned = (bitmap_bytes + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);

        let placement_phys = regions
            .iter()
            .filter(|r| r.region_kind() == Some(KbipMemoryKind::Usable))
            .find_map(|r| {
                let start = r.phys_start.max(PMM_MIN_PHYS);
                let aligned_start = (start + PAGE_SIZE_4K as u64 - 1) & !(PAGE_SIZE_4K as u64 - 1);
                if r.phys_end() > aligned_start
                    && (r.phys_end() - aligned_start) >= bitmap_aligned as u64
                {
                    Some(aligned_start)
                } else {
                    None
                }
            })
            .ok_or("no usable region large enough for PMM bitmap")?;

        let bitmap_ptr = (hhdm_offset + placement_phys) as *mut u8;
        unsafe {
            // Mark every frame used (1) initially.
            core::ptr::write_bytes(bitmap_ptr, 0xFF, bitmap_bytes);
        }

        let mut guard = self.state.lock();
        guard.bitmap_ptr = bitmap_ptr;
        guard.bitmap_bytes = bitmap_bytes;
        guard.total_frames = total_frames;
        guard.free_frames = 0;
        guard.usable_frames = 0;
        guard.search_hint = (PMM_MIN_PHYS as usize) / PAGE_SIZE_4K;

        for r in regions {
            if r.region_kind() != Some(KbipMemoryKind::Usable) {
                continue;
            }
            let start = r.phys_start.max(PMM_MIN_PHYS);
            let start_frame = ((start + PAGE_SIZE_4K as u64 - 1) & !(PAGE_SIZE_4K as u64 - 1))
                as usize
                / PAGE_SIZE_4K;
            let end_frame = (r.phys_end() as usize / PAGE_SIZE_4K).min(total_frames);
            for f in start_frame..end_frame {
                guard.set_free(f);
            }
        }
        guard.usable_frames = guard.free_frames;

        // Reserve the bitmap's own frames.
        let bmp_start_frame = (placement_phys as usize) / PAGE_SIZE_4K;
        let bmp_pages = bitmap_aligned / PAGE_SIZE_4K;
        for f in bmp_start_frame..(bmp_start_frame + bmp_pages).min(total_frames) {
            guard.set_used(f);
        }

        Ok(PmmStats {
            total_usable_bytes: (guard.usable_frames as u64) * (PAGE_SIZE_4K as u64),
            free_bytes: (guard.free_frames as u64) * (PAGE_SIZE_4K as u64),
            allocated_bytes: ((guard.usable_frames - guard.free_frames) as u64)
                * (PAGE_SIZE_4K as u64),
        })
    }

    pub fn stats(&self) -> PmmStats {
        let guard = self.state.lock();
        PmmStats {
            total_usable_bytes: (guard.usable_frames as u64) * (PAGE_SIZE_4K as u64),
            free_bytes: (guard.free_frames as u64) * (PAGE_SIZE_4K as u64),
            allocated_bytes: (guard.usable_frames.saturating_sub(guard.free_frames) as u64)
                * (PAGE_SIZE_4K as u64),
        }
    }
}

impl PhysFrameAllocator for BitmapPmm {
    fn alloc_frame_zeroed(&mut self, hhdm_offset: u64) -> Option<PhysAddr> {
        self.alloc_contiguous_zeroed(1, hhdm_offset)
    }

    fn alloc_contiguous_zeroed(&mut self, pages: usize, hhdm_offset: u64) -> Option<PhysAddr> {
        let addr = self.state.lock().alloc_run(pages)?;
        unsafe {
            core::ptr::write_bytes(
                (hhdm_offset + addr.as_u64()) as *mut u8,
                0,
                pages * PAGE_SIZE_4K,
            );
        }
        Some(addr)
    }

    fn free_frame(&mut self, frame: PhysAddr) {
        let idx = (frame.as_u64() as usize) / PAGE_SIZE_4K;
        let mut guard = self.state.lock();
        if idx < guard.total_frames {
            guard.set_free(idx);
            if idx < guard.search_hint {
                guard.search_hint = idx;
            }
        }
    }
}

/// Shared reference wrapper so `&BitmapPmm` can act as [`PhysFrameAllocator`].
pub struct PmmRef<'a>(pub &'a BitmapPmm);

impl PhysFrameAllocator for PmmRef<'_> {
    fn alloc_frame_zeroed(&mut self, hhdm_offset: u64) -> Option<PhysAddr> {
        self.alloc_contiguous_zeroed(1, hhdm_offset)
    }

    fn alloc_contiguous_zeroed(&mut self, pages: usize, hhdm_offset: u64) -> Option<PhysAddr> {
        let addr = self.0.state.lock().alloc_run(pages)?;
        unsafe {
            core::ptr::write_bytes(
                (hhdm_offset + addr.as_u64()) as *mut u8,
                0,
                pages * PAGE_SIZE_4K,
            );
        }
        Some(addr)
    }

    fn free_frame(&mut self, frame: PhysAddr) {
        let idx = (frame.as_u64() as usize) / PAGE_SIZE_4K;
        let mut guard = self.0.state.lock();
        if idx < guard.total_frames {
            guard.set_free(idx);
            if idx < guard.search_hint {
                guard.search_hint = idx;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmm_alloc_contiguous_and_free() {
        // Simulate 2 MiB of usable RAM inside a heap-backed buffer mapped with hhdm_offset = base - 0x10_0000.
        let mut backing = vec![0u8; 2 * 1024 * 1024];
        let base_ptr = backing.as_mut_ptr() as u64;
        // Align simulated region at 0x10_0000..0x28_0000
        let align_off = ((base_ptr + 4095) & !4095) - base_ptr;
        let hhdm_offset = (base_ptr + align_off).wrapping_sub(0x0010_0000);

        let regions = [KbipMemoryRegion::new(
            0x0010_0000,
            0x0018_0000,
            KbipMemoryKind::Usable,
            0,
        )];

        let pmm = BitmapPmm::new_uninit();
        let stats = unsafe { pmm.init(&regions, hhdm_offset) }.unwrap();
        assert!(stats.free_bytes > 0x0017_0000);

        let mut r = PmmRef(&pmm);
        let f1 = r.alloc_frame_zeroed(hhdm_offset).unwrap();
        let f4 = r.alloc_contiguous_zeroed(4, hhdm_offset).unwrap();
        assert_ne!(f1, f4);

        let before = pmm.stats().free_bytes;
        r.free_frame(f1);
        assert_eq!(pmm.stats().free_bytes, before + PAGE_SIZE_4K as u64);
    }
}
