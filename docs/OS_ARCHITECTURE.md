# KnightOS System Architecture

This document defines the end-to-end architecture of **KnightOS** across all core subsystems. Architectural choices and trade-offs are recorded in [DECISIONS.md](DECISIONS.md).

---

## 1. High-Level Layered Topology

KnightOS uses a **capability-based microkernel with asynchronous shared-memory fastpaths and grouped user-space driver domains**.

```text
+-----------------------------------------------------------------------------------+
|                              User-Space Applications                              |
|  [ Native KnightOS Apps ]   [ Linux ELF Compat Layer ]   [ Windows PE (.exe) ]    |
+-----------------------------------------------------------------------------------+
|                          User-Space Subsystems & Services                         |
|  [ KnightCompositor + GPU UMD ]  [ VFS / KnightFS ]  [ Netd / Audiod / Powerd ]   |
|  [ Driver Host Domains: Storage (NVMe/VirtIO), GPU KMD, Input, USB, Network ]     |
+===================================================================================+
|                     KnightOS Syscall & Capability ABI Boundary                    |
+===================================================================================+
|                         KnightOS Microkernel (Privileged)                         |
|  [ Capability Engine ]  [ Sync & Async Ring IPC ]  [ SMP Preemptive Scheduler ]   |
|  [ VMO / Address-Space Manager ]  [ IRQ/MMIO/IOMMU Broker ]                       |
+-----------------------------------------------------------------------------------+
|                        Hardware Abstraction Layer (HAL)                           |
|          [ arch/x86_64 ]                       [ arch/aarch64 ]                   |
|   UEFI/ACPI, GDT/IDT, CR3, APIC, VT-d    UEFI/ACPI+DTB, EL1/EL2, TTBR, GIC, SMMU  |
+-----------------------------------------------------------------------------------+
```

---

## 2. Kernel Architecture Summary

- **Model:** Capability-based microkernel (see [KERNEL_DESIGN.md](KERNEL_DESIGN.md) for the full microkernel vs. modular monolithic evaluation).
- **Trusted Computing Base (TCB):** Strictly limited to physical/virtual memory management (`VMO` & page tables), capability enforcement, thread scheduling, synchronous/asynchronous IPC, and minimal interrupt/DMA brokering.
- **No Ambient Authority:** Processes hold no global filesystem or UID-0 root privileges by default. Every resource—memory regions, channels, IRQ lines, MMIO BARs, GPU queues—is accessed via unforgeable kernel capabilities.

---

## 3. Hardware Abstraction Layer (HAL)

The kernel core never executes CPU-specific instructions or touches architecture registers directly. Instead, `kernel/hal/` defines compile-time Rust traits and normalized data types implemented separately under `arch/x86_64/` and `arch/aarch64/`:

- **`CpuArch`:** Privilege transition, syscall/exception entry/exit, context switch, TLS base (`FS`/`GS` vs. `TPIDR_EL0`/`TPIDR_EL1`), CPU feature probing, memory barriers (`DMB`/`DSB`/`ISB` vs. x86 fences).
- **`MmuArch`:** Multi-level page table manipulation, ASID/PCID invalidation, `W^X` flag enforcement, cache coherency ops.
- **`InterruptArch`:** Local APIC/x2APIC + IOAPIC vs. GICv2/v3/v4, MSI/MSI-X routing, per-CPU timer programming (`TSC-deadline` vs. ARM Generic Timer).
- **`IommuArch`:** Intel VT-d / AMD-Vi vs. ARM SMMUv3 domain creation and DMA page-table isolation for user-space drivers.
- **`PlatformDiscovery`:** Normalized topology extracted from ACPI (`MADT`, `MCFG`, `FADT`, `IORT`/`DMAR`) or Flattened Device Tree (`DTB`) on ARM64 SoCs.

---

## 4. Boot Architecture

- **Firmware Standard:** UEFI 2.8+ on both x86-64 and ARM64.
- **Bootloader:** Custom Rust UEFI bootloader (`knightboot`) generating `BOOTX64.EFI` and `BOOTAA64.EFI`.
- **Handoff Contract:** Versioned **KnightBoot Info Protocol (KBIP)** passing normalized memory descriptors, GOP framebuffer metadata, ACPI RSDP / DTB physical pointer, KASLR seed, and preloaded `initrd` boot bundle.
- Full specification in [BOOT_PROCESS.md](BOOT_PROCESS.md).

---

## 5. Memory Management Architecture

KnightOS splits memory management into low-level physical/virtual kernel primitives (`mm/`) and user-space policy:

1. **Physical Memory Manager (`mm/pmm`):**
   - Multi-order buddy allocator seeded from the KBIP memory map, paired with per-NUMA-node/zone bitmaps for fast 4 KiB, 2 MiB, and 1 GiB huge-page allocation.
   - Tracks page state (free, kernel, VMO-backed, DMA-pinned) without dynamic heap allocations during early boot.
2. **Kernel Allocators (`mm/slab`):**
   - Per-CPU magazine/slab allocator for fixed-size kernel objects (threads, capabilities, VMOs, endpoints) to minimize lock contention and fragmentation.
3. **Virtual Memory Objects (`VMO`):**
   - First-class capability-backed memory objects representing anonymous RAM, zero-copy shared IPC buffers, file-backed page cache slices, or contiguous DMA/VRAM mappings.
   - Supports copy-on-write (CoW) clones (critical for efficient process spawning and Linux `fork()` compatibility), demand paging, and page-pinning for IOMMU DMA.
4. **Virtual Memory Manager (`mm/vmm`):**
   - Higher-half kernel mapping with strict `W^X` (no page is simultaneously writable and executable).
   - User address spaces (`Vmar` — Virtual Memory Address Regions) managed cleanly with guard pages and ASLR.
   - Unified page cache backed by VMOs so VFS and applications share physical pages with zero double-buffering—preserving low RAM consumption.

---

## 6. Process & Thread Management Architecture

- **Process (`Task`):** An isolated container owning a virtual address space (`Vmar`), a capability handle table (`CNode`), resource quotas (RAM, CPU weight), and one or more threads. Processes have no implicit parent-child privileges beyond explicitly delegated capabilities.
- **Thread:** The unit of kernel scheduling with its own register state, kernel stack, priority/scheduling class, and TLS pointer.
- **Scheduling Classes:**
  1. **Realtime / Audio-Video Isochronous (`RT`):** Bounded-latency deadlines for audio DSP and VR/frame-pacing threads.
  2. **Interactive / Gaming (`Interactive`):** EEVDF-inspired virtual-deadline scheduler with latency-sensitivity boosting for foreground UI and active game render/input loops.
  3. **Normal (`Normal`):** Fair-share CPU distribution across background services and apps.
  4. **Background / Maintenance (`Idle`):** Compression, indexing, and background updates throttled when interactive demand exists.
- **SMP:** Per-CPU runqueues with cache-aware load balancing and bounded work-stealing.

---

## 7. Filesystem Architecture

To meet the twin goals of **low storage footprint** and **high reliability**, KnightOS separates the VFS routing daemon from filesystem implementations:

1. **User-Space VFS (`fs/vfs`):**
   - Namespace/mount router exposed via capability channels and **shared-memory I/O rings** (for streaming throughput without per-read IPC overhead).
   - Interacts directly with kernel-managed VMOs so file data enters the unified page cache once.
2. **Native Filesystem — `KnightFS` (`fs/knightfs`):**
   - Copy-on-Write (CoW) B+-tree metadata and extent-based data layout.
   - Inline transparent compression (`LZ4` for hot latency-sensitive blocks, `Zstd` for system/cold data) to minimize SSD footprint.
   - Checksums on metadata and optional data checksums, atomic snapshots for safe rollbacks/updates, and TRIM/discard awareness.
3. **Interoperability Filesystems (`fs/fat32`, `fs/compat`):**
   - `FAT32` (required for UEFI ESP management and removable media).
   - Read/write `ext4` and `NTFS`/`exFAT` user-space drivers for dual-boot data exchange and compatibility runtimes.

---

## 8. Driver Architecture

Drivers run in **unprivileged user-space Driver Host Domains** rather than kernel Ring 0 / EL1:

- **Capability Grants:** `devmgr` probes buses (PCIe, ACPI, DTB, USB) via a narrow bus capability and spawns driver hosts with **only** the exact MMIO BAR VMOs, IRQ wait objects, and IOMMU-restricted DMA capabilities for their assigned device.
- **Grouping for Efficiency:** To avoid microkernel RAM bloat and excessive context switches, tightly related low-risk drivers (e.g., HID input + basic timers) can share a sandboxed domain, while high-risk or complex drivers (GPU, NVMe, Wi-Fi/NIC, USB xHCI) run in dedicated isolated domains.
- **Crash Recovery:** If a GPU or audio driver crashes, `devmgr` revokes its capabilities, resets the device/IOMMU domain, and restarts the driver host without panicking the kernel.
- **Bring-Up Strategy:** VirtIO (`virtio-blk`, `virtio-net`, `virtio-gpu`, `virtio-input`) first in QEMU, followed by standard NVMe, xHCI, AHCI, HDA, and vendor GPU drivers.

---

## 9. Graphics & Desktop Architecture

Inspired by the visual clarity and smoothness of modern macOS desktops while staying architected for PC gaming:

1. **GPU Hardware Abstraction (`graphics/gpu-hal`):**
   - Explicit, low-overhead command-submission architecture aligned with Vulkan/modern GPU hardware queues (command buffers, fences/timelines, explicit memory residency).
   - Hardware-isolated per-application GPU contexts mapped via GPU MMU/IOMMU.
2. **KnightCompositor (`graphics/compositor`):**
   - Scene-graph retained compositor supporting hardware overlays, sub-pixel antialiasing, wide color gamut / HDR pipelines, damage-region tracking, and adaptive refresh rates (VRR / 120 Hz+).
   - Implements frosted-glass/vibrancy shaders, smooth spring physics animations, and unified window shadows cleanly on the GPU.
   - **Gaming Direct Scanout:** Fullscreen or borderless-fullscreen games bypass composition overhead whenever possible, flipping swapchain buffers directly to the display controller with minimal input-to-photon latency.
3. **Native UI Kit (`graphics/ui-kit`):**
   - Retained-mode GPU-accelerated widget toolkit for native KnightOS apps (`apps/shell`, `apps/settings`, `apps/terminal`), ensuring visual coherence and low RAM usage.

---

## 10. Security Architecture

Security is enforced structurally across layers (detailed in [SECURITY_DESIGN.md](SECURITY_DESIGN.md)):

- **Capability-Based Isolation:** Applications receive explicit, attenuable capabilities (e.g., read-only access to one user-chosen folder via a system file picker portal).
- **Memory & CPU Mitigations:** Strict `W^X`, kernel/user KASLR, guard pages, `SMEP`/`SMAP` (x86-64), `PAN`/`PXN` (ARM64), CFI/stack canaries.
- **DMA Isolation:** Mandatory IOMMU (`VT-d`/`AMD-Vi`/`SMMUv3`) for user-space drivers; no untrusted device can DMA into kernel or foreign process RAM.
- **System Integrity:** Signed, compressed, read-only base OS image with atomic A/B partitions and verified boot path (`UEFI Secure Boot` -> signed `knightboot` -> signed kernel & `initrd`).

---

## 11. Application Compatibility Architecture

KnightOS supports three application tiers without polluting the core kernel ABI:

| Tier | Target Format | Runtime Mechanism | Graphics & Audio Path |
| :--- | :--- | :--- | :--- |
| **1. Native** | KnightOS ELF + `libknight` | Direct kernel syscalls + capability IPC | Native `KnightGPU` / `Vulkan` + `KnightCompositor` + `audiod` |
| **2. Linux** | Standard Linux `x86_64` / `aarch64` ELF | User-space `compat-linux` personality translating Linux syscalls (`futex`, `epoll`, `mmap`, `clone`) to KnightOS VMOs/IPC; dynamic linker + Wayland-to-KnightCompositor bridge | Native Vulkan ICD / Mesa Zink -> `KnightGPU`; PipeWire/PulseAudio shim -> `audiod` |
| **3. Windows** | Windows PE (`.exe` / `.dll`) | User-space `compat-win` personality (PE loader, NT/Win32 API server, isolated virtual registry & drive-letter VFS view, SEH/TEB/PEB emulation; `x86_64` native or WoW64/FEX-style translation where needed) | Direct3D 9/10/11/12 & DXGI translation layer -> `KnightGPU` explicit command backend; WASAPI/XAudio shim -> `audiod` |

### Why User-Space Compatibility Personalities?
Keeping Linux and Windows syscall translation in user-space personality servers (assisted by fast kernel primitives like VMO aliasing, `WaitMany`/futex-wait primitives, and user-mode trap/syscall redirection) ensures:
1. The kernel stays small, clean, and independent—never inheriting Linux or Windows kernel technical debt.
2. Compatibility layers can be updated, sandboxed per-application, or omitted entirely on minimal installs to save storage and RAM.
