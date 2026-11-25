#![cfg_attr(not(test), no_std)]
//! KnightOS Kernel-User ABI Contract (`knight-abi`).
//!
//! Defines syscall numbers, [`KnightStatus`], [`Handle`], capability [`Rights`],
//! [`VmarFlags`], and [`PortPacket`] shared identically between the kernel and
//! user-space (`libknight`, `initd`, drivers, services).

/// Opaque 32-bit capability handle index into a task's `CNode`.
pub type Handle = u32;

pub const HANDLE_INVALID: Handle = 0;

/// Standardized non-errno kernel status return type (0 = OK, negative = error).
pub type KnightStatus = i64;

pub mod status {
    use super::KnightStatus;

    pub const OK: KnightStatus = 0;
    pub const ERR_INVALID_ARG: KnightStatus = -1;
    pub const ERR_BAD_HANDLE: KnightStatus = -2;
    pub const ERR_ACCESS_DENIED: KnightStatus = -3;
    pub const ERR_NO_MEMORY: KnightStatus = -4;
    pub const ERR_NOT_FOUND: KnightStatus = -5;
    pub const ERR_SHOULD_WAIT: KnightStatus = -6;
    pub const ERR_PEER_CLOSED: KnightStatus = -7;
    pub const ERR_BUFFER_TOO_SMALL: KnightStatus = -8;
    pub const ERR_ALREADY_EXISTS: KnightStatus = -9;
    pub const ERR_TIMED_OUT: KnightStatus = -10;
    pub const ERR_WX_VIOLATION: KnightStatus = -11;
    pub const ERR_INTERNAL: KnightStatus = -12;
}

/// Capability rights bitmask (`u32`). Rights can only be attenuated, never amplified.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXEC: Self = Self(1 << 2);
    pub const MAP: Self = Self(1 << 3);
    pub const DUPLICATE: Self = Self(1 << 4);
    pub const TRANSFER: Self = Self(1 << 5);
    pub const SIGNAL: Self = Self(1 << 6);
    pub const WAIT: Self = Self(1 << 7);
    pub const MANAGE: Self = Self(1 << 8);
    pub const DMA: Self = Self(1 << 9);
    pub const IRQ: Self = Self(1 << 10);

    pub const BASIC: Self = Self(Self::DUPLICATE.0 | Self::TRANSFER.0 | Self::WAIT.0);
    pub const DEFAULT_VMO: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::MAP.0 | Self::BASIC.0);
    pub const DEFAULT_CHANNEL: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::BASIC.0 | Self::SIGNAL.0);
    pub const DEFAULT_PORT: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::BASIC.0);
    pub const ALL: Self = Self(0x07FF);

    pub const fn contains(self, required: Self) -> bool {
        (self.0 & required.0) == required.0
    }

    pub const fn attenuate(self, mask: Self) -> Self {
        Self(self.0 & mask.0)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Virtual Memory Address Region (`Vmar`) mapping permissions.
pub mod vmar_flags {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const EXEC: u32 = 1 << 2;
}

/// Syscall numbers (x86-64: `RAX`, ARM64: `X8`).
pub mod syscall {
    // Tasks & Threads
    pub const THREAD_EXIT: u64 = 1;
    pub const THREAD_YIELD: u64 = 2;
    pub const THREAD_CREATE: u64 = 3;
    pub const TASK_CREATE: u64 = 4;
    pub const TASK_EXIT: u64 = 5;

    // Capabilities
    pub const HANDLE_CLOSE: u64 = 10;
    pub const HANDLE_DUPLICATE: u64 = 11;
    pub const HANDLE_INFO: u64 = 12;

    // Memory (VMO / VMAR)
    pub const VMO_CREATE: u64 = 20;
    pub const VMO_READ: u64 = 21;
    pub const VMO_WRITE: u64 = 22;
    pub const VMO_CLONE: u64 = 23;
    pub const VMAR_MAP: u64 = 24;
    pub const VMAR_UNMAP: u64 = 25;
    pub const VMO_CREATE_DMA: u64 = 26;
    pub const MMIO_MAP: u64 = 27;
    pub const PCI_ECAM_INFO: u64 = 28;
    pub const FB_INFO: u64 = 29;

    // IPC & Events
    pub const CHANNEL_CREATE: u64 = 30;
    pub const CHANNEL_WRITE: u64 = 31;
    pub const CHANNEL_READ: u64 = 32;
    pub const PORT_CREATE: u64 = 33;
    pub const PORT_WAIT: u64 = 34;
    pub const PORT_QUEUE: u64 = 35;

    // Fast Synchronization
    pub const FUTEX_WAIT: u64 = 40;
    pub const FUTEX_WAKE: u64 = 41;

    // Time & Diagnostics
    pub const CLOCK_MONOTONIC: u64 = 50;
    pub const DEBUG_LOG: u64 = 51;
    pub const SYS_INFO: u64 = 52;
}

/// Packet delivered by `sys_port_wait` and queued by `sys_port_queue` or channel signals.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortPacket {
    pub key: u64,
    pub kind: u32,
    pub status: i32,
    pub data: [u64; 2],
}

pub mod packet_kind {
    pub const USER: u32 = 1;
    pub const SIGNAL: u32 = 2;
}

/// Basic system telemetry returned by `sys_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SysInfo {
    pub arch_id: u32,
    pub online_cpus: u32,
    pub total_usable_bytes: u64,
    pub free_bytes: u64,
}

/// Graphics linear framebuffer metadata returned by `fb_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FbInfo {
    pub phys_base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub pixel_format: u32,
}

/// Magic header for `initrd-<arch>.kpkg` bundles (`"KPKGv1\0\0"`).
pub const KPKG_MAGIC: [u8; 8] = *b"KPKGv1\0\0";

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KpkgHeader {
    pub magic: [u8; 8],
    pub entry_count: u32,
    pub _reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KpkgEntry {
    pub name: [u8; 64],
    pub offset: u64,
    pub size: u64,
}
