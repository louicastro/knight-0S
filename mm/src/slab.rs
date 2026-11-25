//! Kernel slab & page-backed [`GlobalAlloc`] (`KnightKernelHeap`).

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use knightos_hal::{PAGE_SIZE_4K, PhysAddr, PhysFrameAllocator};

use crate::pmm::{BitmapPmm, PmmRef};

const SLAB_CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

#[repr(C)]
struct FreeNode {
    next: *mut FreeNode,
}

#[repr(C)]
struct LargeHeader {
    pages: usize,
    phys: u64,
}

struct SlabState {
    bins: [*mut FreeNode; SLAB_CLASSES.len()],
}

unsafe impl Send for SlabState {}

impl SlabState {
    const fn new() -> Self {
        Self {
            bins: [null_mut(); SLAB_CLASSES.len()],
        }
    }
}

pub struct KnightKernelHeap {
    locked: AtomicBool,
    hhdm_offset: AtomicU64,
    pmm: AtomicPtr<BitmapPmm>,
    state: core::cell::UnsafeCell<SlabState>,
}

unsafe impl Sync for KnightKernelHeap {}
unsafe impl Send for KnightKernelHeap {}

impl KnightKernelHeap {
    pub const fn new_uninit() -> Self {
        Self {
            locked: AtomicBool::new(false),
            hhdm_offset: AtomicU64::new(0),
            pmm: AtomicPtr::new(null_mut()),
            state: core::cell::UnsafeCell::new(SlabState::new()),
        }
    }

    /// Binds this global heap allocator to an initialized [`BitmapPmm`] and `hhdm_offset`.
    pub unsafe fn init(&self, pmm: &'static BitmapPmm, hhdm_offset: u64) {
        self.acquire();
        self.pmm.store((pmm as *const BitmapPmm).cast_mut(), Ordering::Release);
        self.hhdm_offset.store(hhdm_offset, Ordering::Release);
        self.release();
    }

    fn acquire(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn release(&self) {
        self.locked.store(false, Ordering::Release);
    }

    fn bin_index(layout: Layout) -> Option<usize> {
        let req = layout.size().max(layout.align()).max(core::mem::size_of::<FreeNode>());
        SLAB_CLASSES.iter().position(|&sz| sz >= req)
    }
}

unsafe impl GlobalAlloc for KnightKernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.acquire();
        let hhdm = self.hhdm_offset.load(Ordering::Acquire);
        let pmm_ptr = self.pmm.load(Ordering::Acquire);
        if pmm_ptr.is_null() {
            self.release();
            return null_mut();
        }
        let mut pmm_ref = PmmRef(unsafe { &*pmm_ptr });
        let state = unsafe { &mut *self.state.get() };

        let result = if let Some(idx) = Self::bin_index(layout) {
            if state.bins[idx].is_null() {
                if let Some(page_phys) = pmm_ref.alloc_frame_zeroed(hhdm) {
                    let obj_size = SLAB_CLASSES[idx];
                    let page_virt = (hhdm + page_phys.as_u64()) as usize;
                    let count = PAGE_SIZE_4K / obj_size;
                    for i in 0..count {
                        let node = (page_virt + i * obj_size) as *mut FreeNode;
                        unsafe {
                            (*node).next = state.bins[idx];
                        }
                        state.bins[idx] = node;
                    }
                }
            }
            let head = state.bins[idx];
            if !head.is_null() {
                state.bins[idx] = unsafe { (*head).next };
                head as *mut u8
            } else {
                null_mut()
            }
        } else {
            // Large allocation: place LargeHeader immediately before aligned data pointer.
            let align = layout.align().max(core::mem::align_of::<LargeHeader>());
            let header_pad = align.max(core::mem::size_of::<LargeHeader>());
            let total = layout.size() + header_pad;
            let pages = total.div_ceil(PAGE_SIZE_4K);
            if let Some(phys) = pmm_ref.alloc_contiguous_zeroed(pages, hhdm) {
                let base = hhdm + phys.as_u64();
                let data_start = (base + core::mem::size_of::<LargeHeader>() as u64 + align as u64 - 1)
                    & !(align as u64 - 1);
                let hdr_virt = (data_start - core::mem::size_of::<LargeHeader>() as u64) as *mut LargeHeader;
                unsafe {
                    core::ptr::write(
                        hdr_virt,
                        LargeHeader {
                            pages,
                            phys: phys.as_u64(),
                        },
                    );
                }
                data_start as *mut u8
            } else {
                null_mut()
            }
        };

        self.release();
        result
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        self.acquire();
        let pmm_ptr = self.pmm.load(Ordering::Acquire);
        if pmm_ptr.is_null() {
            self.release();
            return;
        }
        let mut pmm_ref = PmmRef(unsafe { &*pmm_ptr });
        let state = unsafe { &mut *self.state.get() };

        if let Some(idx) = Self::bin_index(layout) {
            let node = ptr as *mut FreeNode;
            unsafe {
                (*node).next = state.bins[idx];
            }
            state.bins[idx] = node;
        } else {
            let hdr_virt = ((ptr as u64) - core::mem::size_of::<LargeHeader>() as u64) as *const LargeHeader;
            let hdr = unsafe { core::ptr::read(hdr_virt) };
            for i in 0..hdr.pages {
                pmm_ref.free_frame(PhysAddr(hdr.phys + (i * PAGE_SIZE_4K) as u64));
            }
        }

        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightboot_protocol::{KbipMemoryKind, KbipMemoryRegion};

    #[test]
    fn slab_alloc_and_dealloc_small_and_large() {
        let mut backing = vec![0u8; 2 * 1024 * 1024];
        let base_ptr = backing.as_mut_ptr() as u64;
        let align_off = ((base_ptr + 4095) & !4095) - base_ptr;
        let hhdm_offset = (base_ptr + align_off).wrapping_sub(0x0010_0000);
        let regions = [KbipMemoryRegion::new(
            0x0010_0000,
            0x0018_0000,
            KbipMemoryKind::Usable,
            0,
        )];

        static TEST_PMM: BitmapPmm = BitmapPmm::new_uninit();
        unsafe { TEST_PMM.init(&regions, hhdm_offset) }.unwrap();

        let heap = KnightKernelHeap::new_uninit();
        unsafe { heap.init(&TEST_PMM, hhdm_offset) };

        let small_layout = Layout::from_size_align(64, 8).unwrap();
        let p1 = unsafe { heap.alloc(small_layout) };
        assert!(!p1.is_null());
        unsafe {
            core::ptr::write_bytes(p1, 0xAB, 64);
            heap.dealloc(p1, small_layout);
        }

        let large_layout = Layout::from_size_align(8192, 4096).unwrap();
        let p2 = unsafe { heap.alloc(large_layout) };
        assert!(!p2.is_null());
        unsafe {
            core::ptr::write_bytes(p2, 0xCD, 8192);
            heap.dealloc(p2, large_layout);
        }
    }
}
