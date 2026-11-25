# KnightOS

KnightOS is an independent operating-system project built from scratch in Rust, with minimal assembly. Its architecture targets **x86-64** and **ARM64 (AArch64)**, with a capability-based microkernel design and a custom UEFI boot path.

> **Status:** Early development. The architecture, documentation, workspace, and development tooling are in place; the system is not production-ready. Treat desktop, driver, and Linux/Windows application compatibility features as design goals unless the roadmap marks them complete.

## Project goals

- Build a small, memory-safe kernel with capability-based isolation.
- Support x86-64 and ARM64 through architecture-specific hardware abstraction layers.
- Develop native user-space services, a desktop compositor, and applications.
- Explore Linux ELF and Windows PE application compatibility as longer-term goals.

See the [roadmap](docs/ROADMAP.md) for milestones and progress, and the [architecture overview](docs/OS_ARCHITECTURE.md) for the system design.

## Get started

### Requirements

Install the Rust toolchain pinned by `rust-toolchain.toml`, the targets and components it specifies, plus the platform tools needed for cross-compilation and QEMU. The [development setup guide](docs/DEVELOPMENT_SETUP.md) has installation instructions for Windows and Linux.

Check your environment from the repository root:

```bash
./tools/check-env.sh
```

On Windows, run the PowerShell environment check script at `tools/check-env.ps1` from the repository root.

### Build and test

```bash
# Run host-side tests
cargo xtask test

# Build the bootloader, kernel, and initrd for x86-64
cargo xtask build --arch x86_64

# Run the QEMU smoke test (requires QEMU and UEFI firmware)
cargo xtask qemu-test --arch x86_64
```

Use `--arch aarch64` for ARM64 builds and QEMU testing. See `cargo xtask help` for all available commands and options. The QEMU workflow and firmware requirements are documented in the [development setup guide](docs/DEVELOPMENT_SETUP.md).

## Repository map

| Path | Contents |
| --- | --- |
| `boot/` | UEFI bootloader and boot protocol |
| `kernel/`, `arch/`, `mm/` | Kernel, CPU-specific code, and memory management |
| `drivers/`, `fs/`, `services/` | Drivers, filesystems, and system services |
| `graphics/`, `apps/` | Graphics stack, UI, and applications |
| `tests/`, `tools/` | Tests, environment checks, build automation, and QEMU runners |
| `docs/` | Architecture, design, setup, security, and roadmap documentation |

## Documentation

- [System architecture](docs/OS_ARCHITECTURE.md)
- [Kernel design](docs/KERNEL_DESIGN.md)
- [Boot process](docs/BOOT_PROCESS.md)
- [Hardware support](docs/HARDWARE_SUPPORT.md)
- [Security design](docs/SECURITY_DESIGN.md)
- [Development setup](docs/DEVELOPMENT_SETUP.md)
- [Engineering roadmap](docs/ROADMAP.md)
- [Architecture decisions](docs/DECISIONS.md)

## Engineering principles

1. Prefer open standards and clear architecture boundaries.
2. Keep architecture-specific code behind hardware abstraction interfaces.
3. Prioritize safety and stability over synthetic benchmark results.
4. Verify capabilities with automated tests or reproducible QEMU/hardware runs.
