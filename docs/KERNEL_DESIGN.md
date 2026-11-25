# KnightOS Kernel Design

This document specifies the internal architecture of the KnightOS kernel, evaluates the choice between a **modular monolithic kernel** and a **microkernel**, and defines the core kernel primitives.

---

## 1. Kernel Architecture Evaluation: Modular Monolithic vs. Microkernel

Before committing to a kernel structure, we evaluate the two primary architectures against KnightOS's requirements: high-performance PC gaming, Windows `.exe` and Linux ELF compatibility, low storage/RAM usage, x86-64 and ARM64 portability, and strong security/stability.

### Option A: Modular Monolithic Kernel
- **Description:** Scheduler, memory manager, VFS, filesystems, networking stack, GPU memory/command management, and core device drivers execute in privileged kernel space (Ring 0 / EL1), organized as clean Rust traits and loadable modules.
- **Advantages:**
  - Zero-IPC latency for VFS, page cache, `futex`/`epoll`, and driver calls.
  - Simpler initial bring-up and lower baseline RAM overhead (single kernel heap and unified page cache).
  - Straightforward implementation of complex POSIX/NT virtual memory and synchronization semantics.
- **Disadvantages:**
  - **Large Trusted Computing Base (TCB):** Every driver (especially complex GPU, Wi-Fi, USB, and filesystem code) shares kernel address space. Even with Rust, third-party C hardware shims or `unsafe` MMIO/DMA bugs can cause kernel panics or privilege escalation.
  - **Poor Fault Containment:** A GPU driver hang or memory corruption crashes the entire desktop session.
  - **Harder Sandboxing:** Isolating Windows/Linux compatibility quirks or untrusted drivers requires extra ad-hoc mechanisms inside Ring 0.

### Option B: Pure Microkernel (Strict Minimalism)
- **Description:** Only address spaces, threads, and synchronous IPC live in the kernel; every policy—even paging decisions and every tiny bus driver in its own process—runs in user space.
- **Advantages:** Tiny TCB, mathematical/formal auditability, strong fault containment.
- **Disadvantages:** Frequent context switches on synchronous IPC degrade I/O, VFS, and frame-submission throughput; per-process stacks/IPC buffers across dozens of micro-services inflate RAM usage; fragmented page caching if memory ownership isn't unified.

### Option C (Selected): Capability-Based Microkernel with Async Shared-Memory Rings & Grouped Driver Domains
- **Description:** A small, memory-safe **capability microkernel** (~15–25K LOC target for core) that owns physical/virtual memory objects (`VMO`), address spaces (`Vmar`), capabilities (`CNode`), threads/scheduling, and IPC, while **drivers, filesystems, networking, compositor, and OS compatibility layers run in user space**. Crucially, it solves classic microkernel bottlenecks through three design mechanisms:
  1. **Asynchronous Shared-Memory Ring Buffers:** High-throughput paths (NVMe block I/O, network packets, VFS streaming, GPU command submission, input events) transfer batches over shared VMOs (`io_ring`-style submission/completion rings), amortizing syscalls across dozens or hundreds of operations.
  2. **Unified Kernel-Managed VMOs:** File caches and IPC buffers use the same kernel VMO pages mapped into VFS and client address spaces—eliminating double caching and keeping RAM usage low.
  3. **Grouped User-Space Driver Domains + IOMMU:** Drivers run outside Ring 0/EL1 with hardware IOMMU isolation, but related low-risk platform drivers share a domain to avoid wasteful per-driver process overhead, while complex/volatile drivers (GPU, NVMe, Wi-Fi) run in dedicated domains.

### Decision Summary
**Selected:** **Capability-Based Microkernel with Asynchronous Shared-Memory Fastpaths** (ADR-001 in [DECISIONS.md](DECISIONS.md)).
- **Why it wins for KnightOS:** Desktop users notice GPU/USB/audio driver crashes more than microsecond syscall differences. Isolating drivers in user space makes KnightOS dramatically more stable and secure, keeps the privileged kernel clean and genuinely original, and allows Windows/Linux compatibility personalities to evolve cleanly in user space—while shared-memory rings and direct GPU scanout preserve PC gaming performance.

---

## 2. Kernel Core Subsystems (`kernel/`)

```text
kernel/
├── core/   # Boot handoff, early console, panic, spinlocks, per-CPU locals
├── cap/    # CNode capability tables, rights bitmasks, handle resolution, revocation
├── ipc/    # Synchronous channels, async ports/WaitSets, shared-memory ring signaling
├── sched/  # Preemptive SMP scheduler, scheduling classes, priority inheritance
└── hal/    # Architecture traits implemented in arch/x86_64 and arch/aarch64
```

### 2.1 Capability Engine (`kernel/cap`)
Every kernel object is referenced via a 32-bit `Handle` indexing into the owning `Task`'s `CNode` (capability table):

- **Handle Entry:** `{ object_ref: Arc<KernelObject>, rights: Rights, generation: u32 }`.
- **Rights Bitmask:** `READ`, `WRITE`, `EXEC`, `MAP`, `DUPLICATE`, `TRANSFER`, `SIGNAL`, `MANAGE`, `DMA`, `IRQ`.
- **Attenuation:** `cap_duplicate(handle, subset_rights)` creates a weaker handle; rights can never be amplified.
- **Revocation:** Objects track capability derivation trees or epoch tokens so `devmgr` or a parent sandbox can cleanly revoke access when a driver restarts or a user revokes app permissions.

### 2.2 IPC & Synchronization (`kernel/ipc`)
KnightOS provides three composable primitives:

1. **Channel (Bidirectional Message + Capability Transfer):**
   - Transfers small inline byte payloads (up to 4 KiB) plus up to 16 transferred capability handles in a single call.
   - Supports `channel_call()` (send + wait reply) with direct thread handoff optimization for low-latency RPC.
2. **Port & WaitSet (`epoll`/`kqueue`-class event multiplexer):**
   - Aggregates readiness signals across channels, timers, IRQs, and shared-memory ring doorbells in one syscall (`port_wait()`).
   - Used heavily by user-space services, `KnightCompositor`, and the Linux/Windows compatibility layers.
3. **Futex / User-Space Fast Synchronization:**
   - Kernel `futex_wait(addr, expected, deadline)` and `futex_wake(addr, count)` operating on user-mapped VMO offsets—enabling uncontended user-space mutexes/semaphores in 0 syscalls and supporting Windows/Linux synchronization primitives directly.

### 2.3 Memory Objects & Address Spaces (`mm/`)
- **`Vmo` (Virtual Memory Object):** Represents a contiguous or paged array of physical frames. Supports:
  - `vmo_create(size, flags)` (anonymous, contiguous DMA, or resizable)
  - `vmo_clone(src, offset, size, COW)` (copy-on-write snapshot for fast process spawn & `fork`)
  - `vmo_pin(offset, len)` (for IOMMU-backed hardware DMA, requires `DMA` right)
- **`Vmar` (Virtual Memory Address Region):** Tree of virtual mappings inside a `Task`. Enforces `W^X` (cannot set `WRITE` and `EXEC` simultaneously unless an explicit, audited JIT capability is granted to a sandboxed runtime).

### 2.4 SMP Scheduler (`kernel/sched`)
- **Per-CPU Runqueues:** Eliminates global scheduler lock contention on high-core-count CPUs.
- **Hybrid Preemptive Design:**
  - **Strict Priority RT Band (0–31):** Fixed-priority with deadline/budget clamping (prevents runaway RT threads from starving the system) + Priority Inheritance Protocol (PIP) on synchronous IPC/futexes to prevent priority inversion.
  - **EEVDF-Inspired Virtual-Deadline Band:** Used for `Interactive`, `Normal`, and `Idle` tasks. Each task tracks virtual runtime (`vruntime`) and a latency weight/slice (`lag` + `virtual deadline`), naturally giving interactive desktop input and game render threads immediate preemption without complex heuristic hacks.
- **Affinity & Topology Awareness:** Respects hybrid cores (e.g., Intel P-cores/E-cores, AMD CCDs, ARM big.LITTLE) exposed via HAL topology tables so gaming/UI threads prefer performance cores while background tasks stay on efficiency cores.

---

## 3. Syscall ABI Design

KnightOS exposes a clean, minimal, versioned syscall ABI (~45–60 core syscalls, **not** POSIX syscalls):

| Category | Representative Syscalls |
| :--- | :--- |
| **Tasks & Threads** | `task_create`, `task_exit`, `thread_create`, `thread_exit`, `thread_yield`, `thread_set_affinity` |
| **Capabilities** | `handle_close`, `handle_duplicate`, `handle_replace`, `handle_info` |
| **Memory (`VMO`/`VMAR`)** | `vmo_create`, `vmo_clone`, `vmo_read`, `vmo_write`, `vmar_map`, `vmar_unmap`, `vmar_protect` |
| **IPC & Events** | `channel_create`, `channel_write`, `channel_read`, `channel_call`, `port_create`, `port_wait`, `timer_set` |
| **Sync** | `futex_wait`, `futex_wake`, `futex_requeue` |
| **Hardware (Privileged Cap)** | `irq_bind`, `irq_ack`, `mmio_map`, `iommu_bind`, `ioport_grant` (x86-64 only, capability-gated) |
| **Diagnostics & Time** | `clock_monotonic`, `debug_log`, `sys_info` |

### Calling Convention Isolation
- **x86-64:** `SYSCALL` / `SYSRET` instruction; syscall number in `RAX`; args in `RDI, RSI, RDX, R10, R8, R9`; return in `RAX` (negative errno-free `KnightStatus` codes).
- **ARM64:** `SVC #0` instruction; syscall number in `X8`; args in `X0–X5`; return in `X0`.

---

## 4. Panic, Faults, and Diagnostics

- **User-Space Exceptions:** Page faults, invalid instructions, and arithmetic faults are delivered to an attached exception channel (used for crash reporters, debuggers, and Windows SEH / Linux signal emulation).
- **Kernel Faults:** Any unhandled fault or invariant failure in kernel space triggers an immediate deterministic panic dump over serial UART + early framebuffer panic overlay (never silently corrupting state).
- **Kernel-to-User Upcalls (`VDSO` / `libknight`):** High-frequency read-only data (monotonic clock scaling factors for `RDTSC` / `CNTVCT_EL0`, thread-local metadata) is mapped via a read-only kernel `vDSO` VMO so `clock_monotonic()` often executes in user space with zero syscall overhead.
