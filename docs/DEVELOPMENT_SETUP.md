# KnightOS Development Environment Setup

This guide explains how to prepare a clean, open-source cross-compilation and virtualization environment for building and debugging **KnightOS** on **x86-64** and **ARM64** hosts (Windows 11 or Linux).

---

## 1. Required Toolchain Components

| Tool | Purpose | License / Cost |
| :--- | :--- | :--- |
| **Rust Toolchain (`rustup`, `rustc`, `cargo`)** | Compiles `knightboot`, kernel core, HAL, drivers, and native user space | MIT / Apache-2.0 |
| **Rust Targets & Components** | `x86_64-unknown-uefi`, `aarch64-unknown-uefi`, `x86_64-unknown-none`, `aarch64-unknown-none`, `rust-src`, `llvm-tools-preview` | MIT / Apache-2.0 |
| **LLVM / Clang / LLD** | Cross-target C compilation, `aarch64` assembly (`clang` integrated assembler), ELF/PE linking (`lld`), `llvm-objcopy` | Apache-2.0 w/ LLVM Exception |
| **NASM** | Standalone x86-64 assembly (`ap_boot.asm`, real-mode trampolines) | BSD-2-Clause |
| **QEMU (`qemu-system-x86_64`, `qemu-system-aarch64`)** | Emulates x86-64 (`q35`) and ARM64 (`virt`) machines without needing bare-metal hardware during bring-up | GPL-2.0 |
| **UEFI Firmware (`OVMF` / `AAVMF`)** | Open-source EDK2 UEFI firmware images bundled with QEMU (`edk2-x86_64-code.fd`, `edk2-aarch64-code.fd`) | BSD-2-Clause-Patent |
| **GDB (`gdb` / `gdb-multiarch`) or LLDB** | Remote kernel debugging via QEMU's `-s -S` GDB stub | GPL-3.0 / Apache-2.0 |
| **Git** | Version control | GPL-2.0 |
| **Build Automation (`just` + `Python 3` / Cargo `xtask`)** | Orchestrates multi-arch builds, FAT32 ESP image creation, and QEMU test runs | CC0 / PSF |

---

## 2. Workstation Check

Run the repository verification script first to audit what is already installed:

- **Windows (PowerShell):**
  ```powershell
  .\tools\check-env.ps1
  ```
- **Linux / macOS (Bash):**
  ```bash
  ./tools/check-env.sh
  ```

---

## 3. Installation Instructions

### A. Windows 11 (x86-64 Host)

1. **Install Rust via `rustup`:**
   If not installed, download from <https://rustup.rs>. Then install the pinned toolchain and bare-metal/UEFI targets defined in `rust-toolchain.toml`:
   ```powershell
   rustup toolchain install nightly
   rustup component add rust-src llvm-tools-preview rustfmt clippy --toolchain nightly
   rustup target add x86_64-unknown-uefi aarch64-unknown-uefi x86_64-unknown-none aarch64-unknown-none --toolchain nightly
   ```
2. **Install LLVM/Clang, NASM, QEMU, GDB, and `just` via `winget`:**
   ```powershell
   winget install LLVM.LLVM
   winget install NASM.NASM
   winget install SoftwareFreedomConservancy.QEMU
   winget install Casey.Just
   ```
   > **Note on `gdb-multiarch` on Windows:** Standard MinGW GDB often only targets x86-64. For debugging **both** `x86_64` and `aarch64` QEMU kernels on Windows, install `gdb-multiarch` inside **MSYS2** (`pacman -S mingw-w64-ucrt-x86_64-gdb-multiarch`) or use **LLDB** bundled with `LLVM.LLVM` (`lldb`), which natively supports both `x86_64` and `aarch64` remote stubs.
3. **Ensure QEMU & LLVM binaries are on `PATH`:**
   Add `C:\Program Files\LLVM\bin`, `C:\Program Files\NASM`, and `C:\Program Files\qemu` to your user `PATH`, then re-run `.\tools\check-env.ps1`.

### B. Ubuntu / Debian Linux (x86-64 or ARM64 Host)

```bash
sudo apt update
sudo apt install -y \
    build-essential clang lld llvm nasm \
    qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64 \
    gdb-multiarch mtools dosfstools git python3

# Optional task runner
cargo install just

# Install Rust targets
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview rustfmt clippy --toolchain nightly
rustup target add x86_64-unknown-uefi aarch64-unknown-uefi x86_64-unknown-none aarch64-unknown-none --toolchain nightly
```

---

## 4. Planned QEMU & UEFI Boot Workflow (Phase 2+)

Once Phase 2 implements `knightboot` and the early kernel, the build runner (`tools/` + Cargo `xtask`) will create a FAT32 EFI System Partition (ESP) directory or disk image and launch QEMU with UEFI firmware:

### x86-64 QEMU Reference Command
```bash
qemu-system-x86_64 \
  -machine q35,accel=tcg \
  -cpu qemu64,+ssse3,+sse4.1,+sse4.2,+x2apic \
  -smp 4 -m 1024M \
  -drive if=pflash,format=raw,readonly=on,file=edk2-x86_64-code.fd \
  -drive format=raw,file=fat:rw:build/esp-x86_64 \
  -serial stdio \
  -no-reboot -no-shutdown
```
*(Enable `-accel kvm` on Linux or `-accel whpx` on Windows when testing without `-s -S` single-stepping.)*

### ARM64 (AArch64) QEMU Reference Command
```bash
qemu-system-aarch64 \
  -machine virt,gic-version=3 \
  -cpu cortex-a72 \
  -smp 4 -m 1024M \
  -drive if=pflash,format=raw,readonly=on,file=edk2-aarch64-code.fd \
  -drive format=raw,file=fat:rw:build/esp-aarch64 \
  -device virtio-gpu-pci \
  -serial stdio \
  -no-reboot -no-shutdown
```

---

## 5. GDB / LLDB Debugging Workflow

1. Start QEMU paused at reset with a GDB stub listening on TCP `:1234` (`-s -S`).
2. Attach from a second terminal:
   - **GDB:**
     ```bash
     gdb-multiarch target/x86_64-unknown-none/debug/knight-kernel
     (gdb) target remote :1234
     (gdb) hbreak kmain
     (gdb) continue
     ```
   - **LLDB (especially convenient on Windows):**
     ```powershell
     lldb target\aarch64-unknown-none\debug\knight-kernel
     (lldb) gdb-remote localhost:1234
     (lldb) breakpoint set --name kmain
     (lldb) continue
     ```
3. For UEFI bootloader (`knightboot`) debugging, log the image relocation base printed to serial COM1/PL011 and load PE symbols via `add-symbol-file BOOTX64.EFI <image_base + .text_offset>`.

---

## 6. Build Automation Strategy

Phase 2 will introduce a Cargo **`xtask`** crate under `tools/` (and optional `justfile` wrappers) so builds require zero fragile shell-specific Makefiles:
- `cargo xtask build --arch x86_64|aarch64` — builds `knightboot` (`*-unknown-uefi`) and `kernel` (`*-unknown-none`), packages `initrd`, and populates the ESP image.
- `cargo xtask run --arch x86_64|aarch64` — launches QEMU with OVMF/AAVMF and serial logging.
- `cargo xtask test` — runs host unit tests (`mm`, `cap`, `sched` pure logic) and automated headless QEMU integration boot tests (`tests/qemu`).
