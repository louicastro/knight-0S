# KnightOS Boot Process & UEFI Specification

This document evaluates bootloader options, selects KnightOS's boot strategy, and defines the complete 7-stage UEFI boot sequence for **x86-64** and **ARM64 (AArch64)**.

---

## 1. Bootloader Evaluation & Selection

Because KnightOS is an independent operating system targeting UEFI on both x86-64 and ARM64, we evaluated three bootloader approaches:

| Option | Strengths | Weaknesses | Verdict |
| :--- | :--- | :--- | :--- |
| **GRUB2** | Ubiquitous, supports Multiboot2 & chainloading | Large legacy C codebase, GPLv3 licensing, fragile UEFI/ARM64 cross-tooling, carries legacy BIOS assumptions | **Rejected** |
| **Limine** | Clean modern boot protocol, supports x86-64 & ARM64 UEFI, higher-half direct mapping | External third-party binary dependency; couples KnightOS kernel entry to Limine's protocol revisions | **Rejected** (useful reference only) |
| **Custom UEFI Bootloader (`knightboot`) in Rust** | 100% repo-owned, `no_std` Rust memory safety, builds natively to `x86_64-unknown-uefi` & `aarch64-unknown-uefi`, defines our own minimal **KBIP** protocol, seamless future Secure Boot / TPM2 measured boot | Requires writing ~1,500–2,500 LOC of UEFI loader + ELF64 mapper in Phase 2 | **Selected** |

**Decision:** Build **`knightboot`**, a custom Rust UEFI bootloader (`boot/knightboot`) passing a versioned **KnightBoot Info Protocol (`KBIP`)** structure (`boot/protocol`). See ADR-003 in [DECISIONS.md](DECISIONS.md).

---

## 2. EFI System Partition (ESP) Layout

```text
/EFI/
├── BOOT/
│   ├── BOOTX64.EFI          # knightboot for x86-64
│   └── BOOTAA64.EFI         # knightboot for ARM64
└── KNIGHTOS/
    ├── knightboot.toml      # Boot configuration (slot A/B, cmdline, timeout)
    ├── kernel-x86_64.elf    # Signed higher-half x86-64 microkernel
    ├── kernel-aarch64.elf   # Signed higher-half ARM64 microkernel
    ├── initrd-x86_64.kpkg   # Compressed initial service & driver bundle
    └── initrd-aarch64.kpkg
```

---

## 3. Seven-Stage UEFI Boot Sequence

### Stage 1: Firmware Initialization
- Platform firmware executes hardware initialization (POST, DRAM training, PCIe root complex enumeration, UEFI DXE drivers).
- **Secure Boot (when enabled):** Firmware verifies the cryptographic signature of `BOOTX64.EFI` / `BOOTAA64.EFI` against enrolled keys before transferring control.
- Firmware enters `knightboot` in **64-bit long mode** (x86-64, paging enabled with identity-mapped RAM) or **AArch64 EL2 / EL1** (ARM64, MMU enabled with identity mapping).

### Stage 2: Bootloader Execution (`knightboot`)
1. Initialize UEFI `SystemTable` console and serial diagnostics (COM1 on x86-64 or PL011/SBSA UART via SPCR/DTB on ARM64).
2. Read `knightboot.toml` via `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL` and determine active A/B boot slot.
3. Query `EFI_GRAPHICS_OUTPUT_PROTOCOL` (`GOP`) to select the highest native display resolution (with fallback to 1280×720 / 1024×768) and capture framebuffer physical base, stride, width, height, and pixel format (`RGBX`/`BGRX`).
4. Locate hardware description tables from the UEFI Configuration Table:
   - **x86-64:** ACPI 2.0+ `RSDP` (`EFI_ACPI_20_TABLE_GUID`).
   - **ARM64:** ACPI 2.0+ `RSDP` **or** Flattened Device Tree (`EFI_DTB_TABLE_GUID`).
5. Gather entropy via `EFI_RNG_PROTOCOL` (supplemented later by `RDRAND`/`RDSEED` or ARM `RNDR`) for KASLR and stack canary seeds.
6. Verify cryptographic signatures of the kernel ELF and `initrd.kpkg` bundle before loading.

### Stage 3: Kernel & Boot-Bundle Loading
1. Read `kernel-<arch>.elf` and `initrd-<arch>.kpkg` into UEFI-allocated physical pages (`EfiLoaderData`).
2. Validate ELF64 headers (`EI_CLASS = 64`, matching `EM_X86_64` or `EM_AARCH64`, no invalid overlapping `PT_LOAD` segments).
3. Allocate physical frames (`AllocatePages`) for each `PT_LOAD` segment, zero-fill `.bss`, and record segment permissions (`PF_R`, `PF_W`, `PF_X`) to enforce strict `W^X` in the kernel page tables.
4. Allocate initial bootstrap page tables and map:
   - Higher-half kernel virtual base (e.g., `0xFFFF_FFFF_8000_0000` + KASLR slide).
   - Direct physical memory map window (HHDM, e.g., `0xFFFF_8000_0000_0000`) for fast physical-to-virtual translation inside the kernel.
   - Trampoline identity mapping for the tiny transition stub that swaps page tables.
   - `KBIP` struct, normalized memory map, `initrd` buffer, and initial 64 KiB kernel boot stack.

### Stage 4: Memory Map Acquisition & `ExitBootServices`
UEFI boot services own hardware timers and memory until explicitly relinquished:
1. Query `BootServices->GetMemoryMap()` with buffer size probing.
2. Allocate buffer with extra slack (+8 descriptors, because allocating the buffer itself may split a descriptor).
3. Call `GetMemoryMap()` again to obtain the exact `map_key`, then immediately call `BootServices->ExitBootServices(image_handle, map_key)`.
4. **Retry Loop Rule:** If `ExitBootServices` returns `EFI_INVALID_PARAMETER`, re-fetch `GetMemoryMap()` (with zero new heap allocations) and retry `ExitBootServices` until success.
5. **Post-Exit Invariant:** Once `ExitBootServices` succeeds, UEFI Boot Services are dead—no UEFI console, file, or allocation calls are permitted. `knightboot` converts `EFI_MEMORY_DESCRIPTOR` entries into a coalesced, sorted array of `KbipMemoryRegion` (`Usable`, `AcpiReclaimable`, `Reserved`, `KernelAndModules`, `Framebuffer`).

### Stage 5: Architecture-Specific CPU Handoff
Because x86-64 and ARM64 differ fundamentally in privilege levels and paging hardware, `knightboot` runs an architecture-specific handoff stub (`arch/x86_64` or `arch/aarch64`):

#### x86-64 CPU Handoff
1. `CLI` (disable maskable interrupts) and mask legacy PIC (`0x21`, `0xA1`).
2. Load minimal bootstrap `GDT` (64-bit kernel code/data segments) and null `IDT`.
3. Configure `EFER` (`NXE` for no-execute, `SCE` for `SYSCALL`), `CR0` (`WP`, `PG`, `MP`), and `CR4` (`PAE`, `PGE`, `OSFXSR`, `OSXMMEXCPT`; enable `SMEP`/`SMAP` once kernel tables are active).
4. Load `CR3` with the newly built kernel PML4 (or 5-level PML5) physical address.
5. Switch `RSP` to the higher-half kernel stack and jump (`JMP`) to the higher-half ELF entry point `_start(kbip_phys_or_virt)`.

#### ARM64 (AArch64) CPU Handoff
1. Mask DAIF exceptions (`MSR DAIFSet, #0xf`).
2. Inspect `CurrentEL`:
   - If booting at **EL2** (hypervisor mode, common on UEFI ARM64 boards), configure `HCR_EL2` (AArch64 at EL1), `CNTHCTL_EL2` (grant EL1 physical timer/counter access), and execute `ERET` to drop cleanly into **EL1**. (Optional future hypervisor hook can record EL2 availability before dropping.)
3. Configure `MAIR_EL1` (Normal Write-Back cacheable vs. Device-nGnRnE MMIO attributes), `TCR_EL1` (48-bit VA, 4 KiB granule, inner shareable), and install kernel page table root in `TTBR1_EL1` (plus trampoline in `TTBR0_EL1`).
4. Invalidate TLBs (`TLBI VMALLE1`), issue `DSB SY; ISB`, enable MMU and caches in `SCTLR_EL1` (including `WXN` where applicable).
5. Set `SP` to the higher-half kernel stack and branch (`BR`) to `_start(kbip_virt)`.

### Stage 6: Kernel Entry Point (`_start` -> `kmain`)
1. Clear unmapped lower-half trampoline entries from `PML4` / `TTBR0_EL1`.
2. Verify `KBIP` magic (`0x4B4E4947_48544231` — `"KNIGHTB1"`), version, and CRC32/checksum.
3. Initialize early architecture-agnostic logging (early UART + basic GOP framebuffer text renderer).
4. Initialize **Physical Memory Manager (`mm/pmm`)** from `KbipMemoryRegion` entries, reserving kernel, `initrd`, framebuffer, and ACPI/DTB regions.
5. Initialize **Virtual Memory Manager (`mm/vmm`)** and **Slab Allocator (`mm/slab`)**, enabling kernel heap and VMO creation.
6. Install full architecture trap/interrupt handlers (`IDT` + `TSS`/`IST` on x86-64; `VBAR_EL1` vector table on ARM64) and per-CPU local storage (`GS_BASE` vs. `TPIDR_EL1`).

### Stage 7: Initial System & SMP Initialization
1. Parse ACPI (`MADT`, `MCFG`, `FADT`, `DMAR`/`IORT`) or ARM64 `DTB` to discover CPU cores, interrupt controllers, PCIe ECAM bases, timers, and IOMMUs.
2. Initialize interrupt controller (**x2APIC/IOAPIC** on x86-64; **GICv2/v3 + ITS** on ARM64) and calibrate high-resolution monotonic timer (`TSC` + APIC timer vs. ARM Generic Timer).
3. Wake Application Processors (APs) / secondary cores:
   - **x86-64:** Send `INIT`–`SIPI`–`SIPI` IPIs to APIC IDs pointing to a low-RAM real-mode-to-long-mode trampoline (`arch/x86_64/ap_boot.asm`).
   - **ARM64:** Invoke **PSCI `CPU_ON`** (SMC/HVC) or spin-table wakeup per DTB/ACPI MADT (`arch/aarch64/ap_boot.S`).
4. Initialize IOMMU domains (`VT-d`/`AMD-Vi` or `SMMUv3`) if present.
5. Unpack `initrd.kpkg` into an initial read-only memory VMO, create the root capability table, and spawn **`initd`** (`services/initd`) as the first user-space process.
6. Enter the SMP scheduler loop—kernel initialization is complete and control transitions to user-space bring-up.

---

## 4. KnightBoot Info Protocol (`KBIP`) Contract

```text
KbipBootInfo (C-ABI / #[repr(C)], 8-byte aligned)
├── magic: u64                  // "KNIGHTB1"
├── version: u32                // Protocol revision (v1)
├── total_size: u32
├── arch: u32                   // 1 = x86_64, 2 = aarch64
├── flags: u32                  // Bitflags (secure_boot, kaslr_enabled, el2_present)
├── hhdm_offset: u64            // Higher-half direct map virtual base
├── kernel_phys_base: u64
├── kernel_virt_base: u64
├── memory_map_ptr: u64         // Pointer to KbipMemoryRegion[]
├── memory_map_count: u64
├── framebuffer: KbipFramebuffer
├── acpi_rsdp_phys: u64         // 0 if absent
├── dtb_phys: u64               // 0 if absent (ARM64 SoCs)
├── initrd_phys: u64
├── initrd_size: u64
└── kaslr_seed: [u8; 32]
```
