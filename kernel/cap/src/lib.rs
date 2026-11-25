#![cfg_attr(not(test), no_std)]
//! Capability Engine (`CNode`) with generational handles and strict rights attenuation.

extern crate alloc;

use alloc::vec::Vec;
use knight_abi::{HANDLE_INVALID, Handle, KnightStatus, Rights, status};

/// Per-task capability table mapping 32-bit generational [`Handle`]s to kernel objects.
///
/// Handle encoding: `((generation as u32) << 16) | (slot_index as u32)`.
/// Slot 0 is permanently reserved as `HANDLE_INVALID`.
pub struct CNode<T: Clone> {
    slots: Vec<Option<CapSlot<T>>>,
    generations: Vec<u16>,
}

struct CapSlot<T: Clone> {
    object: T,
    rights: Rights,
}

impl<T: Clone> CNode<T> {
    pub fn new() -> Self {
        // Slot 0 is permanently invalid.
        Self {
            slots: alloc::vec![None],
            generations: alloc::vec![0],
        }
    }

    fn encode_handle(idx: usize, generation: u16) -> Handle {
        ((generation as u32) << 16) | (idx as u32 & 0xFFFF)
    }

    fn decode_handle(handle: Handle) -> Option<(usize, u16)> {
        if handle == HANDLE_INVALID {
            return None;
        }
        let idx = (handle & 0xFFFF) as usize;
        let generation = (handle >> 16) as u16;
        if idx == 0 { None } else { Some((idx, generation)) }
    }

    /// Inserts `object` with `rights` and returns its new unforgeable [`Handle`].
    pub fn insert(&mut self, object: T, rights: Rights) -> Result<Handle, KnightStatus> {
        for idx in 1..self.slots.len() {
            if self.slots[idx].is_none() {
                let next_gen = self.generations[idx].wrapping_add(1).max(1);
                self.generations[idx] = next_gen;
                self.slots[idx] = Some(CapSlot { object, rights });
                return Ok(Self::encode_handle(idx, next_gen));
            }
        }
        if self.slots.len() >= 65535 {
            return Err(status::ERR_NO_MEMORY);
        }
        let idx = self.slots.len();
        self.slots.push(Some(CapSlot { object, rights }));
        self.generations.push(1);
        Ok(Self::encode_handle(idx, 1))
    }

    /// Resolves `handle` and verifies that it holds all `required` rights.
    pub fn get(&self, handle: Handle, required: Rights) -> Result<(T, Rights), KnightStatus> {
        let (idx, generation) = Self::decode_handle(handle).ok_or(status::ERR_BAD_HANDLE)?;
        if idx >= self.slots.len() || self.generations[idx] != generation {
            return Err(status::ERR_BAD_HANDLE);
        }
        let slot = self.slots[idx].as_ref().ok_or(status::ERR_BAD_HANDLE)?;
        if !slot.rights.contains(required) {
            return Err(status::ERR_ACCESS_DENIED);
        }
        Ok((slot.object.clone(), slot.rights))
    }

    /// Returns the rights mask of `handle`.
    pub fn rights(&self, handle: Handle) -> Result<Rights, KnightStatus> {
        let (_, r) = self.get(handle, Rights::NONE)?;
        Ok(r)
    }

    /// Duplicates `handle` with attenuated rights (`current_rights & requested_mask`).
    /// Never amplifies rights; requires `Rights::DUPLICATE`.
    pub fn duplicate(&mut self, handle: Handle, mask: Rights) -> Result<Handle, KnightStatus> {
        let (obj, existing_rights) = self.get(handle, Rights::DUPLICATE)?;
        let attenuated = existing_rights.attenuate(mask);
        self.insert(obj, attenuated)
    }

    /// Removes `handle` from this `CNode` and returns its `(object, rights)` for transfer.
    pub fn take(&mut self, handle: Handle, required: Rights) -> Result<(T, Rights), KnightStatus> {
        let (idx, generation) = Self::decode_handle(handle).ok_or(status::ERR_BAD_HANDLE)?;
        if idx >= self.slots.len() || self.generations[idx] != generation {
            return Err(status::ERR_BAD_HANDLE);
        }
        let slot = self.slots[idx].as_ref().ok_or(status::ERR_BAD_HANDLE)?;
        if !slot.rights.contains(required) {
            return Err(status::ERR_ACCESS_DENIED);
        }
        let taken = self.slots[idx].take().unwrap();
        Ok((taken.object, taken.rights))
    }

    /// Closes `handle` and invalidates its generation.
    pub fn close(&mut self, handle: Handle) -> KnightStatus {
        let Some((idx, generation)) = Self::decode_handle(handle) else {
            return status::ERR_BAD_HANDLE;
        };
        if idx >= self.slots.len() || self.generations[idx] != generation {
            return status::ERR_BAD_HANDLE;
        }
        if self.slots[idx].take().is_some() {
            status::OK
        } else {
            status::ERR_BAD_HANDLE
        }
    }
}

impl<T: Clone> Default for CNode<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cnode_attenuation_and_stale_generation() {
        let mut cnode = CNode::<u64>::new();
        let h1 = cnode.insert(42, Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE)).unwrap();
        let h2 = cnode.duplicate(h1, Rights::READ).unwrap();

        assert_eq!(cnode.get(h2, Rights::READ).unwrap().0, 42);
        assert_eq!(cnode.get(h2, Rights::WRITE).err(), Some(status::ERR_ACCESS_DENIED));

        // Closing h1 must invalidate stale h1 even after slot reuse.
        assert_eq!(cnode.close(h1), status::OK);
        assert_eq!(cnode.get(h1, Rights::NONE).err(), Some(status::ERR_BAD_HANDLE));

        let h3 = cnode.insert(99, Rights::READ).unwrap();
        assert_ne!(h1, h3);
        assert_eq!(cnode.get(h1, Rights::NONE).err(), Some(status::ERR_BAD_HANDLE));
        assert_eq!(cnode.get(h3, Rights::READ).unwrap().0, 99);
    }
}
