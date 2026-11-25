//! Zero-dependency `KPKGv1` initial ramdisk archive parser.

use core::mem::size_of;
use knight_abi::{KPKG_MAGIC, KpkgEntry, KpkgHeader};

pub fn find_entry<'a>(initrd: &'a [u8], target_name: &str) -> Option<&'a [u8]> {
    if initrd.len() < size_of::<KpkgHeader>() {
        return None;
    }
    let hdr = unsafe { core::ptr::read_unaligned(initrd.as_ptr() as *const KpkgHeader) };
    if hdr.magic != KPKG_MAGIC {
        return None;
    }
    let table_start = size_of::<KpkgHeader>();
    let entry_size = size_of::<KpkgEntry>();
    let count = hdr.entry_count as usize;
    if table_start.checked_add(count.checked_mul(entry_size)?)? > initrd.len() {
        return None;
    }

    for i in 0..count {
        let off = table_start + i * entry_size;
        let ent = unsafe { core::ptr::read_unaligned(initrd[off..].as_ptr() as *const KpkgEntry) };
        let name_len = ent.name.iter().position(|&b| b == 0).unwrap_or(ent.name.len());
        if let Ok(name) = core::str::from_utf8(&ent.name[..name_len])
            && name == target_name
        {
            let start = ent.offset as usize;
            let end = start.checked_add(ent.size as usize)?;
            if end <= initrd.len() {
                return Some(&initrd[start..end]);
            }
        }
    }
    None
}
