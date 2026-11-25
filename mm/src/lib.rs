#![cfg_attr(not(test), no_std)]
//! KnightOS Memory Management (`pmm`, `vmm`, `slab`, `vmo`, `vmar`).

extern crate alloc;

pub mod pmm;
pub mod slab;
pub mod vmar;
pub mod vmo;
pub mod vmm;
