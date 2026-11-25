# KnightOS Security Design

Security in KnightOS is architectural rather than bolted on. Every design decision prioritizes small Trusted Computing Base (TCB) boundaries, memory safety, hardware isolation, and explicit least-privilege access.

---

## 1. Threat Model

KnightOS defends against five primary threat classes on a general-purpose gaming and desktop workstation:

1. **Untrusted Applications & Third-Party Binaries:** Malicious or compromised native, Linux, or Windows apps attempting to read user files, spy on input/screen, escalate privileges, or persist silently.
2. **Buggy or Exploited Device Drivers:** Complex GPU, Wi-Fi, USB, or storage drivers encountering malformed hardware inputs or malicious DMA peripherals (Thunderbolt/PCIe hotplug attacks).
3. **Kernel Memory Corruption:** Use-after-free, buffer overflows, integer overflows, or race conditions across the syscall boundary.
4. **Physical/Evil-Maid & Supply-Chain Tampering:** Offline modification of bootloader, kernel, or OS binaries on disk.
5. **Cross-App Side Channels & Input Snooping:** Background apps reading keystrokes, clipboard, or GPU memory of foreground apps/games.

---

## 2. Capability-Based Access Control

KnightOS rejects ambient authority (such as POSIX `UID 0` root or unrestricted global filesystem paths):

- **Unforgeable Handles:** A process can only interact with objects (files, directories, network sockets, display surfaces, audio streams, MMIO BARs) for which it holds an explicit capability handle in its kernel-managed `CNode`.
- **Least Privilege & Attenuation:** Handles carry granular rights (`READ`, `WRITE`, `EXEC`, `MAP`, `DUPLICATE`, `TRANSFER`, `MANAGE`, `DMA`, `IRQ`). Rights can only be narrowed, never escalated.
- **User Consent Portals:** Unsandboxed file access does not exist for normal apps. Accessing documents outside an app's private container occurs through a trusted system file-picker portal that grants a scoped capability strictly to the user-selected file or directory.
- **Compositor Input Isolation:** Only the focused window receives keyboard events; background apps cannot install global keyloggers without an explicit, system-badged accessibility/hotkey capability.

---

## 3. Memory Safety & CPU Hardware Protections

- **Memory-Safe Implementation:** Core kernel subsystems (`kernel/`, `mm/`), `knightboot`, system services, and native drivers are written in **Rust**, isolating raw pointer/register `unsafe` blocks behind small, auditable HAL boundaries.
- **Strict `W^X` (Write XOR Execute):** The VMM forbids mapping any page simultaneously writable and executable (`RWX`). Just-In-Time (JIT) compilers (browsers, game emulators, compatibility translators) require a dedicated `JIT` policy creating dual-mapped or explicitly transitioned `RW` -> `RX` code pages with auditability.
- **CPU Mitigation Features Enabled by Default:**
  - **x86-64:** `NX` bit, `SMEP` (no kernel execution of user pages), `SMAP` (no unintended kernel access to user pages), `UMIP`, `PCID`/KPTI-style separation where required by CPU errata, `FSGSBASE` isolation, stack probes/guard pages.
  - **ARM64:** `PXN` (Privileged Execute Never), `UXN`, `PAN` (Privileged Access Never), `WXN`, `BTI` & Pointer Authentication (`PAC`) on ARMv8.3+ silicon.
- **Randomization & Hardening:** Kernel & user ASLR (KASLR seeded from UEFI RNG + hardware RNG), unmapped guard pages around every thread stack, zero-initialization of newly allocated user VMO pages (preventing cross-process RAM leakage).

---

## 4. Hardware DMA & Driver Isolation

Because drivers run in **unprivileged user-space Driver Host Domains**:

- **Mandatory IOMMU Enforcement:** `devmgr` configures **Intel VT-d / AMD-Vi** or **ARM SMMUv3** before enabling PCIe Bus Mastering on any device. Devices can only DMA into explicitly pinned VMO pages granted to that driver domain—preventing malicious USB4/Thunderbolt peripherals from reading kernel memory.
- **Interrupt & MMIO Scoping:** A driver domain receives only the specific MMIO BAR VMO and IRQ wait handle for its hardware ID, never unrestricted `/dev/mem` or global I/O port access.
- **Crash Containment:** Faults inside a driver domain trap to `devmgr`, which revokes capabilities, resets the hardware/IOMMU domain, and cleanly restarts the driver without crashing the OS.

---

## 5. Application & Compatibility Sandboxing

- **Native Apps:** Run in individual capability sandboxes declared via signed app manifests.
- **Linux & Windows Compatibility Environments:** Each compatibility environment (`compat-linux`, `compat-win`) runs per-application or per-wine-prefix in an unprivileged sandbox. A Windows `.exe` sees only its isolated virtual prefix (`C:\`) plus explicitly shared user directories—preventing legacy software from touching OS system files or unrelated user data.

---

## 6. Verified Boot & Atomic System Integrity

1. **UEFI Secure Boot Chain:** Firmware verifies `knightboot` (`BOOTX64.EFI` / `BOOTAA64.EFI`); `knightboot` verifies cryptographic signatures on `kernel-<arch>.elf` and `initrd-<arch>.kpkg` before handoff.
2. **Immutable Base System & A/B Updates:** Core OS binaries reside in a read-only, compressed, integrity-verified system image partitioned into **Slot A** and **Slot B**. System updates write to the inactive slot in the background, verify hashes, and flip the boot slot atomically—with automatic rollback if the new slot fails health checks on boot.
3. **Full-Disk Encryption (Planned Phase 4):** Per-volume XTS-AES-256 / ChaCha20-Poly1305 encryption in `KnightFS` tied to TPM 2.0 sealing + user passphrase.
