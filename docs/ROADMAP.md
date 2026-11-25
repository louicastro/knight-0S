# KnightOS Engineering Roadmap

This roadmap tracks KnightOS from architectural foundation through bare-metal bring-up, desktop UI, hardware drivers, and Windows/Linux application compatibility.

---

## Phase 1 — Foundation & System Architecture *(Current Phase — Complete)*
- Evaluate and select kernel architecture (Capability-based microkernel + async shared-memory rings).
- Define language policy (Rust core, minimal Assembly, scoped C/C++).
- Specify x86-64 and ARM64 HAL isolation and boot protocols (`knightboot` + `KBIP`).
- Establish repository directory structure and toolchain verification scripts (`tools/`).
- Author core architectural, security, boot, hardware, and ADR documentation (`docs/`).

---

## Phase 2 — Bootloader, HAL & Early Kernel Bring-Up *(Next)*
**Objective:** Boot an original KnightOS kernel from UEFI into a verified 64-bit higher-half environment with SMP, memory management, and kernel unit/QEMU tests on **both** x86-64 and ARM64.

### Phase 2 Implementation Plan
1. **Build Automation (`tools/xtask`):**
   - Configure workspace `Cargo.toml`, linker scripts (`arch/x86_64/linker.ld`, `arch/aarch64/linker.ld`), and `xtask` ESP image builder + QEMU runner.
2. **KBIP Protocol & `knightboot` (`boot/protocol`, `boot/knightboot`):**
   - Implement `#![no_std]` UEFI application for `x86_64-unknown-uefi` and `aarch64-unknown-uefi`.
   - Load `kernel.elf`, configure GOP framebuffer, collect ACPI RSDP / DTB, execute `ExitBootServices()` retry loop, set up initial higher-half page tables, and jump to `_start(kbip)`.
3. **Early HAL & Diagnostics (`kernel/hal`, `arch/x86_64`, `arch/aarch64`):**
   - Early UART (`COM1` and `PL011`) + GOP framebuffer bitmap font console.
   - Exception/interrupt vector tables (`GDT`/`IDT`/`TSS` on x86-64; `EL2->EL1` drop + `VBAR_EL1` on ARM64) and deterministic panic handler.
4. **Physical & Virtual Memory (`mm/pmm`, `mm/vmm`, `mm/slab`):**
   - Parse `KbipMemoryRegion` into a buddy + bitmap physical frame allocator.
   - Implement `MmuArch` 4-level paging (`CR3`) and `TTBR0_EL1`/`TTBR1_EL1` with strict `W^X` and higher-half direct mapping.
   - Stand up kernel slab/heap allocator so `alloc` works cleanly in kernel space.
5. **ACPI/DTB Discovery, Timers, Interrupts & SMP:**
   - Parse ACPI `MADT` (x86-64) and `DTB`/`MADT` (ARM64).
   - Bring up `x2APIC`/`IOAPIC` + TSC/APIC timer and `GICv3` + Generic Timer.
   - Wake secondary cores via `INIT-SIPI-SIPI` and `PSCI CPU_ON`, verified via automated QEMU SMP boot tests (`tests/qemu`).

---

## Phase 3 — Microkernel Core, IPC, Scheduler & User-Space Bring-Up
- Implement `CNode` capability tables, handle rights/attenuation, and `Task` / `Thread` lifecycles.
- Implement `SYSCALL`/`SYSRET` and `SVC` user-mode transitions + `libknight` syscall wrappers.
- Implement channels, `Port`/`WaitSet` event multiplexing, `futex`, and shared-memory VMOs.
- Implement SMP preemptive scheduler (RT + EEVDF-inspired interactive classes).
- Spawn `initd` and `devmgr` from `initrd.kpkg` in user space.
- Implement VirtIO user-space drivers (`virtio-blk`, `virtio-input`, `virtio-net`, `virtio-gpu`) and initial `VFS` + `KnightFS` read/write support.

---

## Phase 4 — Desktop Shell, Real Hardware & Storage
- Implement `KnightCompositor` (GPU-accelerated retained scene graph, vibrancy/glass shaders, damage tracking, HiDPI, VRR).
- Build `ui-kit` and core native apps (`apps/shell` dock/menubar/launcher, `apps/terminal`, `apps/settings`, file manager).
- Enable bare-metal PC/ARM64 hardware drivers: PCIe NVMe, xHCI USB, HID, Intel/Realtek Ethernet, HDA Audio (`audiod`), ACPI power management.
- Implement IOMMU enforcement (`VT-d`/`AMD-Vi`/`SMMUv3`) for user-space driver domains and A/B atomic OS updates with `KnightFS` snapshots and Zstd/LZ4 compression.

---

## Phase 5 — PC Gaming Stack & Multi-OS App Compatibility
- Implement explicit `gpu-hal` + Vulkan-class KMD/UMD pipeline for discrete & integrated GPUs (AMD RDNA first, Intel Xe/NVIDIA) with fullscreen direct scanout bypass.
- Implement **`compat-linux`:** User-space Linux ELF loader, syscall translator (`futex`, `epoll`, `clone`, `mmap`), dynamic linker support, and Wayland-to-KnightCompositor bridge.
- Implement **`compat-win`:** User-space Windows PE (`.exe`/`.dll`) loader, NT/Win32 API server, isolated virtual prefix/registry, and Direct3D 9/10/11/12 translation to `gpu-hal`.
- Performance tuning: frame-pacing telemetry, CPU P/E-core scheduling heuristics, and low-latency audio/input pipelines.
