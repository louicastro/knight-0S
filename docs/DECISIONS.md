# KnightOS Architecture Decision Records (ADRs)

This log captures every foundational technical decision made during Phase 1, including considered alternatives, trade-offs, and rationales.

---

## ADR-001: Capability-Based Microkernel with Shared-Memory Fastpaths
- **Status:** Accepted
- **Context:** KnightOS needs high stability and security (desktop & untrusted apps/drivers), low RAM/storage footprint, multi-architecture portability, **and** low-latency PC gaming + Windows/Linux app compatibility.
- **Options Considered:**
  1. *Modular Monolithic Kernel:* Fast in-kernel VFS/driver calls, smaller initial LOC to reach user space, but large TCB where any buggy GPU/Wi-Fi/USB driver crashes or compromises Ring 0 / EL1.
  2. *Pure Minimalist Microkernel:* Tiny TCB and strict isolation, but naive synchronous IPC for every I/O/frame hurts gaming and duplicates caches across servers.
  3. *Capability-Based Microkernel + Async Shared-Memory Rings + Grouped Driver Domains:* Tiny Rust core managing `VMO`s, `Vmar`s, `CNode`s, `Sched`, and `IPC`; drivers/VFS/compositor/compat layers run in user space using zero-copy shared-memory rings and unified kernel VMOs.
- **Decision:** **Option 3.**
- **Consequences:** Slightly steeper Phase 3 driver/IPC infrastructure work, repaid by crash-recoverable user-space drivers, zero ambient authority, clean user-space Windows/Linux compatibility isolation, and full-speed shared-memory I/O & GPU command submission.

---

## ADR-002: Programming Language Strategy
- **Status:** Accepted
- **Context:** Select implementation languages balancing memory safety, bare-metal control, ecosystem interoperability, and maintainability.
- **Options Considered:**
  1. *Pure C + Assembly:* Mature tooling, fast compile times, but chronically vulnerable to UAF, buffer overflows, and data races in complex kernel/driver code.
  2. *Pure Rust + Assembly:* Compile-time memory/thread safety, strong enums/traits for HAL, great `no_std` support; minor friction only at C-ABI/GPU/compat ecosystem edges.
  3. *Rust (Primary) + Assembly (Minimal CPU Stubs) + C/C++ (Scoped Ecosystem/Compat Edges):*
- **Decision:** **Option 3:**
  - **Rust (`no_std` / `std`):** Kernel core, `knightboot`, memory manager, scheduler, native user-space drivers, `KnightFS`, `KnightCompositor`, and native services/apps.
  - **Assembly (NASM for x86-64 real-mode/trampolines, GAS/inline `asm!` for x86-64 & AArch64):** Strictly <5% of code—early boot trampolines, trap/syscall entry, context-switch frames, cache/TLB barriers.
  - **C (C17):** Thin C ABI (`libknight-sys`) for third-party C ports and select hardware/ACPI/firmware shims only where Rust equivalents are unavailable or impractical.
  - **C++ (C++20):** Forbidden in kernel/TCB; allowed strictly in user-space compatibility/shader translation layers (e.g., D3D/Vulkan shader compilers).

---

## ADR-003: Custom UEFI Bootloader (`knightboot`) & KBIP Protocol
- **Status:** Accepted
- **Context:** Boot both x86-64 and ARM64 cleanly on UEFI without tying KnightOS to GPLv3 GRUB2 or an external third-party bootloader binary.
- **Options Considered:**
  1. *GRUB2:* Heavy, legacy BIOS baggage, GPLv3.
  2. *Limine:* Clean protocol and UEFI multi-arch support, but introduces external bootloader ownership and protocol coupling.
  3. *Custom Rust UEFI Bootloader (`knightboot`) + `KBIP`:* Native `x86_64-unknown-uefi` & `aarch64-unknown-uefi` PE32+ binaries owned inside the repo.
- **Decision:** **Option 3.** Gives full ownership of Secure Boot signing, KASLR, ELF64 `W^X` mapping, and normalized ACPI/DTB handoff while keeping boot logic clean and minimal.

---

## ADR-004: Strict HAL Isolation for x86-64 & ARM64 Parity
- **Status:** Accepted
- **Context:** x86-64 and ARM64 differ in firmware (ACPI vs. ACPI/DTB), privilege levels (Ring 0/3 vs. EL2/EL1/EL0), paging (`CR3` vs. split `TTBR0/TTBR1`), memory ordering (TSO vs. weak ordering), interrupts (`x2APIC` vs. `GICv3`), and SMP wakeup (`SIPI` vs. `PSCI`).
- **Decision:** Isolate all CPU/MMU/IRQ/IOMMU code behind compile-time Rust traits in `kernel/hal/`, implemented strictly in `arch/x86_64/` and `arch/aarch64/`. No `#[cfg(target_arch)]` branches are permitted inside core scheduler, capability, or VMO logic.

---

## ADR-005: Native Filesystem (`KnightFS`) + User-Space VFS
- **Status:** Accepted
- **Context:** Achieve low storage consumption, crash resilience, atomic updates, and zero-copy page caching across microkernel boundaries.
- **Decision:** Implement a user-space VFS router backed by kernel-managed VMOs (unified page cache) and a custom Copy-on-Write extent/B+-tree filesystem (`KnightFS`) with inline LZ4/Zstd compression, metadata checksums, and snapshots—alongside FAT32 (ESP) and read/write `ext4`/`NTFS` compatibility drivers.

---

## ADR-006: User-Space Compatibility Personalities for Linux ELF & Windows PE
- **Status:** Accepted
- **Context:** Support Windows `.exe` games/apps and native Linux ELF binaries without copying Linux/Windows kernel code or bloating the KnightOS kernel ABI.
- **Decision:** Keep the kernel syscall ABI clean (~45–60 capability/VMO/IPC/futex syscalls) and implement Linux (`compat-linux`) and Windows (`compat-win`) personalities as sandboxed user-space loaders, syscall/API translators, and graphics/audio bridges targeting `gpu-hal`, `KnightCompositor`, and `audiod`.

---

## ADR-007: Explicit GPU HAL & Direct Scanout Compositor
- **Status:** Accepted
- **Context:** Deliver a fluid, macOS-inspired desktop UI without sacrificing PC gaming frame rates or input-to-photon latency.
- **Decision:** Design `gpu-hal` around explicit Vulkan-class command buffers, timeline fences, and per-context GPU MMU isolation, paired with `KnightCompositor` supporting damage-tracked vibrancy rendering for the desktop and **automatic direct scanout bypass** for fullscreen games.

---

## ADR-006: Capability-Delegated Process Spawning (`SYS_TASK_CREATE`)
- **Options Considered:** (1) POSIX `fork()`+`exec()` duplicating parent ambient authority, vs. (2) ELF-VMO capability spawn (`TASK_CREATE(elf_vmo, offset, len, init_chan, extra_cap)`).
- **Decision:** Use **Option 2**. Children start with an empty `CNode` receiving only explicitly transferred capabilities (`init_chan` + optional attenuated `extra_cap`), preserving KnightOS zero-ambient-authority isolation while letting `initd` launch `shell.elf` and driver hosts cleanly from `initrd.kpkg`.

---

## ADR-007: Dual-Backend `gpu-hal` (`LinearFbScanout` + `VirtioGpuScanout`)
- **Options Considered:** (1) Wait for vendor RDNA/Xe firmware before running `KnightCompositor`, vs. (2) Unified `ScanoutBackend` trait supporting both UEFI GOP linear framebuffers and PCIe `virtio-gpu-pci` 2D DMA scanout.
- **Decision:** Use **Option 2**. `KnightCompositor` renders damage-tracked glass/vibrancy scenes into a RAM backbuffer and presents cleanly over either UEFI GOP (`x86_64`/bare metal) or `virtio-gpu-pci` (`aarch64` QEMU / virtualized GPUs).

---

## ADR-008: KnightFS v2 Dual A/B Superblocks & Transparent Compression
- **Decision:** Upgrade `KnightFS` on-disk layout to alternating A/B superblocks (blocks 0 and 1) with monotonic CRC32-protected generation numbers for atomic failover/rollback, paired with inline `#![no_std]` lossless compression before disk write when compressed payload is smaller.
