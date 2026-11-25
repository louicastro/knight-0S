#![cfg_attr(not(test), no_std)]
//! KnightFS v2 — CoW/A-B superblock generation failover + transparent lossless compression
//! + CRC32 end-to-end payload integrity verification.

pub const BLOCK_SIZE: usize = 4096;
pub const MAGIC: [u8; 8] = *b"KNIGHTFS";
pub const VERSION: u32 = 2;
pub const MAX_NAME_LEN: usize = 38;
pub const INODE_SIZE: usize = 64;
pub const INODE_TABLE_BLOCKS: u64 = 2;
pub const MAX_INODES: usize = (BLOCK_SIZE * INODE_TABLE_BLOCKS as usize) / INODE_SIZE;

const SUPER_BLOCK_A: u64 = 0;
const SUPER_BLOCK_B: u64 = 1;
const BITMAP_BLOCK: u64 = 2;
const INODE_START_BLOCK: u64 = 3;
const DATA_START_BLOCK: u64 = INODE_START_BLOCK + INODE_TABLE_BLOCKS;

const INODE_FLAG_COMPRESSED: u16 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    IoError,
    InvalidSuperblock,
    ChecksumMismatch,
    InvalidName,
    NotFound,
    NoSpace,
    BufferTooSmall,
    InvalidGeometry,
    CorruptCompressedData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub size_bytes: u32,
    pub stored_bytes: u32,
    pub start_block: u32,
    pub block_count: u32,
    pub data_crc32: u32,
    pub compressed: bool,
}

pub trait BlockDevice {
    fn sector_size(&self) -> usize;
    fn sector_count(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError>;
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError>;
}

#[derive(Clone, Copy)]
struct Superblock {
    generation: u64,
    active_slot: u8,
    total_blocks: u64,
    bitmap_start: u64,
    bitmap_blocks: u64,
    inode_start: u64,
    inode_blocks: u64,
    data_start: u64,
}

impl Superblock {
    fn encode(&self, out: &mut [u8; BLOCK_SIZE]) {
        out.fill(0);
        out[0..8].copy_from_slice(&MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.total_blocks.to_le_bytes());
        out[24..32].copy_from_slice(&self.bitmap_start.to_le_bytes());
        out[32..40].copy_from_slice(&self.bitmap_blocks.to_le_bytes());
        out[40..48].copy_from_slice(&self.inode_start.to_le_bytes());
        out[48..56].copy_from_slice(&self.inode_blocks.to_le_bytes());
        out[56..64].copy_from_slice(&self.data_start.to_le_bytes());
        out[64..72].copy_from_slice(&self.generation.to_le_bytes());
        out[72] = self.active_slot;
        let sum = crc32(&out[0..76]);
        out[76..80].copy_from_slice(&sum.to_le_bytes());
    }

    fn decode(buf: &[u8; BLOCK_SIZE]) -> Result<Self, FsError> {
        if buf[0..8] != MAGIC {
            return Err(FsError::InvalidSuperblock);
        }
        let ver = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let bsz = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
        if ver != VERSION || bsz != BLOCK_SIZE {
            return Err(FsError::InvalidSuperblock);
        }
        let stored_crc = u32::from_le_bytes(buf[76..80].try_into().unwrap());
        if crc32(&buf[0..76]) != stored_crc {
            return Err(FsError::ChecksumMismatch);
        }
        let sb = Self {
            total_blocks: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            bitmap_start: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            bitmap_blocks: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
            inode_start: u64::from_le_bytes(buf[40..48].try_into().unwrap()),
            inode_blocks: u64::from_le_bytes(buf[48..56].try_into().unwrap()),
            data_start: u64::from_le_bytes(buf[56..64].try_into().unwrap()),
            generation: u64::from_le_bytes(buf[64..72].try_into().unwrap()),
            active_slot: buf[72] & 1,
        };
        if sb.data_start < DATA_START_BLOCK || sb.total_blocks <= sb.data_start {
            return Err(FsError::InvalidSuperblock);
        }
        Ok(sb)
    }
}

#[derive(Clone, Copy)]
struct Inode {
    used: bool,
    name_len: u8,
    flags: u16,
    size_bytes: u32,
    stored_bytes: u32,
    start_block: u32,
    block_count: u32,
    data_crc32: u32,
    name: [u8; MAX_NAME_LEN],
}

impl Inode {
    const fn empty() -> Self {
        Self {
            used: false,
            name_len: 0,
            flags: 0,
            size_bytes: 0,
            stored_bytes: 0,
            start_block: 0,
            block_count: 0,
            data_crc32: 0,
            name: [0; MAX_NAME_LEN],
        }
    }

    fn name_bytes(&self) -> &[u8] {
        let len = (self.name_len as usize).min(MAX_NAME_LEN);
        &self.name[..len]
    }

    fn decode(slot: &[u8]) -> Self {
        let used = slot[0] != 0;
        let name_len = slot[1];
        let flags = u16::from_le_bytes(slot[2..4].try_into().unwrap());
        let size_bytes = u32::from_le_bytes(slot[4..8].try_into().unwrap());
        let start_block = u32::from_le_bytes(slot[8..12].try_into().unwrap());
        let block_count = u32::from_le_bytes(slot[12..16].try_into().unwrap());
        let data_crc32 = u32::from_le_bytes(slot[16..20].try_into().unwrap());
        let stored_bytes = u32::from_le_bytes(slot[20..24].try_into().unwrap());
        let mut name = [0u8; MAX_NAME_LEN];
        name.copy_from_slice(&slot[24..24 + MAX_NAME_LEN]);
        Self {
            used,
            name_len,
            flags,
            size_bytes,
            stored_bytes: if stored_bytes == 0 && (flags & INODE_FLAG_COMPRESSED) == 0 {
                size_bytes
            } else {
                stored_bytes
            },
            start_block,
            block_count,
            data_crc32,
            name,
        }
    }

    fn encode(&self, slot: &mut [u8]) {
        slot[..INODE_SIZE].fill(0);
        slot[0] = u8::from(self.used);
        slot[1] = self.name_len;
        slot[2..4].copy_from_slice(&self.flags.to_le_bytes());
        slot[4..8].copy_from_slice(&self.size_bytes.to_le_bytes());
        slot[8..12].copy_from_slice(&self.start_block.to_le_bytes());
        slot[12..16].copy_from_slice(&self.block_count.to_le_bytes());
        slot[16..20].copy_from_slice(&self.data_crc32.to_le_bytes());
        slot[20..24].copy_from_slice(&self.stored_bytes.to_le_bytes());
        slot[24..24 + MAX_NAME_LEN].copy_from_slice(&self.name);
    }
}

pub struct KnightFs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
    scratch: [u8; BLOCK_SIZE],
}

impl<D: BlockDevice> KnightFs<D> {
    fn check_geometry(dev: &D) -> Result<(usize, u64), FsError> {
        let ssz = dev.sector_size();
        if ssz == 0 || !BLOCK_SIZE.is_multiple_of(ssz) {
            return Err(FsError::InvalidGeometry);
        }
        let sec_per_blk = (BLOCK_SIZE / ssz) as u64;
        let total_blocks = (dev.sector_count() / sec_per_blk).min((BLOCK_SIZE * 8) as u64);
        if total_blocks <= DATA_START_BLOCK + 2 {
            return Err(FsError::InvalidGeometry);
        }
        Ok((sec_per_blk as usize, total_blocks))
    }

    fn read_block_into(dev: &mut D, blk: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), FsError> {
        let spb = (BLOCK_SIZE / dev.sector_size()) as u64;
        dev.read_sectors(blk * spb, buf)
    }

    fn write_block_from(dev: &mut D, blk: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), FsError> {
        let spb = (BLOCK_SIZE / dev.sector_size()) as u64;
        dev.write_sectors(blk * spb, buf)
    }

    /// Formats `dev` with dual A/B KnightFS v2 superblocks.
    pub fn format(mut dev: D) -> Result<Self, FsError> {
        let (_, total_blocks) = Self::check_geometry(&dev)?;
        let sb_a = Superblock {
            generation: 1,
            active_slot: 0,
            total_blocks,
            bitmap_start: BITMAP_BLOCK,
            bitmap_blocks: 1,
            inode_start: INODE_START_BLOCK,
            inode_blocks: INODE_TABLE_BLOCKS,
            data_start: DATA_START_BLOCK,
        };
        let mut scratch = [0u8; BLOCK_SIZE];
        sb_a.encode(&mut scratch);
        Self::write_block_from(&mut dev, SUPER_BLOCK_A, &scratch)?;
        let sb_b = Superblock {
            generation: 2,
            active_slot: 1,
            ..sb_a
        };
        sb_b.encode(&mut scratch);
        Self::write_block_from(&mut dev, SUPER_BLOCK_B, &scratch)?;

        scratch.fill(0);
        for b in 0..DATA_START_BLOCK as usize {
            scratch[b / 8] |= 1 << (b % 8);
        }
        Self::write_block_from(&mut dev, BITMAP_BLOCK, &scratch)?;

        scratch.fill(0);
        for ib in 0..INODE_TABLE_BLOCKS {
            Self::write_block_from(&mut dev, INODE_START_BLOCK + ib, &scratch)?;
        }

        Ok(Self {
            dev,
            sb: sb_b,
            scratch,
        })
    }

    /// Mounts by evaluating A/B superblocks and selecting the highest valid generation.
    pub fn mount(mut dev: D) -> Result<Self, FsError> {
        Self::check_geometry(&dev)?;
        let mut scratch = [0u8; BLOCK_SIZE];
        Self::read_block_into(&mut dev, SUPER_BLOCK_A, &mut scratch)?;
        let a = Superblock::decode(&scratch).ok();
        Self::read_block_into(&mut dev, SUPER_BLOCK_B, &mut scratch)?;
        let b = Superblock::decode(&scratch).ok();

        let sb = match (a, b) {
            (Some(sa), Some(sb)) => {
                if sb.generation >= sa.generation { sb } else { sa }
            }
            (Some(sa), None) => sa,
            (None, Some(sb)) => sb,
            (None, None) => return Err(FsError::InvalidSuperblock),
        };
        Ok(Self { dev, sb, scratch })
    }

    pub fn mount_or_format(mut dev: D) -> Result<(Self, bool), FsError> {
        Self::check_geometry(&dev)?;
        let mut scratch = [0u8; BLOCK_SIZE];
        Self::read_block_into(&mut dev, SUPER_BLOCK_A, &mut scratch)?;
        let a = Superblock::decode(&scratch).ok();
        Self::read_block_into(&mut dev, SUPER_BLOCK_B, &mut scratch)?;
        let b = Superblock::decode(&scratch).ok();
        if a.is_some() || b.is_some() {
            let fs = Self::mount(dev)?;
            return Ok((fs, true));
        }
        Self::format(dev).map(|fs| (fs, false))
    }

    fn bump_ab_superblock(&mut self) -> Result<(), FsError> {
        self.sb.generation = self.sb.generation.wrapping_add(1);
        self.sb.active_slot ^= 1;
        let target = if self.sb.active_slot == 0 {
            SUPER_BLOCK_A
        } else {
            SUPER_BLOCK_B
        };
        self.sb.encode(&mut self.scratch);
        Self::write_block_from(&mut self.dev, target, &self.scratch)
    }

    pub fn generation(&self) -> u64 {
        self.sb.generation
    }

    pub fn into_inner(self) -> D {
        self.dev
    }

    pub fn total_blocks(&self) -> u64 {
        self.sb.total_blocks
    }

    fn validate_name(name: &str) -> Result<&[u8], FsError> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_NAME_LEN || bytes.contains(&b'/') {
            return Err(FsError::InvalidName);
        }
        Ok(bytes)
    }

    fn find_inode(&mut self, name: &[u8]) -> Result<Option<(usize, Inode)>, FsError> {
        let entries_per_blk = BLOCK_SIZE / INODE_SIZE;
        let mut first_free: Option<usize> = None;
        for blk_idx in 0..self.sb.inode_blocks as usize {
            Self::read_block_into(
                &mut self.dev,
                self.sb.inode_start + blk_idx as u64,
                &mut self.scratch,
            )?;
            for slot in 0..entries_per_blk {
                let off = slot * INODE_SIZE;
                let inode = Inode::decode(&self.scratch[off..off + INODE_SIZE]);
                let global_idx = blk_idx * entries_per_blk + slot;
                if inode.used {
                    if inode.name_bytes() == name {
                        return Ok(Some((global_idx, inode)));
                    }
                } else if first_free.is_none() {
                    first_free = Some(global_idx);
                }
            }
        }
        Ok(first_free.map(|idx| (idx, Inode::empty())))
    }

    fn write_inode(&mut self, global_idx: usize, inode: &Inode) -> Result<(), FsError> {
        let entries_per_blk = BLOCK_SIZE / INODE_SIZE;
        let blk_idx = global_idx / entries_per_blk;
        let slot = global_idx % entries_per_blk;
        let blk = self.sb.inode_start + blk_idx as u64;
        Self::read_block_into(&mut self.dev, blk, &mut self.scratch)?;
        let off = slot * INODE_SIZE;
        inode.encode(&mut self.scratch[off..off + INODE_SIZE]);
        Self::write_block_from(&mut self.dev, blk, &self.scratch)
    }

    fn set_bitmap_range(bitmap: &mut [u8; BLOCK_SIZE], start: u32, count: u32, used: bool) {
        for b in start..start.saturating_add(count) {
            let idx = (b as usize) / 8;
            let bit = 1u8 << ((b as usize) % 8);
            if idx < bitmap.len() {
                if used {
                    bitmap[idx] |= bit;
                } else {
                    bitmap[idx] &= !bit;
                }
            }
        }
    }

    fn alloc_contiguous(&self, bitmap: &[u8; BLOCK_SIZE], count: u32) -> Option<u32> {
        let max_blk = self.sb.total_blocks.min((BLOCK_SIZE * 8) as u64) as u32;
        let mut run_start = self.sb.data_start as u32;
        let mut run_len = 0u32;
        for b in (self.sb.data_start as u32)..max_blk {
            let byte = bitmap[(b as usize) / 8];
            let free = (byte & (1u8 << ((b as usize) % 8))) == 0;
            if free {
                if run_len == 0 {
                    run_start = b;
                }
                run_len += 1;
                if run_len == count {
                    return Some(run_start);
                }
            } else {
                run_len = 0;
            }
        }
        None
    }

    /// Writes/replaces a file with transparent compression (for single-block inputs when smaller)
    /// and commits via alternating A/B superblock generation.
    pub fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), FsError> {
        let name_bytes = Self::validate_name(name)?;
        if data.len() > u32::MAX as usize {
            return Err(FsError::NoSpace);
        }
        let Some((slot_idx, existing)) = self.find_inode(name_bytes)? else {
            return Err(FsError::NoSpace);
        };

        let mut comp_buf = [0u8; BLOCK_SIZE];
        let mut compressed_len = None;
        if !data.is_empty() && data.len() <= BLOCK_SIZE {
            if let Some(clen) = compress_rle(data, &mut comp_buf)
                && clen < data.len()
            {
                compressed_len = Some(clen);
            }
        }

        let (payload, flags, stored_bytes) = if let Some(clen) = compressed_len {
            (&comp_buf[..clen], INODE_FLAG_COMPRESSED, clen as u32)
        } else {
            (data, 0u16, data.len() as u32)
        };

        let needed_blocks = ((payload.len() + BLOCK_SIZE - 1) / BLOCK_SIZE).max(1) as u32;

        let mut bitmap = [0u8; BLOCK_SIZE];
        Self::read_block_into(&mut self.dev, self.sb.bitmap_start, &mut bitmap)?;
        if existing.used && existing.block_count > 0 {
            Self::set_bitmap_range(&mut bitmap, existing.start_block, existing.block_count, false);
        }
        let start_block = self
            .alloc_contiguous(&bitmap, needed_blocks)
            .ok_or(FsError::NoSpace)?;
        Self::set_bitmap_range(&mut bitmap, start_block, needed_blocks, true);

        for blk_off in 0..needed_blocks {
            self.scratch.fill(0);
            let byte_off = (blk_off as usize) * BLOCK_SIZE;
            if byte_off < payload.len() {
                let copy_len = (payload.len() - byte_off).min(BLOCK_SIZE);
                self.scratch[..copy_len].copy_from_slice(&payload[byte_off..byte_off + copy_len]);
            }
            Self::write_block_from(&mut self.dev, start_block as u64 + blk_off as u64, &self.scratch)?;
        }

        Self::write_block_from(&mut self.dev, self.sb.bitmap_start, &bitmap)?;

        let mut name_arr = [0u8; MAX_NAME_LEN];
        name_arr[..name_bytes.len()].copy_from_slice(name_bytes);
        let inode = Inode {
            used: true,
            name_len: name_bytes.len() as u8,
            flags,
            size_bytes: data.len() as u32,
            stored_bytes,
            start_block,
            block_count: needed_blocks,
            data_crc32: crc32(data),
            name: name_arr,
        };
        self.write_inode(slot_idx, &inode)?;
        self.bump_ab_superblock()
    }

    pub fn stat(&mut self, name: &str) -> Result<FileStat, FsError> {
        let name_bytes = Self::validate_name(name)?;
        match self.find_inode(name_bytes)? {
            Some((_, inode)) if inode.used => Ok(FileStat {
                size_bytes: inode.size_bytes,
                stored_bytes: inode.stored_bytes,
                start_block: inode.start_block,
                block_count: inode.block_count,
                data_crc32: inode.data_crc32,
                compressed: (inode.flags & INODE_FLAG_COMPRESSED) != 0,
            }),
            _ => Err(FsError::NotFound),
        }
    }

    pub fn read_file(&mut self, name: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let name_bytes = Self::validate_name(name)?;
        let Some((_, inode)) = self.find_inode(name_bytes)? else {
            return Err(FsError::NotFound);
        };
        if !inode.used {
            return Err(FsError::NotFound);
        }
        let len = inode.size_bytes as usize;
        if out.len() < len {
            return Err(FsError::BufferTooSmall);
        }
        if (inode.flags & INODE_FLAG_COMPRESSED) != 0 {
            let clen = inode.stored_bytes as usize;
            if clen > BLOCK_SIZE {
                return Err(FsError::CorruptCompressedData);
            }
            let mut comp_tmp = [0u8; BLOCK_SIZE];
            Self::read_block_into(&mut self.dev, inode.start_block as u64, &mut comp_tmp)?;
            let dec_len = decompress_rle(&comp_tmp[..clen], &mut out[..len])
                .ok_or(FsError::CorruptCompressedData)?;
            if dec_len != len {
                return Err(FsError::CorruptCompressedData);
            }
        } else {
            let mut remaining = len;
            let mut dst_off = 0usize;
            for blk_off in 0..inode.block_count {
                Self::read_block_into(
                    &mut self.dev,
                    inode.start_block as u64 + blk_off as u64,
                    &mut self.scratch,
                )?;
                let take = remaining.min(BLOCK_SIZE);
                out[dst_off..dst_off + take].copy_from_slice(&self.scratch[..take]);
                dst_off += take;
                remaining -= take;
            }
        }
        if crc32(&out[..len]) != inode.data_crc32 {
            return Err(FsError::ChecksumMismatch);
        }
        Ok(len)
    }

    pub fn delete_file(&mut self, name: &str) -> Result<(), FsError> {
        let name_bytes = Self::validate_name(name)?;
        let Some((slot_idx, inode)) = self.find_inode(name_bytes)? else {
            return Err(FsError::NotFound);
        };
        if !inode.used {
            return Err(FsError::NotFound);
        }
        let mut bitmap = [0u8; BLOCK_SIZE];
        Self::read_block_into(&mut self.dev, self.sb.bitmap_start, &mut bitmap)?;
        Self::set_bitmap_range(&mut bitmap, inode.start_block, inode.block_count, false);
        Self::write_block_from(&mut self.dev, self.sb.bitmap_start, &bitmap)?;
        self.write_inode(slot_idx, &Inode::empty())?;
        self.bump_ab_superblock()
    }

    pub fn list_files<F: FnMut(&str, u32)>(&mut self, mut cb: F) -> Result<usize, FsError> {
        let entries_per_blk = BLOCK_SIZE / INODE_SIZE;
        let mut count = 0usize;
        for blk_idx in 0..self.sb.inode_blocks as usize {
            Self::read_block_into(
                &mut self.dev,
                self.sb.inode_start + blk_idx as u64,
                &mut self.scratch,
            )?;
            for slot in 0..entries_per_blk {
                let off = slot * INODE_SIZE;
                let inode = Inode::decode(&self.scratch[off..off + INODE_SIZE]);
                if inode.used
                    && let Ok(name) = core::str::from_utf8(inode.name_bytes())
                {
                    cb(name, inode.size_bytes);
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

/// Fast `#![no_std]` RLE compressor:
/// - `0x00..=0x7F`: `(tag + 3)` repetitions of following byte (`3..=130`)
/// - `0x80..=0xFF`: `(tag & 0x7F) + 1` literal bytes (`1..=128`)
pub fn compress_rle(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut si = 0usize;
    let mut di = 0usize;
    while si < src.len() {
        let b = src[si];
        let mut run = 1usize;
        while si + run < src.len() && run < 130 && src[si + run] == b {
            run += 1;
        }
        if run >= 3 {
            if di + 2 > dst.len() {
                return None;
            }
            dst[di] = (run - 3) as u8;
            dst[di + 1] = b;
            di += 2;
            si += run;
        } else {
            let lit_start = si;
            let mut lit_len = 0usize;
            while si < src.len() && lit_len < 128 {
                let cur = src[si];
                let mut r = 1usize;
                while si + r < src.len() && r < 3 && src[si + r] == cur {
                    r += 1;
                }
                if r >= 3 {
                    break;
                }
                si += 1;
                lit_len += 1;
            }
            if di + 1 + lit_len > dst.len() {
                return None;
            }
            dst[di] = 0x80 | ((lit_len - 1) as u8);
            dst[di + 1..di + 1 + lit_len].copy_from_slice(&src[lit_start..lit_start + lit_len]);
            di += 1 + lit_len;
        }
    }
    Some(di)
}

pub fn decompress_rle(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut si = 0usize;
    let mut di = 0usize;
    while si < src.len() {
        let tag = src[si];
        si += 1;
        if (tag & 0x80) == 0 {
            let count = (tag as usize) + 3;
            let val = *src.get(si)?;
            si += 1;
            if di + count > dst.len() {
                return None;
            }
            dst[di..di + count].fill(val);
            di += count;
        } else {
            let count = ((tag & 0x7F) as usize) + 1;
            if si + count > src.len() || di + count > dst.len() {
                return None;
            }
            dst[di..di + count].copy_from_slice(&src[si..si + count]);
            si += count;
            di += count;
        }
    }
    Some(di)
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemDisk {
        data: Vec<u8>,
    }

    impl MemDisk {
        fn new(sectors: usize) -> Self {
            Self {
                data: vec![0; sectors * 512],
            }
        }
    }

    impl BlockDevice for MemDisk {
        fn sector_size(&self) -> usize {
            512
        }
        fn sector_count(&self) -> u64 {
            (self.data.len() / 512) as u64
        }
        fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
            let off = lba as usize * 512;
            let end = off.checked_add(buf.len()).ok_or(FsError::IoError)?;
            if end > self.data.len() {
                return Err(FsError::IoError);
            }
            buf.copy_from_slice(&self.data[off..end]);
            Ok(())
        }
        fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
            let off = lba as usize * 512;
            let end = off.checked_add(buf.len()).ok_or(FsError::IoError)?;
            if end > self.data.len() {
                return Err(FsError::IoError);
            }
            self.data[off..end].copy_from_slice(buf);
            Ok(())
        }
    }

    #[test]
    fn format_write_read_overwrite_delete_and_remount() {
        let disk = MemDisk::new(512);
        let mut fs = KnightFs::format(disk).unwrap();

        let payload = b"KnightOS phase 4 persistent payload!";
        fs.write_file("hello.txt", payload).unwrap();

        let big: Vec<u8> = (0..5000u32).map(|i| (i & 0xFF) as u8).collect();
        fs.write_file("big.bin", &big).unwrap();

        let mut out = vec![0u8; 64];
        let n = fs.read_file("hello.txt", &mut out).unwrap();
        assert_eq!(&out[..n], payload);

        let mut big_out = vec![0u8; 6000];
        let bn = fs.read_file("big.bin", &mut big_out).unwrap();
        assert_eq!(&big_out[..bn], big.as_slice());

        fs.write_file("hello.txt", b"v2").unwrap();
        let n2 = fs.read_file("hello.txt", &mut out).unwrap();
        assert_eq!(&out[..n2], b"v2");

        let mut names = Vec::new();
        let count = fs.list_files(|name, _| names.push(name.to_string())).unwrap();
        assert_eq!(count, 2);

        let raw_disk = fs.into_inner();
        let mut remounted = KnightFs::mount(raw_disk).unwrap();
        let n3 = remounted.read_file("hello.txt", &mut out).unwrap();
        assert_eq!(&out[..n3], b"v2");

        remounted.delete_file("hello.txt").unwrap();
        assert_eq!(remounted.stat("hello.txt"), Err(FsError::NotFound));
    }

    #[test]
    fn detects_bit_flip_corruption() {
        let disk = MemDisk::new(256);
        let mut fs = KnightFs::format(disk).unwrap();
        fs.write_file("config.toml", b"mode=release").unwrap();
        let stat = fs.stat("config.toml").unwrap();
        let mut raw_disk = fs.into_inner();
        raw_disk.data[stat.start_block as usize * BLOCK_SIZE] ^= 0x5A;

        let mut remounted = KnightFs::mount(raw_disk).unwrap();
        let mut out = [0u8; 64];
        assert_eq!(
            remounted.read_file("config.toml", &mut out),
            Err(FsError::ChecksumMismatch)
        );
    }

    #[test]
    fn transparent_compression_and_ab_superblock_failover() {
        let disk = MemDisk::new(256);
        let mut fs = KnightFs::format(disk).unwrap();
        let compressible = [b'K'; 512];
        fs.write_file("banner.bin", &compressible).unwrap();
        let st = fs.stat("banner.bin").unwrap();
        assert!(st.compressed);
        assert!(st.stored_bytes < 512);
        let expected_gen = fs.generation();

        // Corrupt whichever superblock slot was written latest -> must fail over to prior valid slot!
        let mut raw_disk = fs.into_inner();
        let latest_blk = if expected_gen % 2 == 1 {
            SUPER_BLOCK_A
        } else {
            SUPER_BLOCK_B
        };
        raw_disk.data[latest_blk as usize * BLOCK_SIZE] ^= 0xFF;

        let mut remounted = KnightFs::mount(raw_disk).unwrap();
        assert_eq!(remounted.generation(), expected_gen - 1);
        let mut out = [0u8; 512];
        let n = remounted.read_file("banner.bin", &mut out).unwrap();
        assert_eq!(n, 512);
        assert_eq!(out, compressible);
    }
}
