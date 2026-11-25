# KnightOS Hardware Support & Processor Architecture Plan

KnightOS treats **x86-64** and **ARM64 (AArch64)** as first-class architectures from day one without assuming identical boot flows, memory ordering, interrupt hardware, or firmware interfaces.

---

## 1. Architecture Comparison Matrix

| Subsystem | x86-64 (AMD64 / Intel 64) | ARM64 (AArch64 / ARMv8.0+) | HAL Abstraction Strategy |
| :--- | :--- | :--- | :--- |
| **Firmware & Discovery** | UEFI 2.8+ & **ACPI** (`RSDP`, `MADT`, `MCFG`, `FADT`, `DMAR`) | UEFI 2.8+ with **ACPI (SBBR)** OR **Flattened Device Tree (`DTB`)** | `PlatformDiscovery` produces normalized `CpuTopology`, `IrqChipDesc`, `PcieEcam`, `IommuDesc` |
| **Privilege Rings / ELs** | Ring 0 (Kernel), Ring 3 (User) | **EL1** (Kernel), **EL0** (User); may boot in **EL2** and drop to EL1 | `CpuArch` hides ring/EL details behind kernel/user transitions |
| **Syscall & Trap Entry** | `SYSCALL`/`SYSRET`, `IDT` exceptions, `SWAPGS` + `TSS.IST` | `SVC #0`/`ERET`, `VBAR_EL1` vector table, `SP_EL0`/`SP_EL1` | Separate trap stubs in `arch/x86_64` & `arch/aarch64` calling common Rust `syscall_dispatch()` / `trap_dispatch()` |
| **Virtual Memory & TLB** | 4-level (48-bit) or 5-level (57-bit) paging; single `CR3` + `PCID`; `PAT` | Split `TTBR0_EL1` (user) + `TTBR1_EL1` (kernel); 4 KiB/16 KiB granules; `ASID`; `MAIR_EL1` | `MmuArch` trait exposes `AddressSpace`, `map_page`, `unmap_page`, `flush_tlb`, `PageFlags` |
| **Memory Ordering** | Strong **TSO** (Total Store Order); DMA/MMIO still needs fences/uncached attrs | **Weakly ordered**; requires explicit `DMB`/`DSB`/`ISB` and explicit I-cache invalidation (`IC IVAU`) | Portable `hal::fence` & `hal::cache` primitives (zero-cost no-op on x86 where TSO suffices, real barriers on ARM64) |
| **Interrupt Controller** | Local **APIC / x2APIC** + **IOAPIC** + MSI/MSI-X | **GICv2 / GICv3 / GICv4** (+ ITS for MSI/MSI-X) | `InterruptArch` trait (`ack`, `eoi`, `enable_irq`, `send_ipi`, `route_msi`) |
| **Timers & Clocks** | Invariant **TSC** + APIC Timer (fallback HPET) | **ARM Generic Timer** (`CNTVCT_EL0`, `CNTP_TVAL_EL0`) | `TimerArch` trait (`monotonic_ticks`, `ticks_per_sec`, `arm_oneshot`) |
| **Secondary CPU Wakeup** | `INIT`–`SIPI`–`SIPI` via APIC + 16-bit real-mode trampoline | **PSCI (`CPU_ON`)** via `SMC`/`HVC` (or spin-table) directly in 64-bit EL1/EL2 | `SmpArch::start_cpu(cpu_id, entry_phys, stack_top)` |
| **IOMMU (DMA Isolation)** | Intel **VT-d** / AMD **AMD-Vi** | ARM **SMMUv3** | `IommuArch` trait (`create_domain`, `map_dma`, `unmap_dma`) |
| **Legacy I/O** | x86 Port I/O (`IN`/`OUT`) for legacy UART/PCI config fallback | Strictly **MMIO only** (no I/O port space) | Never expose port I/O in common kernel; isolate legacy ports in `arch/x86_64` |

---

## 2. Isolation Rules for Architecture-Specific Code

To prevent `#ifdef` spaghetti and hidden platform coupling:

1. **Zero `#[cfg(target_arch)]` inside `kernel/sched`, `kernel/cap`, `kernel/ipc`, or `mm/` logic** (except selecting the concrete HAL implementation module at `kernel/hal/mod.rs`).
2. **All assembly lives strictly in `arch/<target>/`** (or tightly audited `core::arch::asm!` blocks inside `arch/<target>/`).
3. **Common Virtual Address Layout Contract:** Both architectures map the kernel into the upper canonical half (`>= 0xFFFF_8000_0000_0000`) and user spaces into the lower canonical half (`0x0000_0000_0001_0000` .. `0x0000_7FFF_FFFF_F000`), leaving a non-canonical/unmapped guard gap.

---

## 3. Baseline ISA Requirements

- **x86-64 Baseline (`x86-64-v2` minimum, runtime feature dispatch for v3/AVX2/AVX-512):**
  - Requires `SSE2`, `NX`, `PAE`, `FXSR`, invariant `TSC` (or HPET fallback), `CMPXCHG16B`.
  - Dynamically enables `SMEP`, `SMAP`, `PCID`, `FSGSBASE`, `XSAVE`/`AVX`/`AVX2`, `UMIP`, and `x2APIC` when supported by `CPUID`.
- **ARM64 Baseline (`ARMv8.0-A` minimum):**
  - Requires 4 KiB page granule support, NEON/ASIMD, Generic Timer, GICv2 or GICv3.
  - Dynamically enables `PAN`, `UAO`, `LSE` atomics (ARMv8.1), `BTI`/`PAC` (ARMv8.3/8.5), and `RNDR` when reported by `ID_AA64*` registers.

---

## 4. Phased Hardware Enablement Plan

### Tier 1 — Virtualized Bring-Up (Phase 2)
- **QEMU `q35` (x86-64 + OVMF UEFI):** Serial `COM1`, GOP framebuffer, x2APIC/IOAPIC, PCIe ECAM, `virtio-blk`, `virtio-net`, `virtio-gpu`, `virtio-input`.
- **QEMU `virt` (ARM64 + AAVMF/EDK2 UEFI):** `PL011` UART, `ramfb`/GOP, GICv3, PSCI SMP, Generic Timer, PCIe ECAM, VirtIO devices.

### Tier 2 — Standard Desktop & Laptop Hardware (Phases 3–4)
- **Storage:** PCIe **NVMe** (multi-queue mapped to per-CPU cores) and **AHCI/SATA**.
- **USB & Input:** **xHCI** (USB 3.x), HID keyboards, mice, game controllers.
- **Networking:** Intel/Realtek Ethernet (`e1000e`, `igc`, `r8169`) and modern Wi-Fi stacks.
- **Audio:** Intel HD Audio (`HDA`) and USB Audio Class 2.0.
- **Power & Thermal:** ACPI S0ix/S3 sleep, CPU P-states/C-states, battery/fan sensors.

### Tier 3 — High-Performance PC Gaming GPUs (Phases 4–5)
- **GPUs:** Explicit Vulkan-class kernel mode drivers (KMD) + user-space drivers (UMD) prioritizing **AMD RDNA** (open hardware documentation), **Intel Arc/Xe**, and **NVIDIA** (via open GPU firmware/GSP path) with full DisplayPort/HDMI/VRR support.
- **ARM64 Desktop/SBC Targets:** SystemReady SR/ES compliant ARM64 workstations and documented SBCs/laptops via DTB/ACPI.
