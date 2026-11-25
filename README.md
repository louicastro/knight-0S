# KnightOS

[![Rust](https://img.shields.io/badge/Rust-nightly-orange.svg)](https://www.rust-lang.org/)
[![Architectures](https://img.shields.io/badge/architecture-x86--64%20%7C%20AArch64-blue.svg)](docs/HARDWARE_SUPPORT.md)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-green.svg)](Cargo.toml)

**KnightOS** is an independent desktop operating system built from scratch for x86-64 and ARM64 hardware. It is not a Linux distribution or a derivative of Windows or macOS. The project explores a capability-based microkernel, Rust-first systems programming, user-space drivers, and a native desktop environment.

## Project Status

> [!WARNING]
> KnightOS is in early development. Phase 1 is complete, and the Phase 2 kernel bring-up work is underway. It is not yet a bootable or usable desktop OS.

The repository currently provides the architecture, specifications, workspace layout, ABI contract, build automation, and portions of the boot and system scaffolding. Planned behavior is documented as design targets, not implemented features.

| Phase | Status | Focus |
| :--- | :--- | :--- |
| 1 - Foundation | Complete | Architecture, boot protocol, security model, toolchain, and repository structure |
| 2 - Boot and Bring-Up | Next | UEFI boot, HAL, memory management, SMP, interrupts, and early kernel |
| 3 - Microkernel | Planned | Capabilities, syscalls, IPC, scheduling, init services, and user-space drivers |
| 4 - Desktop | Planned | Compositor, UI toolkit, native applications, storage, and real hardware support |
| 5 - Compatibility | Planned | Linux and Windows application environments and PC gaming support |

See [docs/ROADMAP.md](docs/ROADMAP.md) for the implementation plan.

## Design Highlights

- **Capability-based security:** Kernel objects are accessed through rights-bearing handles rather than ambient global authority.
- **Rust-first implementation:** Memory-safe Rust provides the core implementation, with minimal architecture-specific assembly.
- **Strict architecture isolation:** x86-64 and AArch64 code is isolated behind HAL and platform boundaries.
- **UEFI boot support:** `knightboot` and the KnightBoot Info Protocol define the handoff from firmware to the kernel.
- **Shared-memory IPC:** Channels, ports, wait sets, VMOs, and event packets form the planned user-space communication model.
- **User-space drivers:** Device hosts aim to contain driver faults and enforce IOMMU-scoped device access.
- **Native desktop stack:** KnightCompositor, a GPU HAL, UI kit, themes, shell, terminal, and settings applications.

The public kernel-user contract is defined in [apps/sdk/abi/src/lib.rs](apps/sdk/abi/src/lib.rs).

## Repository Layout

```text
boot/        UEFI bootloader and KnightBoot Info Protocol
kernel/      Architecture-independent microkernel, HAL, capabilities, IPC, and scheduler
arch/        x86-64 and AArch64 CPU, MMU, interrupt, and platform code
mm/          Physical memory, virtual memory, VMOs, and allocators
drivers/     Bus, storage, VirtIO, GPU, input, network, and audio driver infrastructure
fs/          Virtual filesystem and native storage implementations
services/    initd, device management, networking, audio, power, and compatibility services
graphics/    Compositor, GPU HAL, UI kit, themes, and rendering infrastructure
apps/        Native SDK and desktop applications
tests/       Unit, integration, benchmark, and QEMU test areas
docs/        Architecture, design, setup, hardware, security, and roadmap documentation
tools/       Environment checks and Cargo xtask build automation
```

## Getting Started

### 1. Check prerequisites

Install Git and the pinned nightly Rust toolchain, then verify the required tools:

```powershell
rustup show
.\tools\check-env.ps1
```

On Linux or macOS:

```bash
rustup show
./tools/check-env.sh
```

### 2. Install Rust targets

The pinned toolchain includes the supported targets. If needed, install them manually:

```bash
rustup target add \
  x86_64-unknown-uefi \
  aarch64-unknown-uefi \
  x86_64-unknown-none \
  aarch64-unknown-none
```

### 3. Run host tests

```bash
cargo xtask test
```

### 4. Build and run

```bash
# Build all boot, kernel, and initrd artifacts
cargo xtask build

# Build one architecture
cargo xtask build --arch x86_64
cargo xtask build --arch aarch64

# Run in QEMU
cargo xtask run --arch x86_64
cargo xtask run --arch aarch64
```

Use `cargo xtask --help` for the complete command list. Full toolchain installation and debugging instructions are available in [docs/DEVELOPMENT_SETUP.md](docs/DEVELOPMENT_SETUP.md).

## Documentation

| Document | Purpose |
| :--- | :--- |
| [OS Architecture](docs/OS_ARCHITECTURE.md) | System-wide model and component relationships |
| [Kernel Design](docs/KERNEL_DESIGN.md) | Kernel architecture, IPC, scheduling, capabilities, and syscalls |
| [Boot Process](docs/BOOT_PROCESS.md) | UEFI handoff and KnightBoot protocol |
| [Hardware Support](docs/HARDWARE_SUPPORT.md) | Architectures, HAL boundaries, and hardware phases |
| [Security Design](docs/SECURITY_DESIGN.md) | Threat model, memory protections, isolation, and updates |
| [Development Setup](docs/DEVELOPMENT_SETUP.md) | Toolchain installation and QEMU debugging |
| [Engineering Roadmap](docs/ROADMAP.md) | Current phase and future implementation milestones |
| [Architecture Decisions](docs/DECISIONS.md) | Recorded design alternatives and rationale |

## Engineering Principles

- Do not copy Linux, Windows, or macOS kernel implementations.
- Prefer open standards such as UEFI, ACPI, Device Tree, ELF64, PCIe, NVMe, and Vulkan-class GPU interfaces.
- Keep architecture-specific code behind explicit HAL traits.
- Prioritize isolation, stability, and memory safety over synthetic benchmarks.
- Validate claimed behavior through unit tests or reproducible QEMU and hardware runs.

## Contributing

Contributions are welcome as the project moves from Phase 1 scaffolding into kernel implementation. When contributing:

1. Read the relevant design document before changing a subsystem.
2. Keep architecture-specific code behind the established abstractions.
3. Add or update tests for behavior you change.
4. Run `cargo xtask test` and the smallest relevant xtask build before submitting a change.

Open an issue to propose a substantial design change or to discuss implementation work.

## License

KnightOS is dual-licensed under **Apache-2.0 OR MIT**. This license declaration is defined by [Cargo.toml](Cargo.toml).
