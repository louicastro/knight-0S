#![cfg_attr(not(test), no_std)]
//! KnightOS Hardware Abstraction Layer (HAL) trait definitions.
//!
//! Core kernel subsystems (`mm`, `sched`, `ipc`, `cap`) interact exclusively with
//! these traits and normalized types, keeping CPU/MMU/IRQ code isolated in `arch/`.

pub const PAGE_SIZE_4K: usize = 4096;
pub const PAGE_SIZE_2M: usize = 2 * 1024 * 1024;
pub const MAX_CPUS: usize = 64;

/// Physical address wrapper.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct PhysAddr(pub u64);

impl PhysAddr {
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_aligned(self, align: u64) -> bool {
        (self.0 & (align - 1)) == 0
    }

    pub const fn align_down(self, align: u64) -> Self {
        Self(self.0 & !(align - 1))
    }

    pub const fn align_up(self, align: u64) -> Self {
        Self((self.0 + align - 1) & !(align - 1))
    }
}

/// Virtual address wrapper.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct VirtAddr(pub u64);

impl VirtAddr {
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_aligned(self, align: u64) -> bool {
        (self.0 & (align - 1)) == 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CachePolicy {
    WriteBack,
    Uncached,
    DeviceMmio,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PageFlags {
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    pub global: bool,
    pub cache: CachePolicy,
}

impl PageFlags {
    pub const fn kernel_rw() -> Self {
        Self {
            readable: true,
            writable: true,
            executable: false,
            user: false,
            global: true,
            cache: CachePolicy::WriteBack,
        }
    }

    pub const fn kernel_ro() -> Self {
        Self {
            readable: true,
            writable: false,
            executable: false,
            user: false,
            global: true,
            cache: CachePolicy::WriteBack,
        }
    }

    pub const fn kernel_rx() -> Self {
        Self {
            readable: true,
            writable: false,
            executable: true,
            user: false,
            global: true,
            cache: CachePolicy::WriteBack,
        }
    }

    pub const fn mmio_rw() -> Self {
        Self {
            readable: true,
            writable: true,
            executable: false,
            user: false,
            global: true,
            cache: CachePolicy::DeviceMmio,
        }
    }

    /// Strict W^X check: a mapping may never be simultaneously writable and executable.
    pub const fn is_wx_valid(&self) -> bool {
        !(self.writable && self.executable)
    }
}

/// Physical page frame allocator interface passed to [`MmuArch`].
pub trait PhysFrameAllocator {
    fn alloc_frame_zeroed(&mut self, hhdm_offset: u64) -> Option<PhysAddr>;
    fn alloc_contiguous_zeroed(&mut self, pages: usize, hhdm_offset: u64) -> Option<PhysAddr>;
    fn free_frame(&mut self, frame: PhysAddr);
}

/// Architecture-independent MMU page-table manipulation interface.
pub trait MmuArch {
    /// Maps a single 4 KiB page using `hhdm_offset` to access page-table frames.
    fn map_4k(
        root: PhysAddr,
        hhdm_offset: u64,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<(), &'static str>;

    /// Unmaps a 4 KiB page and returns the previously mapped physical address if present.
    fn unmap_4k(root: PhysAddr, hhdm_offset: u64, virt: VirtAddr) -> Option<PhysAddr>;

    /// Removes KnightBoot's temporary lower-half identity trampoline mappings.
    fn clear_lower_half(hhdm_offset: u64);

    /// Flushes TLB entries for a single virtual page.
    fn flush_tlb_page(virt: VirtAddr);

    /// Flushes the entire non-global/current TLB.
    fn flush_tlb_all();

    /// Returns the active kernel page-table root physical address (`CR3` or `TTBR1_EL1`).
    fn active_kernel_root() -> PhysAddr;

    /// Allocates a fresh user-space page-table root (sharing kernel higher half on x86-64,
    /// or empty lower-half `TTBR0_EL1` root on ARM64).
    fn new_user_root(
        hhdm_offset: u64,
        alloc: &mut dyn PhysFrameAllocator,
    ) -> Result<PhysAddr, &'static str>;

    /// Switches the active user-space address space root (`CR3` or `TTBR0_EL1`).
    fn switch_user_root(root: PhysAddr);
}

/// Architecture-independent syscall dispatch callback registered by `kernel`.
pub type SyscallDispatchFn =
    extern "C" fn(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64;

/// Architecture-independent timer tick callback for preemptive scheduling.
pub type TimerTickFn = extern "C" fn();

/// Architecture-independent early serial UART interface.
pub trait SerialArch {
    fn init(hhdm_offset: u64, base_phys_override: u64);
    fn write_byte(byte: u8);
    fn write_str(s: &str) {
        for b in s.bytes() {
            if b == b'\n' {
                Self::write_byte(b'\r');
            }
            Self::write_byte(b);
        }
    }
}

/// CPU control & synchronization primitives.
pub trait CpuArch {
    fn arch_name() -> &'static str;
    fn current_hw_id() -> u32;
    fn enable_interrupts();
    fn disable_interrupts();
    fn halt_once();
    fn halt_forever() -> ! {
        Self::disable_interrupts();
        loop {
            Self::halt_once();
        }
    }
}

/// Normalized CPU core descriptor discovered from ACPI MADT or ARM64 DTB.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuCoreDesc {
    pub logical_id: u32,
    pub hw_id: u64,
    pub is_bsp: bool,
    pub enabled: bool,
}

/// Discovered platform IOMMU engine type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IommuKind {
    None,
    IntelVtd,
    AmdVi,
    ArmSmmuv3,
}

/// Architecture-independent IOMMU domain isolation contract.
pub trait IommuArch {
    fn kind() -> IommuKind;
    fn init_domain(iommu_phys: u64, hhdm_offset: u64) -> bool;
}

/// Normalized platform hardware topology discovered at boot.
#[derive(Clone, Copy, Debug)]
pub struct PlatformTopology {
    pub cpu_count: usize,
    pub cpus: [CpuCoreDesc; MAX_CPUS],
    pub irq_controller_phys: u64,
    pub irq_redistributor_phys: u64,
    pub ioapic_phys: u64,
    pub pcie_ecam_phys: u64,
    pub iommu_phys: u64,
    pub iommu_kind: IommuKind,
    pub uart_phys: u64,
    pub psci_use_hvc: bool,
}

impl PlatformTopology {
    pub const fn empty() -> Self {
        Self {
            cpu_count: 0,
            cpus: [CpuCoreDesc {
                logical_id: 0,
                hw_id: 0,
                is_bsp: false,
                enabled: false,
            }; MAX_CPUS],
            irq_controller_phys: 0,
            irq_redistributor_phys: 0,
            ioapic_phys: 0,
            pcie_ecam_phys: 0,
            iommu_phys: 0,
            iommu_kind: IommuKind::None,
            uart_phys: 0,
            psci_use_hvc: true,
        }
    }
}
